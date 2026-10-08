#!/usr/bin/env python3
"""Prove a symbols .so can actually map PCs to source, before it is bundled.

"Carries DWARF" does NOT predict whether coverage renders. A symbols artifact can
have a complete, entirely valid-looking DWARF -- hundreds of compile units, correct
relative source paths, a full file table, non-zero addresses -- and still map
NOTHING, because every address in its line program points outside the code.

That is not hypothetical. platform-tools v1.51 and v1.44 both build a working
marinade program with good-looking debug info, but v1.44 writes each
DW_LNE_set_address as 4 zero bytes followed by a 4-byte address inside an 8-byte
field -- shifted left 32 bits. Measured on the two artifacts:

    v1.44   706 non-zero addresses,   0/706 inside .text   (>>32 puts 706/706 back)
    v1.51   716 non-zero addresses, 716/716 inside .text

With the v1.44 artifact the worker reports:

    [COVERAGE] DWARF source map loaded: 0 PCs resolved, 0 functions
    [LCOV] Source-level coverage: 0 source files

and the cover task then fails with
    SourcesOriginalPath "..." does not match any source file in the coverage profile
because the profile has no source files at all. The message names the prefix, so it
reads as a path bug -- and every prefix "fails" identically. If a second prefix fails
the same way, the prefix is not the problem; run this instead.

Needs no llvm: it parses the ELF section table and the DWARF line program directly.
A check that silently degrades when a tool is missing is how a dead artifact ships.

usage: verify-dwarf-addrs.py <symbols.so>
exit 0 = addresses map into .text; exit 1 = coverage would render empty.
"""
import struct
import sys


def sections(d):
    shoff = struct.unpack_from('<Q', d, 0x28)[0]
    shent = struct.unpack_from('<H', d, 0x3a)[0]
    shnum = struct.unpack_from('<H', d, 0x3c)[0]
    shstr = struct.unpack_from('<H', d, 0x3e)[0]
    stroff = struct.unpack_from('<Q', d, shoff + shstr * shent + 0x18)[0]
    out = {}
    for i in range(shnum):
        o = shoff + i * shent
        n = struct.unpack_from('<I', d, o)[0]
        off = struct.unpack_from('<Q', d, o + 0x18)[0]
        sz = struct.unpack_from('<Q', d, o + 0x20)[0]
        name = d[stroff + n:d.index(b'\0', stroff + n)].decode('utf-8', 'replace')
        out[name] = (off, sz)
    return out


def uleb(b, i):
    r = sh = 0
    while True:
        x = b[i]
        i += 1
        r |= (x & 0x7f) << sh
        sh += 7
        if not x & 0x80:
            return r, i


def cstr(b, i):
    j = b.index(b'\0', i)
    return b[i:j], j + 1


def set_addresses(line):
    """Every DW_LNE_set_address operand in .debug_line, via a real opcode walk.

    A naive byte scan for the 00 09 02 prefix finds false positives in the middle
    of other operands, so walk the header (to learn opcode_base and the standard
    operand counts) and then step the program opcode by opcode.
    """
    addrs = []
    pos = 0
    while pos < len(line) - 10:
        unit_len = struct.unpack_from('<I', line, pos)[0]
        if unit_len == 0 or unit_len == 0xffffffff:
            break
        end = pos + 4 + unit_len
        if end > len(line):
            break
        p = pos + 4
        ver = struct.unpack_from('<H', line, p)[0]
        p += 2
        if ver not in (2, 3, 4):     # v5 headers are laid out differently
            break
        header_len = struct.unpack_from('<I', line, p)[0]
        p += 4
        prog = p + header_len
        p += 1                       # minimum_instruction_length
        if ver >= 4:
            p += 1                   # maximum_operations_per_instruction
        p += 3                       # default_is_stmt, line_base, line_range
        opcode_base = line[p]
        p += 1
        std_lens = [line[p + i] for i in range(opcode_base - 1)]
        p += opcode_base - 1
        while True:                  # include_directories
            s, p = cstr(line, p)
            if not s:
                break
        while True:                  # file_names
            nm, p = cstr(line, p)
            if not nm:
                break
            _, p = uleb(line, p)
            _, p = uleb(line, p)
            _, p = uleb(line, p)
        q = prog
        while q < end:
            op = line[q]
            q += 1
            if op == 0:                              # extended opcode
                ln, q = uleb(line, q)
                if ln == 0:
                    continue
                if line[q] == 2:                     # DW_LNE_set_address
                    if ln == 9:
                        addrs.append(struct.unpack_from('<Q', line, q + 1)[0])
                    elif ln == 5:
                        addrs.append(struct.unpack_from('<I', line, q + 1)[0])
                q += ln
            elif op < opcode_base:                   # standard opcode
                if op == 9:                          # fixed_advance_pc: uhalf
                    q += 2
                else:
                    for _ in range(std_lens[op - 1]):
                        _, q = uleb(line, q)
            # special opcodes carry no operands
        pos = end
    return addrs


def main():
    if len(sys.argv) != 2:
        print(__doc__.strip().splitlines()[-3], file=sys.stderr)
        return 2
    path = sys.argv[1]
    data = open(path, 'rb').read()
    if data[:4] != b'\x7fELF':
        print(f"verify-dwarf-addrs: {path} is not an ELF", file=sys.stderr)
        return 1
    sec = sections(data)
    if '.debug_line' not in sec or sec['.debug_line'][1] == 0:
        print(f"verify-dwarf-addrs: ERROR {path} has no .debug_line -- rebuild with "
              f"CARGO_PROFILE_RELEASE_DEBUG=2 and STRIP=none", file=sys.stderr)
        return 1
    if '.text' not in sec:
        print(f"verify-dwarf-addrs: ERROR {path} has no .text", file=sys.stderr)
        return 1

    text_size = sec['.text'][1]
    off, size = sec['.debug_line']
    addrs = set_addresses(data[off:off + size])
    nonzero = [a for a in addrs if a]
    in_text = [a for a in nonzero if a < text_size]

    if not nonzero:
        print(f"verify-dwarf-addrs: ERROR no non-zero line addresses in {path}.",
              file=sys.stderr)
        return 1

    pct = 100.0 * len(in_text) / len(nonzero)
    if pct < 50.0:
        shifted = [a >> 32 for a in nonzero]
        shifted_ok = sum(1 for a in shifted if 0 < a < text_size)
        print(f"verify-dwarf-addrs: ERROR coverage would render EMPTY.", file=sys.stderr)
        print(f"  {len(in_text)}/{len(nonzero)} line addresses fall inside .text "
              f"(0..0x{text_size:x}); range 0x{min(nonzero):x}..0x{max(nonzero):x}",
              file=sys.stderr)
        if shifted_ok > len(nonzero) // 2:
            print(f"  {shifted_ok}/{len(nonzero)} land in .text when shifted right 32 "
                  f"bits: the toolchain wrote 4 pad bytes + a 4-byte address into an "
                  f"8-byte field. Build with a newer platform-tools "
                  f"(--tools-version v1.51 is known good).", file=sys.stderr)
        print(f"  The worker would report '0 PCs resolved' and the cover task would "
              f"fail naming SourcesOriginalPath -- do NOT chase the prefix.",
              file=sys.stderr)
        return 1

    print(f"verify-dwarf-addrs: OK {len(in_text)}/{len(nonzero)} line addresses inside "
          f".text (0x{min(in_text):x}..0x{max(in_text):x})")
    return 0


if __name__ == '__main__':
    sys.exit(main())
