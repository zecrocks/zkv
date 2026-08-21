#!/usr/bin/env python3
"""Release guard for the build-freshness settings in crates/zkv/src/freshness.rs.

A shipped build's expiry is derived from its compile time (build.rs stamps
ZKV_BUILD_UNIX, freshness.rs adds FRESH_WINDOW_SECS), so it cannot go stale on
its own. Two things still can, and this guard is what catches them before a tag
turns into a release:

  * FRESH_WINDOW_SECS shrinking to something that would nag users of a current
    release (min 30 days).
  * ANCHOR_HEIGHT / ANCHOR_UNIX ageing, which makes the projected expiry height
    drift from the wall-clock expiry (max 180 days old).

Exits non-zero with an explanation when either check fails.
"""

import re
import sys
import time
from pathlib import Path

MIN_WINDOW_DAYS = 30
MAX_ANCHOR_AGE_DAYS = 180
DAY = 86_400

SOURCE = Path(__file__).resolve().parent.parent / "crates" / "zkv" / "src" / "freshness.rs"


def const(name: str, text: str) -> int:
    """Read `const NAME: <ty> = <expr>;` and evaluate its integer expression."""
    match = re.search(rf"const {name}:\s*\w+\s*=\s*([^;]+);", text)
    if not match:
        sys.exit(f"check-build-freshness: {name} not found in {SOURCE}")
    expr = match.group(1).replace("_", "").strip()
    if not re.fullmatch(r"[0-9*+\s]+", expr):
        sys.exit(f"check-build-freshness: {name} is not a plain integer expression: {expr}")
    return int(eval(expr))  # noqa: S307 - guarded to digits and * + above


def main() -> None:
    text = SOURCE.read_text()
    window = const("FRESH_WINDOW_SECS", text)
    anchor_unix = const("ANCHOR_UNIX", text)
    anchor_height = const("ANCHOR_HEIGHT", text)
    now = int(time.time())

    window_days = window / DAY
    anchor_age_days = (now - anchor_unix) / DAY
    print(f"freshness window: {window_days:.1f} days")
    print(f"height anchor:    {anchor_height} at unix {anchor_unix} ({anchor_age_days:.1f} days old)")

    failures = []
    if window < MIN_WINDOW_DAYS * DAY:
        failures.append(
            f"FRESH_WINDOW_SECS is {window_days:.1f} days, below the {MIN_WINDOW_DAYS} day minimum: "
            "a release cut with this window would start nagging its own users."
        )
    if anchor_age_days > MAX_ANCHOR_AGE_DAYS:
        failures.append(
            f"the mainnet anchor is {anchor_age_days:.0f} days old, over the {MAX_ANCHOR_AGE_DAYS} day "
            "limit: refresh ANCHOR_HEIGHT/ANCHOR_UNIX (see the comment in freshness.rs)."
        )
    if anchor_age_days < 0:
        failures.append("the mainnet anchor is in the future; check ANCHOR_UNIX.")

    if failures:
        for f in failures:
            print(f"check-build-freshness: {f}", file=sys.stderr)
        sys.exit(1)
    print("check-build-freshness: ok")


if __name__ == "__main__":
    main()
