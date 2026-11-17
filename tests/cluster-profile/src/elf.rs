//! ELF end computation. A programdata account is zero-padded to its allocated size, and a valid
//! ELF can itself end in zero bytes, so the end is derived from the headers, never by trimming.

fn u16le(d: &[u8], o: usize) -> Option<u64> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?) as u64)
}
fn u32le(d: &[u8], o: usize) -> Option<u64> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?) as u64)
}
fn u64le(d: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(o..o + 8)?.try_into().ok()?))
}

/// End offset (exclusive) of an ELF64 little-endian file, or None if `d` is not a well-formed
/// ELF64 whose extents fit inside `d`.
pub fn elf_end(d: &[u8]) -> Option<usize> {
    if d.len() < 64 || &d[0..4] != b"\x7fELF" || d[4] != 2 || d[5] != 1 {
        return None;
    }
    let phoff = u64le(d, 0x20)?;
    let shoff = u64le(d, 0x28)?;
    let phentsize = u16le(d, 0x36)?;
    let phnum = u16le(d, 0x38)?;
    let shentsize = u16le(d, 0x3a)?;
    let shnum = u16le(d, 0x3c)?;
    let mut end = 64u64;
    if phnum > 0 {
        end = end.max(phoff.checked_add(phnum.checked_mul(phentsize)?)?);
        for i in 0..phnum {
            let b = (phoff + i * phentsize) as usize;
            let off = u64le(d, b + 8)?;
            let filesz = u64le(d, b + 32)?;
            end = end.max(off.checked_add(filesz)?);
        }
    }
    if shnum > 0 {
        end = end.max(shoff.checked_add(shnum.checked_mul(shentsize)?)?);
        for i in 0..shnum {
            let b = (shoff + i * shentsize) as usize;
            let ty = u32le(d, b + 4)?;
            if ty == 8 {
                continue; // SHT_NOBITS occupies no file space
            }
            let off = u64le(d, b + 0x18)?;
            let size = u64le(d, b + 0x20)?;
            end = end.max(off.checked_add(size)?);
        }
    }
    if end as usize as u64 != end || end as usize > d.len() {
        return None;
    }
    Some(end as usize)
}

/// Returns the program bytes: the cut ELF, or the full input (flag false) if it is not an ELF.
pub fn cut(d: &[u8]) -> (&[u8], bool) {
    match elf_end(d) {
        Some(e) => (&d[..e], true),
        None => (d, false),
    }
}

#[cfg(test)]
pub fn build_test_elf(tail_zeros: usize) -> Vec<u8> {
    // header(64) + one phdr(56) + .text(16 bytes, ends in zeros) + .bss(NOBITS) + shdrs(3*64)
    let mut f = vec![0u8; 64];
    f[0..4].copy_from_slice(b"\x7fELF");
    f[4] = 2;
    f[5] = 1;
    let phoff = 64u64;
    let text_off = 64 + 56;
    let shoff = (text_off + 16) as u64;
    f[0x20..0x28].copy_from_slice(&phoff.to_le_bytes());
    f[0x28..0x30].copy_from_slice(&shoff.to_le_bytes());
    f[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
    f[0x38..0x3a].copy_from_slice(&1u16.to_le_bytes());
    f[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes());
    f[0x3c..0x3e].copy_from_slice(&3u16.to_le_bytes());
    let mut ph = vec![0u8; 56];
    ph[8..16].copy_from_slice(&(text_off as u64).to_le_bytes());
    ph[32..40].copy_from_slice(&16u64.to_le_bytes());
    f.extend(ph);
    let mut text = vec![0xabu8; 16];
    for b in text[12..].iter_mut() {
        *b = 0; // ELF legitimately ends with zero bytes before the section table
    }
    f.extend(text);
    let mut sh = vec![0u8; 64]; // null section
    let mut s1 = vec![0u8; 64];
    s1[4..8].copy_from_slice(&1u32.to_le_bytes());
    s1[0x18..0x20].copy_from_slice(&(text_off as u64).to_le_bytes());
    s1[0x20..0x28].copy_from_slice(&16u64.to_le_bytes());
    let mut s2 = vec![0u8; 64];
    s2[4..8].copy_from_slice(&8u32.to_le_bytes());
    s2[0x18..0x20].copy_from_slice(&9999u64.to_le_bytes()); // NOBITS: must be ignored
    s2[0x20..0x28].copy_from_slice(&100000u64.to_le_bytes());
    sh.extend(s1);
    sh.extend(s2);
    f.extend(sh);
    f.extend(vec![0u8; tail_zeros]);
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn cut_ignores_padding_and_keeps_inner_zeros() {
        let bare = build_test_elf(0);
        let padded = build_test_elf(5000);
        assert_eq!(elf_end(&bare), Some(bare.len()));
        assert_eq!(elf_end(&padded), Some(bare.len()));
        assert_eq!(Sha256::digest(cut(&padded).0), Sha256::digest(&bare));
    }

    #[test]
    fn trailing_zero_bytes_in_elf_are_kept() {
        // last section ends in zeros: build an ELF whose final section is zero data
        let mut e = build_test_elf(0);
        // pretend the section table is at the end and the last file byte is 0 (it is: null shdr/NOBITS)
        assert_eq!(*e.last().unwrap(), 0);
        let n = e.len();
        e.extend(vec![0u8; 100]);
        assert_eq!(elf_end(&e), Some(n));
    }

    #[test]
    fn non_elf_falls_back() {
        let d = vec![1u8, 2, 3, 0, 0];
        let (c, is) = cut(&d);
        assert!(!is);
        assert_eq!(c, &d[..]);
    }

    #[test]
    fn truncated_elf_is_rejected() {
        let e = build_test_elf(0);
        assert_eq!(elf_end(&e[..e.len() - 10]), None);
    }
}
