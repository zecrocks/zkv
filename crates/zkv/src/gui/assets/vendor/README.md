# Vendored front-end libraries

These are pre-built browser/UMD bundles checked in verbatim, not npm
dependencies. `cargo build` needs no Node to embed them (`include_bytes!` in
`../../assets.rs`); Node is only needed to regenerate the JSX-derived
`../js/*.js` (`scripts/build-gui-assets.sh`).

The canonical license text for all three is emitted into the embedded license
bundle by `crates/zkv/build/license_bundle.rs` (`bundled_web_assets`) and
surfaced via `zkv --licenses` and the GUI "View Licenses" screen.

| File | Library | Version | License | Upstream |
|------|---------|---------|---------|----------|
| `react.production.min.js` | React | 18.3.1 | MIT | https://github.com/facebook/react |
| `react-dom.production.min.js` | React-DOM | 18.3.1 | MIT | https://github.com/facebook/react |
| `lucide.min.js` | Lucide | 0.544.0 | ISC | https://github.com/lucide-icons/lucide |

The version is also baked into each file's header (React/React-DOM as a version
string in the bundle; Lucide in its `@license` comment).

## Updating a bundle

Replace the file with the matching minified production build from the upstream
release for the desired version, then update the version in the table above and
verify the embedded license bundle still builds (`cargo build -p zkv
--features gui`). For Lucide, use the UMD build (`lucide.min.js`) so the global
`lucide` factory the SPA calls is present.
