# zcash_zkv

A key-value store whose entries are signed Zcash shielded memos. Anyone holding
the `zkv1…` address can read; only authorized signers can write. The chain is
the source of truth, and everything on disk is a rebuildable cache.

A database is a Unified Full Viewing Key with an embedded birthday, in one
shielded pool. Reads scan the chain with the UFVK; writes carry recoverable
secp256k1 signatures.

The crate is published as `zcash_zkv`; the library target, the CLI binary, and
every path in the docs are spelled `zkv`.

More information: <https://zec.rocks/zkv>

**Alpha. For testing, not production.** Signing keys are not yet
password-protected and seeds are not meaningfully encrypted at rest.

## As a tool

```sh
cargo install zcash_zkv                     # the `zkv` CLI and the web UI
cargo install zcash_zkv --features desktop  # adds the native desktop window
```

```sh
zkv init
zkv set zec_usd 1008.33
zkv get zec_usd
```

Prebuilt binaries for Linux, macOS, and Windows are on the
[releases page](https://github.com/zecrocks/zkv/releases).

## As a library

The default features build the CLI and the web UI along with the library, which
is what makes `cargo install` work. If you only want the database, turn them
off:

```toml
[dependencies]
zcash_zkv = { version = "0.2", default-features = false, features = ["transparent-inputs"] }
```

That drops clap, axum, reqwest, qrcode, and the tracing subscriber from your
dependency tree. Then:

```rust
use zkv::db::Database;
```

Feature summary:

| feature | default | what it adds |
| --- | --- | --- |
| `transparent-inputs` | yes | transparent input support in the wallet backend |
| `default-subscriber` | yes | `db::install_default_subscriber()`, a stderr `tracing` subscriber honoring `RUST_LOG`. Turn off if your app installs its own |
| `cli` | yes | required to build the `zkv` binary; pulls clap, qrcode, terminal_size |
| `gui` | yes | `zkv::gui::serve` and the `zkv gui-browser` web UI; pulls axum, reqwest, flate2 |
| `desktop` | no | the native window behind `zkv gui`; pulls tauri, and on Linux needs the system webview dev libraries |

## License

MIT OR Apache-2.0.
