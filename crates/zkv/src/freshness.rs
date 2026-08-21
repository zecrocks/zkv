//! Build-freshness check shared by the CLI and the GUI.
//!
//! A shipped build carries a hard expiry. Past it, the CLI prints a notice on
//! every command and the GUI shows a banner above the navbar, both pointing
//! users at the latest release. This is a single source of truth so the two
//! surfaces never disagree.
//!
//! The expiry is **derived from when the binary was compiled**: `build.rs`
//! stamps the build time into `ZKV_BUILD_UNIX` and a build stays fresh for
//! `FRESH_WINDOW_SECS` after that. It used to be a hard-coded cutoff date,
//! which meant every release had to remember to move it: rc1 shipped with an
//! eight week fuse, rc5 with three days, and rc6 shipped already expired. A
//! build-relative window cannot go stale on its own.
//!
//! Expiry is checked two independent ways, ORed together:
//! - **Wall clock** at or past [`crate::freshness::expiry_unix()`].
//! - **Zcash mainnet height** at or past [`crate::freshness::expiry_height()`]. This defends
//!   against a user setting their clock back: the chain tip can't be faked the
//!   same way. It only fires where a live tip is available (the GUI, which
//!   always has one); the CLI's first-line print is clock-only because it runs
//!   before any chain access.

use std::time::{SystemTime, UNIX_EPOCH};

/// The notice shown (verbatim) by both the CLI and the GUI once a build is past
/// its expiry.
pub const OUT_OF_DATE_MESSAGE: &str = "This build is out of date. Please download the latest build from https://github.com/zecrocks/zkv to continue using zkv.";

/// How long a build stays fresh after it was compiled.
///
/// 90 days: comfortably longer than the release cadence, so a user on the
/// current release is never nagged, while an abandoned build still ages out.
/// `scripts/check-build-freshness.py` (run by `release.yml`) refuses to cut a
/// release if this drops below 30 days.
const FRESH_WINDOW_SECS: u64 = 90 * 24 * 60 * 60;

/// When this binary was compiled, in unix seconds, stamped by `build.rs`.
///
/// `build.rs` honours an explicit `ZKV_BUILD_UNIX` and then `SOURCE_DATE_EPOCH`
/// (so a reproducible build pins the same expiry as the release it reproduces)
/// before falling back to the clock. Note that cargo caches build-script
/// output: an incremental rebuild keeps the stamp from the last time this crate
/// was actually rebuilt. Release artifacts come off a fresh checkout, so theirs
/// is always the real build time.
const BUILD_UNIX: u64 = parse_unix(env!("ZKV_BUILD_UNIX"));

/// Mainnet anchor (height and its UTC timestamp) used to project
/// [`crate::freshness::expiry_height()`], refreshed 2026-08-21 from an explorer:
/// height 3,455,954 at 2026-08-21T20:26:39Z.
///
/// To refresh: take a recent mainnet height and its timestamp from any
/// explorer, e.g. `curl -s https://api.blockchair.com/zcash/stats` reports
/// `blocks` (height) and `best_block_time`. The projection only has to be
/// roughly right, since the clock check is the precise gate and this is the
/// tamper backstop, but the further the anchor is in the past the more the
/// projection drifts: `check-build-freshness.py` fails a release once the
/// anchor is more than 180 days old.
const ANCHOR_HEIGHT: u32 = 3_455_954;
const ANCHOR_UNIX: u64 = 1_787_343_999;

/// Post-Blossom target block time (1152 blocks/day). Measured against the
/// previous anchor the real average came out at 75.4 s, so the target is a good
/// enough projection basis.
const TARGET_BLOCK_SECS: u64 = 75;

/// The wall-clock instant this build expires: compile time plus the window.
pub const fn expiry_unix() -> u64 {
    BUILD_UNIX + FRESH_WINDOW_SECS
}

/// The mainnet height this build expires at, projected from the anchor at the
/// target block time. Saturates rather than wrapping, and never returns a
/// height below the anchor.
pub const fn expiry_height() -> u32 {
    let expiry = expiry_unix();
    if expiry <= ANCHOR_UNIX {
        return ANCHOR_HEIGHT;
    }
    let blocks = (expiry - ANCHOR_UNIX) / TARGET_BLOCK_SECS;
    if blocks > u32::MAX as u64 {
        return u32::MAX;
    }
    ANCHOR_HEIGHT.saturating_add(blocks as u32)
}

/// Whether this build is past its expiry, by wall clock or by chain height.
///
/// `chain_tip` is the live **mainnet** tip if one is known (`None` when there is
/// no current database or the probe failed, and on non-mainnet surfaces). The
/// clock check stands alone; the height check only contributes when a tip is
/// supplied.
pub fn build_out_of_date(chain_tip: Option<u32>) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    out_of_date_at(now, chain_tip)
}

/// The pure form of [`crate::freshness::build_out_of_date()`]: no clock, no globals, so the policy
/// is testable. Returns `false` for a `now_unix` before the build's expiry and
/// no qualifying tip.
pub fn out_of_date_at(now_unix: u64, chain_tip: Option<u32>) -> bool {
    now_unix >= expiry_unix() || chain_tip.is_some_and(|h| h >= expiry_height())
}

/// Parse the `ZKV_BUILD_UNIX` stamp at compile time. Panics the build (at const
/// evaluation) if `build.rs` ever emits something that isn't digits.
const fn parse_unix(s: &str) -> u64 {
    let bytes = s.as_bytes();
    assert!(!bytes.is_empty(), "ZKV_BUILD_UNIX must not be empty");
    let mut acc: u64 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        assert!(
            b >= b'0' && b <= b'9',
            "ZKV_BUILD_UNIX must be unix seconds (digits only)"
        );
        acc = acc * 10 + (b - b'0') as u64;
        i += 1;
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_is_the_window_past_the_build() {
        assert_eq!(expiry_unix(), BUILD_UNIX + FRESH_WINDOW_SECS);
        // A build is fresh the moment it is built, and expired the second the
        // window elapses.
        assert!(!out_of_date_at(BUILD_UNIX, None));
        assert!(!out_of_date_at(expiry_unix() - 1, None));
        assert!(out_of_date_at(expiry_unix(), None));
    }

    #[test]
    fn chain_height_expires_independently_of_the_clock() {
        let fresh_clock = BUILD_UNIX;
        assert!(!out_of_date_at(fresh_clock, Some(expiry_height() - 1)));
        // A clock rolled back to build time doesn't help once the chain says
        // otherwise.
        assert!(out_of_date_at(fresh_clock, Some(expiry_height())));
        assert!(out_of_date_at(0, Some(expiry_height() + 1000)));
    }

    #[test]
    fn projected_height_tracks_the_window() {
        // The projection must land ahead of the anchor, by roughly the window
        // converted at the target block time (allow the anchor's own age).
        assert!(expiry_height() > ANCHOR_HEIGHT);
        let window_blocks = (FRESH_WINDOW_SECS / TARGET_BLOCK_SECS) as u32;
        assert!(expiry_height() >= ANCHOR_HEIGHT + window_blocks / 2);
    }

    #[test]
    fn parses_the_build_stamp() {
        assert_eq!(parse_unix("0"), 0);
        assert_eq!(parse_unix("1787343999"), 1_787_343_999);
    }
}
