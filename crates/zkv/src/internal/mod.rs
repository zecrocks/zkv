//! Internal (non-CLI) helpers shared across commands.

pub(crate) mod account;
pub mod funding;
pub mod lock;
pub mod pending;
pub mod recover;
pub mod send;
pub mod snapshot;
pub mod state;
pub mod sync;
pub mod write;

// `protocol` is the library's stable public module. Re-export it here so
// existing `crate::internal::protocol::*` references in `internal::*`
// continue to compile.
pub use crate::protocol;
