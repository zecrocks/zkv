//! Pure-data protocol layer: zkv address parsing, signing, memo encoding, replay.
//!
//! No I/O, no clap, no `tokio`. Commands compose these primitives.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use anyhow::{anyhow, bail};
use bech32::{primitives::decode::CheckedHrpstring, Bech32m, Hrp};
use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
use sha2::{Digest, Sha256};
use transparent::keys::{NonHardenedChildIndex, TransparentKeyScope};
use zcash_address::unified::{self, Container, Encoding};
use zcash_keys::keys::{ReceiverRequirement, UnifiedAddressRequest, UnifiedFullViewingKey};
use zcash_protocol::{
    consensus::{self, NetworkType},
    memo::{Memo, MemoBytes},
    ShieldedProtocol,
};

mod address;
mod auth;
mod memo;
mod replay;
mod sign;
#[cfg(test)]
mod tests;
mod types;
mod version;

pub use address::*;
pub use auth::*;
pub use memo::*;
pub use replay::*;
pub use sign::*;
pub use types::*;
pub use version::*;

// Crate-internal helpers (pub(crate) in their modules), surfaced here so the
// test module and other crate modules reach them without widening the public
// (semver-covered) protocol API.
pub(crate) use replay::{bump_hw, seq_in_window};
pub(crate) use sign::parse_sig_line;
pub(crate) use types::{MAGIC_PREFIX, SIGNED_MAGIC, SIG_HEX_LEN, WIRE_MAGIC};

// Surfaced only for the in-crate test module (kept off non-test builds so they
// don't read as unused there).
#[cfg(test)]
pub(crate) use address::{relabel_hrp, zkv_hrp};
#[cfg(test)]
pub(crate) use sign::MAX_SEQ_BYTES;
