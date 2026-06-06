use std::process::Command;

// Generates the GUI's third-party license bundle into `$OUT_DIR/licenses.txt`
// (see `src/gui/assets.rs`). Only needed, and only compiled, for the `gui`
// feature, so a lean CLI build skips both the work and the `toml` parse.
#[cfg(feature = "gui")]
#[path = "build/license_bundle.rs"]
mod license_bundle;

fn main() {
    // Capture the short git SHA (plus a `-dirty` marker for an unclean working
    // tree) at build time so `zkv --version` reports exactly which commit the
    // binary came from, mirroring the `dev-<sha>` scheme the release workflow
    // already uses for artifact names. Falls back to "unknown" when git isn't
    // available (e.g. building from a packaged crate tarball outside a
    // checkout).
    // An explicit `ZKV_GIT_SHA` in the environment wins over the git probe.
    // CI sets it to the exact commit (`release.yml`) so release artifacts get a
    // clean, deterministic SHA with no `-dirty` marker, independent of whatever
    // untracked scratch a build step may have left in the tree.
    let sha = match std::env::var("ZKV_GIT_SHA") {
        Ok(s) if !s.trim().is_empty() => s.trim().to_owned(),
        _ => git_sha().unwrap_or_else(|| "unknown".to_owned()),
    };
    println!("cargo:rustc-env=ZKV_GIT_SHA={sha}");
    // Recompute when the override changes, or (in its absence) when HEAD moves
    // or the index changes, so the embedded SHA and the dirty marker stay
    // current across rebuilds.
    println!("cargo:rerun-if-env-changed=ZKV_GIT_SHA");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");

    // Generate the third-party license bundle for the embedded GUI.
    #[cfg(feature = "gui")]
    license_bundle::generate();

    // Tauri codegen runs only for the opt-in `desktop` feature; a default
    // build pulls in neither `tauri-build` nor a system webview toolchain.
    #[cfg(feature = "desktop")]
    tauri_build::build();
}

/// The short HEAD SHA, suffixed with `-dirty` when the working tree has
/// uncommitted changes. `None` if git isn't available or this isn't a checkout.
fn git_sha() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut sha = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    if sha.is_empty() {
        return None;
    }
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    if dirty {
        sha.push_str("-dirty");
    }
    Some(sha)
}
