//! Build-freshness check shared by the CLI and the GUI.
//!
//! A shipped build carries a hard expiry. Past it, the CLI prints a notice on
//! every command and the GUI shows a banner above the navbar, both pointing
//! users at the latest release. This is a single source of truth so the two
//! surfaces never disagree.
//!
//! Expiry is checked two independent ways, ORed together:
//! - **Wall clock** at or past `BUILD_EXPIRY_UNIX`.
//! - **Zcash mainnet height** at or past `BUILD_EXPIRY_BLOCK_HEIGHT`. This
//!   defends against a user setting their clock back: the chain tip can't be
//!   faked the same way. It only fires where a live tip is available (the GUI,
//!   which always has one); the CLI's first-line print is clock-only because it
//!   runs before any chain access.

use std::time::{SystemTime, UNIX_EPOCH};

/// The notice shown (verbatim) by both the CLI and the GUI once a build is past
/// its expiry.
pub const OUT_OF_DATE_MESSAGE: &str = "This build is out of date. Please download the latest build from https://github.com/zecrocks/zkv to continue using zkv.";

/// Unix timestamp (seconds, UTC) at which this build is considered out of date.
///
/// `2026-08-01T00:00:00Z`. To recompute for a new cutoff, take the date at
/// midnight UTC and convert to seconds since the Unix epoch, e.g.
/// `date -u -d 2026-08-01 +%s` (GNU) or `python3 -c "import datetime as
/// d;print(int(d.datetime(2026,8,1,tzinfo=d.timezone.utc).timestamp()))"`.
/// Keep it in step with `BUILD_EXPIRY_BLOCK_HEIGHT`: both must describe the
/// same date.
const BUILD_EXPIRY_UNIX: u64 = 1_785_542_400; // 2026-08-01T00:00:00Z

/// Zcash **mainnet** block height at which this build is considered out of date,
/// a clock-tamper-resistant backstop for `BUILD_EXPIRY_UNIX`. Only mainnet
/// heights are compared, so testnet/reg-test tips never trip it.
///
/// `2026-08-01`, projected from the post-Blossom **75 s** target block time
/// (1152 blocks/day). To recompute for a new cutoff:
///
/// 1. Get a recent anchor (a known mainnet height + its UTC timestamp) from any
///    explorer, e.g. `curl -s https://api.blockchair.com/zcash/stats` reports
///    `blocks` (height) and `best_block_time`.
/// 2. `height = anchor_height + (cutoff_unix - anchor_unix) / 75`, rounded.
///
/// Worked example for this value: anchor height 3,368,639 at 2026-06-06 16:26
/// UTC (unix 1,780,763,198), cutoff 2026-08-01 00:00 UTC (unix 1,785,542,400):
/// 3,368,639 + (1,785,542,400 - 1,780,763,198) / 75 ≈ 3,368,639 + 63,723 ≈
/// 3,432,362. (Block time is only a target, so this drifts a little; that is
/// fine, the clock check is the precise gate and this is the tamper backstop.)
const BUILD_EXPIRY_BLOCK_HEIGHT: u32 = 3_432_362; // ~2026-08-01

/// Whether this build is past its expiry, by wall clock or by chain height.
///
/// `chain_tip` is the live **mainnet** tip if one is known (`None` when there is
/// no current database or the probe failed, and on non-mainnet surfaces). The
/// clock check stands alone; the height check only contributes when a tip is
/// supplied. Returns `false` if the system clock is somehow before the Unix
/// epoch and no qualifying tip is given.
pub fn build_out_of_date(chain_tip: Option<u32>) -> bool {
    let clock_expired = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() >= BUILD_EXPIRY_UNIX)
        .unwrap_or(false);
    let height_expired = chain_tip.is_some_and(|h| h >= BUILD_EXPIRY_BLOCK_HEIGHT);
    clock_expired || height_expired
}
