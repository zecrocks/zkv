//! Public library surface for the `zkv` crate.
//!
//! # Quick start
//!
//! ```no_run
//! use zkv::{
//!     db::{Confirmations, Database},
//!     remote::ConnectionArgs,
//! };
//!
//! # async fn run() -> Result<(), zkv::db::ZkvError> {
//! let db = Database::open("mydb", ConnectionArgs::default())?;
//! db.sync().await?;
//! if let Some(value) = db.get("hello", Confirmations::Default)? {
//!     println!("hello = {value}");
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # API tiers
//!
//! - **Stable read/write facade:** [`db::Database`]. The recommended
//!   entry point for Rust consumers; wraps the chain-scan + replay +
//!   signed-broadcast pipeline behind a small handle.
//!
//! - **Stable protocol primitives:** [`protocol`]. Address parsing,
//!   recoverable signing/verification, memo encode/decode, the
//!   owner/writer authorization model ([`protocol::AuthRegistry`],
//!   [`protocol::Authority`], [`protocol::Scope`]), and the
//!   [`protocol::replay_with_seed`] reducer. Depend on this directly if
//!   you only need address validation, signature verification, or to
//!   feed your own memo stream through the replay logic.
//!
//! - **Shallow read client (experimental):** [`shallow`]. Chain-window
//!   reads without a local wallet: scan only the last N blocks (or walk
//!   back from the tip until a key is found) from a bare `zkv1…` address,
//!   validating signatures statelessly. Built for price-oracle consumers.
//!   Public but **not yet semver-covered**; read the module docs' trust
//!   model before depending on it.
//!
//! - **Unstable wallet plumbing:** [`config`], [`data`], [`internal`],
//!   [`remote`]. Public so the sibling `zkv-faucet` crate (and any
//!   other Rust consumer with niche needs) can reach the underlying
//!   machinery without forking. **Not** covered by semver: items,
//!   signatures, and module layout may change in any release. Prefer
//!   [`db::Database`] where it suffices.
//!
//! Common stable entry points:
//!
//! - [`db::Database::open`]: open an existing database.
//! - [`db::Database::get`] / [`db::Database::set`]: read/write a key.
//! - [`protocol::parse_zkv_addr`]: parse a `zkv1…` address string.
//! - [`protocol::verify_command`]: verify a signature against a
//!   canonical payload.

pub mod db;
pub mod protocol;

// NOTE: no outer `///` doc here on purpose. The `shallow` module carries a rich
// inner `//!` doc block whose intra-doc links use bare type names
// (`ShallowClient`, `ShallowWarning`, ...). An outer doc comment on this
// declaration would be concatenated with those inner docs and force the whole
// block to resolve in *this* (crate-root) scope, where those names aren't
// visible, breaking the links. Keeping the summary in the module's own `//!`
// lets the links resolve in the module's scope.
pub mod shallow;

/// Build-freshness check ([`freshness::build_out_of_date`]) and the shared
/// out-of-date notice ([`freshness::OUT_OF_DATE_MESSAGE`]), used by both the
/// CLI and the GUI.
pub mod freshness;

/// Per-database keys, role, and network configuration.
///
/// **Unstable.** See the crate-level docs for the stability contract.
pub mod config;

/// Data-directory resolution and database path layout.
///
/// **Unstable.** See the crate-level docs for the stability contract.
pub mod data;

/// Chain-scan / write / send / pending-tx plumbing the CLI sits on top of.
///
/// **Unstable.** See the crate-level docs for the stability contract.
pub mod internal;

/// `lightwalletd` connection arguments (server choice + direct/SOCKS5).
///
/// **Unstable.** See the crate-level docs for the stability contract.
pub mod remote;

/// Localhost web UI: a single-page database browser plus a JSON API
/// backed by [`db::Database`], served by [`gui::serve`]. Gated behind
/// the `gui` cargo feature; the `zkv gui` subcommand is the canonical
/// caller.
///
/// **Unstable.** See the crate-level docs for the stability contract.
#[cfg(feature = "gui")]
pub mod gui;

/// The bundled "demo-oracles" watch-only database that ships with a fresh
/// install. See [`demo::ensure`] (one-time auto-provision) and
/// [`demo::should_offer_reimport`] (the GUI's manual re-import button).
///
/// **Unstable.** See the crate-level docs for the stability contract.
pub mod demo;

// Internal helpers, not part of any public API tier. Public only so the
// in-tree `zkv` binary (same crate) can reach them via `crate::`.
#[doc(hidden)]
pub mod error;
#[doc(hidden)]
pub mod socks;
#[doc(hidden)]
pub mod ui;
