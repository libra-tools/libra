#!/usr/bin/env python3
"""nextest_group_overlap_check.py — assert a nextest test-group really ran
with max-threads = 1 (plan-20260827 NP-02).

Reads a nextest junit.xml, projects the testcases belonging to the group's
member set onto (start, end) wall-clock intervals, and fails if any two
intervals overlap. Membership is derived the same way the generator derives
it (tests/SERIAL_REGISTRY.tsv key membership): fn rows whose lane keys
include an external key match by last name segment; pure-global site rows
match every test of the host target (junit classname).

usage: python3 tests/nextest_group_overlap_check.py <junit.xml> external
exit codes: 0 ok; 1 overlap found; 2 usage/parse error.
"""

import math
import sys
import xml.etree.ElementTree as ET
from datetime import datetime
from pathlib import Path

# External = any named key outside the in-process closed set; a newly named
# key counts as external by default (same rule as the generator and guard).
INTERNAL_KEYS = {"cwd", "env", "hash_kind"}

# Interval-endpoint tolerance. junit timestamps are millisecond-truncated
# (<=1 ms error per endpoint) while durations are float-precise, so a perfect
# serial handoff can reconstruct with < 2 ms of phantom overlap — and no more.
# 2 ms therefore rejects any real concurrency (even 5-10 ms member tests
# overlap by >= their duration minus scheduling skew) while absorbing the
# encoding jitter. Verified by --selftest below.
EPS = 0.002


def _overlap_count(intervals):
    intervals = sorted(intervals)
    return sum(
        1
        for (s1, e1, _), (s2, e2, _) in zip(intervals, intervals[1:])
        if s2 < e1 - EPS
    )


def _synthetic_junit(cases):
    rows = "".join(
        f'<testcase classname="libra::agent_bridge_vcs_test" '
        f'name="case{i}::commit_create_refuses_on_head_drift" '
        f'timestamp="{ts}" time="{dur}"/>'
        for i, (ts, dur) in enumerate(cases)
    )
    return f"<testsuites><testsuite>{rows}</testsuite></testsuites>"


def selftest() -> int:
    import tempfile

    perfect_handoff = [(0.000, 0.450, "a"), (0.450, 0.985, "b")]
    ms_jitter = [(0.000, 0.4512, "a"), (0.4508, 0.985, "b")]      # < 2 ms phantom
    short_full_overlap = [(0.000, 0.007, "a"), (0.001, 0.008, "b")]  # 7 ms tests
    long_partial_overlap = [(0.000, 0.500, "a"), (0.300, 0.900, "b")]
    interval_ok = (
        _overlap_count(perfect_handoff) == 0
        and _overlap_count(ms_jitter) == 0
        and _overlap_count(short_full_overlap) == 1
        and _overlap_count(long_partial_overlap) == 1
    )

    # feed genuinely malformed durations through the real XML parse path and
    # require the parse-rejection exit code (2) — a regression that drops the
    # finite-value check must turn this red.
    base = "2026-08-28T20:47:42.497+08:00"
    parse_ok = True
    import os

    def _check_synthetic(cases, expect_rc, label):
        path = None
        try:
            with tempfile.NamedTemporaryFile(
                "w", suffix=".xml", delete=False
            ) as f:
                f.write(_synthetic_junit(cases))
                path = f.name
            rc = check_file(path)
            if rc != expect_rc:
                print(f"SELFTEST {label} failed: rc={rc} (want {expect_rc})")
                return False
            return True
        finally:
            if path is not None:
                os.unlink(path)

    for bad in ("NaN", "inf", "-1.0"):
        parse_ok &= _check_synthetic(
            [(base, bad)], 2, f"parse-rejection time={bad}"
        )
    # a well-formed overlapping pair through the same real path must be
    # detected as a genuine overlap (exit 1)
    parse_ok &= _check_synthetic(
        [(base, "0.500"), (base, "0.500")], 1, "same-start overlap"
    )
    parse_ok = bool(parse_ok)

    ok = interval_ok and parse_ok
    print("SELFTEST", "OK" if ok else "FAIL")
    return 0 if ok else 1


def registry_membership(root: Path):
    fns, targets = set(), set()
    lines = (root / "tests/SERIAL_REGISTRY.tsv").read_text(encoding="utf-8").splitlines()
    if not lines or lines[0] != "test_fn\tlane\treason":
        print("FAIL: unexpected registry header", file=sys.stderr)
        sys.exit(2)
    for line in lines[1:]:
        key, lane, _reason = line.split("\t", 2)
        if key.startswith("<site:"):
            if lane == "global":
                path = key[len("<site:"):].split(":", 1)[0]
                if not (path.startswith("tests/") and path.endswith(".rs")):
                    print(f"FAIL: unexpected site path {path}", file=sys.stderr)
                    sys.exit(2)
                targets.add(path[len("tests/"):-len(".rs")])
        else:
            keys = lane[len("lane:"):].split("+") if lane.startswith("lane:") else []
            if any(k not in INTERNAL_KEYS for k in keys):
                fns.add(key)
    return fns, targets


def main() -> int:
    if len(sys.argv) == 2 and sys.argv[1] == "--selftest":
        return selftest()
    if len(sys.argv) != 3 or sys.argv[2] != "external":
        print(__doc__, file=sys.stderr)
        return 2
    return check_file(sys.argv[1])


def check_file(junit_path) -> int:
    junit = Path(junit_path)
    root = Path(__file__).resolve().parent.parent
    fns, targets = registry_membership(root)

    try:
        tree = ET.parse(junit)
    except (OSError, ET.ParseError) as e:
        print(f"FAIL: cannot parse {junit}: {e}", file=sys.stderr)
        return 2

    intervals = []
    for case in tree.getroot().iter("testcase"):
        name = case.get("name") or ""
        classname = case.get("classname") or ""
        # classname is "<crate>::<binary>" or just the binary/target name
        target = classname.split("::")[-1]
        last_segment = name.split("::")[-1]
        if not (last_segment in fns or target in targets):
            continue
        ts, dur = case.get("timestamp"), case.get("time")
        if ts is None or dur is None:
            print(f"FAIL: member testcase without timestamp/time: {classname} {name}",
                  file=sys.stderr)
            return 2
        try:
            start = datetime.fromisoformat(ts).timestamp()
            dur_s = float(dur)
        except ValueError as e:
            print(f"FAIL: unparseable timestamp/time on {classname}::{name}: {e}",
                  file=sys.stderr)
            return 2
        # NaN compares false everywhere and would silently disable the overlap
        # comparison; Inf/negative durations are equally meaningless.
        if not (math.isfinite(start) and math.isfinite(dur_s)) or dur_s < 0:
            print(f"FAIL: non-finite or negative interval on {classname}::{name}: "
                  f"start={start} dur={dur}", file=sys.stderr)
            return 2
        intervals.append((start, start + dur_s, f"{classname}::{name}"))

    if not intervals:
        print("FAIL: junit contains no external-group member testcases", file=sys.stderr)
        return 2

    intervals.sort()
    overlaps = 0
    for (s1, e1, n1), (s2, e2, n2) in zip(intervals, intervals[1:]):
        if s2 < e1 - EPS:
            print(f"OVERLAP: {n1} [{s1:.3f},{e1:.3f}] with {n2} [{s2:.3f},{e2:.3f}]",
                  file=sys.stderr)
            overlaps += 1
    if overlaps:
        print(f"FAIL: {overlaps} overlapping pair(s) in group external "
              f"({len(intervals)} member cases)", file=sys.stderr)
        return 1
    print(f"OK: {len(intervals)} external-group cases, no wall-clock overlap")
    return 0


if __name__ == "__main__":
    sys.exit(main())
