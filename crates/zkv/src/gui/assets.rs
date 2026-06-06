//! Embedded web-UI assets.
//!
//! The frontend (vendored React/ReactDOM/Lucide, IBM Plex fonts, the
//! design-system CSS, and the precompiled JS) is baked into the binary
//! with `include_bytes!`, so `zkv gui` serves it with no filesystem or
//! network dependency. Regenerate the precompiled JS with
//! `scripts/build-gui-assets.sh` after editing the JSX sources.

/// `index.html` with the per-launch session token and the per-response CSP
/// `nonce` substituted in. The nonce authorizes the single inline
/// `window.ZKV_TOKEN` script under a strict `script-src` (see
/// `gui::mod::static_handler`); every other script is loaded from `'self'`.
pub(super) fn index_html(token: &str, nonce: &str) -> String {
    include_str!("assets/index.html")
        .replace("__ZKV_TOKEN__", token)
        .replace("__ZKV_NONCE__", nonce)
}

const CSS: &str = "text/css; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";
const SVG: &str = "image/svg+xml";
const WOFF2: &str = "font/woff2";
const TXT: &str = "text/plain; charset=utf-8";

/// (request path, bytes, content-type). Paths are matched without the
/// leading slash. `index.html` is served by [`index_html`], not here.
const ASSETS: &[(&str, &[u8], &str)] = &[
    ("styles.css", include_bytes!("assets/styles.css"), CSS),
    ("api.js", include_bytes!("assets/api.js"), JS),
    ("forms.js", include_bytes!("assets/forms.js"), JS),
    (
        "design-system/app.css",
        include_bytes!("assets/design-system/app.css"),
        CSS,
    ),
    (
        "design-system/colors_and_type.css",
        include_bytes!("assets/design-system/colors_and_type.css"),
        CSS,
    ),
    (
        "design-system/assets/logo-mark-dark.svg",
        include_bytes!("assets/design-system/assets/logo-mark-dark.svg"),
        SVG,
    ),
    // Vendored runtime libraries.
    (
        "vendor/react.production.min.js",
        include_bytes!("assets/vendor/react.production.min.js"),
        JS,
    ),
    (
        "vendor/react-dom.production.min.js",
        include_bytes!("assets/vendor/react-dom.production.min.js"),
        JS,
    ),
    (
        "vendor/lucide.min.js",
        include_bytes!("assets/vendor/lucide.min.js"),
        JS,
    ),
    // Precompiled application (see scripts/build-gui-assets.sh).
    ("js/icon.js", include_bytes!("assets/js/icon.js"), JS),
    ("js/chrome.js", include_bytes!("assets/js/chrome.js"), JS),
    (
        "js/dashboard.js",
        include_bytes!("assets/js/dashboard.js"),
        JS,
    ),
    ("js/keys.js", include_bytes!("assets/js/keys.js"), JS),
    (
        "js/writeflow.js",
        include_bytes!("assets/js/writeflow.js"),
        JS,
    ),
    ("js/flows.js", include_bytes!("assets/js/flows.js"), JS),
    (
        "js/discover.js",
        include_bytes!("assets/js/discover.js"),
        JS,
    ),
    (
        "js/reference.js",
        include_bytes!("assets/js/reference.js"),
        JS,
    ),
    ("js/app.js", include_bytes!("assets/js/app.js"), JS),
    // Self-hosted IBM Plex (SIL OFL 1.1).
    (
        "fonts/ibm-plex-sans-latin-400-normal.woff2",
        include_bytes!("assets/fonts/ibm-plex-sans-latin-400-normal.woff2"),
        WOFF2,
    ),
    (
        "fonts/ibm-plex-sans-latin-500-normal.woff2",
        include_bytes!("assets/fonts/ibm-plex-sans-latin-500-normal.woff2"),
        WOFF2,
    ),
    (
        "fonts/ibm-plex-sans-latin-600-normal.woff2",
        include_bytes!("assets/fonts/ibm-plex-sans-latin-600-normal.woff2"),
        WOFF2,
    ),
    (
        "fonts/ibm-plex-sans-latin-700-normal.woff2",
        include_bytes!("assets/fonts/ibm-plex-sans-latin-700-normal.woff2"),
        WOFF2,
    ),
    (
        "fonts/ibm-plex-mono-latin-400-normal.woff2",
        include_bytes!("assets/fonts/ibm-plex-mono-latin-400-normal.woff2"),
        WOFF2,
    ),
    (
        "fonts/ibm-plex-mono-latin-500-normal.woff2",
        include_bytes!("assets/fonts/ibm-plex-mono-latin-500-normal.woff2"),
        WOFF2,
    ),
    (
        "fonts/ibm-plex-mono-latin-600-normal.woff2",
        include_bytes!("assets/fonts/ibm-plex-mono-latin-600-normal.woff2"),
        WOFF2,
    ),
    (
        "fonts/ibm-plex-serif-latin-400-normal.woff2",
        include_bytes!("assets/fonts/ibm-plex-serif-latin-400-normal.woff2"),
        WOFF2,
    ),
    (
        "fonts/ibm-plex-serif-latin-500-normal.woff2",
        include_bytes!("assets/fonts/ibm-plex-serif-latin-500-normal.woff2"),
        WOFF2,
    ),
    (
        "fonts/ibm-plex-serif-latin-400-italic.woff2",
        include_bytes!("assets/fonts/ibm-plex-serif-latin-400-italic.woff2"),
        WOFF2,
    ),
    (
        "fonts/ibm-plex-serif-latin-500-italic.woff2",
        include_bytes!("assets/fonts/ibm-plex-serif-latin-500-italic.woff2"),
        WOFF2,
    ),
    ("fonts/OFL.txt", include_bytes!("assets/fonts/OFL.txt"), TXT),
];

/// Look up a static asset by request path (no leading slash).
pub(super) fn lookup(path: &str) -> Option<(&'static [u8], &'static str)> {
    ASSETS
        .iter()
        .find(|(p, _, _)| *p == path)
        .map(|(_, bytes, mime)| (*bytes, *mime))
}

/// The third-party license bundle, generated at build time into `OUT_DIR` by
/// `build.rs` (see `build/license_bundle.rs`) and embedded gzip-compressed;
/// the raw text is ~96% redundant, so the compressed form is ~0.2 MB vs ~6 MB.
/// Inflated once on first call and cached for the process lifetime; served by
/// the "View Licenses" screen via `Engine::licenses` (one code path for both
/// the HTTP and Tauri transports, so it doesn't depend on Tauri's
/// `frontendDist` serving a generated file).
pub(super) fn licenses_text() -> &'static str {
    use std::io::Read as _;
    use std::sync::OnceLock;

    static GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/licenses.txt.gz"));
    static TEXT: OnceLock<String> = OnceLock::new();
    TEXT.get_or_init(|| {
        let mut s = String::new();
        match flate2::read::GzDecoder::new(GZ).read_to_string(&mut s) {
            Ok(_) => s,
            Err(_) => "Failed to decode the license bundle.".to_owned(),
        }
    })
    .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn license_bundle_inflates_and_mentions_project() {
        let text = licenses_text();
        // The gzip embed round-trips to readable text...
        assert!(text.len() > 1000, "bundle unexpectedly small");
        // ...with the project + a known dependency documented.
        assert!(text.contains("THIRD-PARTY SOFTWARE NOTICES"));
        assert!(text.contains("zkv (this project)"));
        // Cached: a second call returns the same backing allocation.
        assert!(std::ptr::eq(licenses_text(), text));
    }
}
