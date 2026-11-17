#!/usr/bin/env python3
"""Turn `cargo test` (libtest, pretty format, captured output) into one JSON line per test.

    parse_libtest.py <test.log>  ->  {"binary", "test", "status": ok|FAILED|ignored, "error"}

libtest's own result lines are the source of truth: every test is counted, whether or not it
writes to HARNESS_REPORT. `error` is the panic message of a failed test (first lines after
"panicked at"), or the first line of its captured output.
"""
import json
import re
import sys

RUNNING = re.compile(r"^\s+Running (?:unittests )?(\S+)")
RESULT = re.compile(r"^test (\S+) \.\.\. (ok|FAILED|ignored)\b")
CAPTURE = re.compile(r"^---- (\S+) (?:stdout|stderr) ----$")


def main(path):
    binary = "?"
    results = []
    errors = {}
    current = None
    capture = []
    with open(path, errors="replace") as f:
        lines = f.read().splitlines()
    for line in lines:
        m = RUNNING.match(line)
        if m:
            binary = m.group(1)
            continue
        m = RESULT.match(line)
        if m:
            results.append({"binary": binary, "test": m.group(1), "status": m.group(2)})
            continue
        m = CAPTURE.match(line)
        if m:
            if current:
                errors.setdefault(current, capture)
            current, capture = m.group(1), []
            continue
        if current is not None:
            if line.startswith("failures:") or line.startswith("test result:"):
                errors.setdefault(current, capture)
                current, capture = None, []
            else:
                capture.append(line)
    if current:
        errors.setdefault(current, capture)

    def message(captured):
        for i, l in enumerate(captured):
            if "panicked at" in l:
                rest = [x for x in captured[i + 1 : i + 4] if x.strip()]
                return " ".join([l.strip()] + [x.strip() for x in rest])[:600]
        nonempty = [l for l in captured if l.strip()]
        return nonempty[0][:600] if nonempty else ""

    for r in results:
        r["error"] = message(errors.get(r["test"], [])) if r["status"] == "FAILED" else ""
        print(json.dumps(r))


if __name__ == "__main__":
    main(sys.argv[1])
