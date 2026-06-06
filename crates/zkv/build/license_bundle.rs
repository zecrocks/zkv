//! Build-time generator for the GUI's third-party license bundle.
//!
//! Invoked from `build.rs` only under the `gui` feature. It enumerates the
//! workspace's resolved dependency graph from `Cargo.lock`, locates each
//! crate's extracted source in the Cargo registry cache, and reproduces the
//! verbatim LICENSE / COPYING / NOTICE text each crate ships, along with its
//! SPDX expression, repository, and authors (read from the crate's own
//! `Cargo.toml`). The result is written to `$OUT_DIR/licenses.txt` and embedded
//! by `src/gui/assets.rs`, so it is regenerated on every build that touches the
//! lockfile and never committed to the repository.
//!
//! Only crates whose source is present in the registry cache are documented;
//! i.e. exactly the crates this build actually compiled. Crates that ship no
//! license file are listed with their SPDX identifier; the canonical text of
//! the common licenses is appended once at the end for reference.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const RULE: &str = "==============================================================================";
const THIN: &str = "------------------------------------------------------------------------------";

const LICENSE_HINTS: &[&str] = &["licen", "copying", "notice", "unlicen", "ofl"];

/// Entry point called from `build.rs`. Best-effort: any failure degrades to a
/// smaller bundle rather than breaking the build (the project + bundled-asset
/// licenses are always emitted).
pub fn generate() {
    let manifest_dir = PathBuf::from(env_var("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| manifest_dir.clone());
    let lock_path = workspace_root.join("Cargo.lock");

    // Rebuild the bundle whenever the lockfile or this generator changes.
    println!("cargo:rerun-if-changed={}", lock_path.display());
    println!("cargo:rerun-if-changed=build/license_bundle.rs");

    let mut out = String::new();
    header(&mut out);
    project_license(&mut out, &workspace_root);
    bundled_web_assets(&mut out, &manifest_dir);

    out.push_str(RULE);
    out.push_str("\nRust dependencies\n");
    out.push_str(RULE);
    out.push_str("\n\n");

    let registry_index = build_registry_index();
    let packages = read_lock_packages(&lock_path);
    for (name, version) in packages {
        // Skip our own workspace crates; covered by the project license above.
        if name == "zkv" || name == "zkv-faucet" {
            continue;
        }
        let dir = match registry_index.get(&format!("{name}-{version}")) {
            Some(d) => d,
            // Source not extracted for this build's feature set → not linked,
            // so it isn't part of what we ship. Skip silently.
            None => continue,
        };
        crate_entry(&mut out, &name, &version, dir);
    }

    appendix(&mut out);

    // Embed gzip-compressed: the bundle is ~96% redundant (the same license
    // texts recur across hundreds of crates), so this turns a ~6 MB embed into
    // ~0.2 MB. `src/gui/assets.rs` inflates it lazily on first view.
    let out_dir = env_var("OUT_DIR");
    let dest = Path::new(&out_dir).join("licenses.txt.gz");
    match gzip(out.as_bytes()) {
        Ok(compressed) => {
            if let Err(e) = fs::write(&dest, compressed) {
                println!("cargo:warning=failed to write license bundle: {e}");
                write_fallback(&dest);
            }
        }
        Err(e) => {
            println!("cargo:warning=failed to compress license bundle: {e}");
            write_fallback(&dest);
        }
    }
}

fn gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    use std::io::Write as _;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    enc.write_all(data)?;
    enc.finish()
}

/// Ensure the embed target exists (gzipped) so the crate still compiles even if
/// generation hit an error.
fn write_fallback(dest: &Path) {
    if let Ok(bytes) = gzip(b"License bundle generation failed.\n") {
        let _ = fs::write(dest, bytes);
    }
}

fn header(out: &mut String) {
    out.push_str(RULE);
    out.push_str("\nTHIRD-PARTY SOFTWARE NOTICES AND LICENSES\n");
    out.push_str(RULE);
    out.push_str(
        "\n\nzkv: a Redis-style key-value store backed by Zcash shielded memos.\n\n\
         This document lists the open-source software bundled with or linked into\n\
         zkv and the zkv Browser, together with the license texts their authors\n\
         ship. It is generated at build time from the resolved dependency graph\n\
         (Cargo.lock) and each crate's published source.\n\n\
         Where a crate is offered under several licenses (an SPDX `OR`), zkv\n\
         elects a permissive option (typically MIT or Apache-2.0); the full text\n\
         of every license shipped by the crate is nonetheless reproduced below.\n\n",
    );
}

fn project_license(out: &mut String, workspace_root: &Path) {
    out.push_str(RULE);
    out.push_str("\nzkv (this project)\n");
    out.push_str(RULE);
    out.push_str("\n\nLicense: MIT OR Apache-2.0\nRepository: https://github.com/zecrocks/zkv\n\n");
    if let Some(text) = read_to_string(&workspace_root.join("LICENSE-MIT")) {
        section(out, "MIT License", &text);
    }
    if let Some(text) = read_to_string(&workspace_root.join("LICENSE-APACHE")) {
        section(out, "Apache License 2.0", &text);
    }
}

fn bundled_web_assets(out: &mut String, manifest_dir: &Path) {
    out.push_str(RULE);
    out.push_str("\nBundled web assets (zkv Browser GUI)\n");
    out.push_str(RULE);
    out.push_str(
        "\n\nThe desktop/browser GUI bundles the following front-end libraries\n\
         and fonts, served locally with no network dependency.\n\n",
    );
    section(
        out,
        "React and React-DOM (MIT License)\nCopyright (c) Meta Platforms, Inc. and affiliates.\nhttps://github.com/facebook/react",
        MIT_TEXT,
    );
    section(
        out,
        "Lucide icons (ISC License)\nCopyright (c) 2022, Lucide Contributors\n(Lucide is a fork of Feather Icons, Copyright (c) 2013-2022 Cole Bemis.)\nhttps://github.com/lucide-icons/lucide",
        LUCIDE_TEXT,
    );
    let ofl = manifest_dir.join("src/gui/assets/fonts/OFL.txt");
    if let Some(text) = read_to_string(&ofl) {
        section(
            out,
            "IBM Plex fonts (SIL Open Font License 1.1)\nCopyright (c) 2019 IBM Corp. https://github.com/IBM/plex\nThe full OFL 1.1 text is also shipped at /fonts/OFL.txt.",
            &text,
        );
    }
}

fn crate_entry(out: &mut String, name: &str, version: &str, dir: &Path) {
    let (license, repository, authors) = read_crate_meta(dir);

    out.push_str(RULE);
    out.push('\n');
    out.push_str(&format!("{name} {version}\n"));
    out.push_str(RULE);
    out.push('\n');
    out.push_str(&format!(
        "License: {}\n",
        license.as_deref().unwrap_or("unspecified")
    ));
    if let Some(repo) = repository {
        out.push_str(&format!("Repository: {repo}\n"));
    }
    if let Some(authors) = authors {
        if !authors.is_empty() {
            out.push_str(&format!("Authors: {authors}\n"));
        }
    }
    out.push('\n');

    let files = collect_license_files(dir);
    if files.is_empty() {
        out.push_str(
            "(No license file was shipped in this crate's source tree. The\n \
             license it declares is shown above; the canonical text of the\n \
             common licenses is reproduced in the appendix below.)\n\n",
        );
    } else {
        for (fname, text) in files {
            section(out, &fname, &text);
        }
    }
}

fn appendix(out: &mut String) {
    out.push_str(RULE);
    out.push_str("\nAPPENDIX: Canonical license texts\n");
    out.push_str(RULE);
    out.push_str(
        "\n\nFor crates that did not ship their own license file, the canonical\n\
         text of the licenses they reference is reproduced here for completeness.\n\n",
    );
    section(out, "MIT License", MIT_TEXT);
    section(out, "ISC License", ISC_TEXT);
    section(out, "BSD 2-Clause License", BSD2_TEXT);
    section(out, "BSD 3-Clause License", BSD3_TEXT);
    section(out, "BSD Zero Clause License (0BSD)", ZEROBSD_TEXT);
    section(out, "zlib License", ZLIB_TEXT);
    section(out, "The Unlicense", UNLICENSE_TEXT);
    section(out, "Boost Software License 1.0 (BSL-1.0)", BSL_TEXT);
    section(out, "LLVM Exception (to Apache-2.0)", LLVM_EXCEPTION_TEXT);
    section(out, "CC0 1.0 Universal", CC0_TEXT);
    section(out, "Unicode License v3", UNICODE_TEXT);
    section(out, "CDLA-Permissive-2.0", CDLA_TEXT);
    section(
        out,
        "Apache License 2.0",
        "The full Apache-2.0 text is reproduced above under \"zkv (this project)\".",
    );
}

// --- helpers --------------------------------------------------------------

fn section(out: &mut String, title: &str, body: &str) {
    out.push_str(THIN);
    out.push('\n');
    out.push_str(title);
    out.push('\n');
    out.push_str(THIN);
    out.push('\n');
    out.push_str(body.trim_end_matches('\n'));
    out.push_str("\n\n");
}

fn env_var(k: &str) -> String {
    std::env::var(k).unwrap_or_default()
}

fn read_to_string(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok().filter(|s| !s.trim().is_empty())
}

/// Cargo home (`$CARGO_HOME`, else `$HOME/.cargo`).
fn cargo_home() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("CARGO_HOME") {
        return Some(PathBuf::from(h));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo"))
}

/// Map `"{name}-{version}" -> extracted source dir` across every registry src
/// cache under Cargo home (there is usually one, named with an index hash).
fn build_registry_index() -> BTreeMap<String, PathBuf> {
    let mut index = BTreeMap::new();
    let Some(home) = cargo_home() else {
        return index;
    };
    let src_root = home.join("registry").join("src");
    let Ok(registries) = fs::read_dir(&src_root) else {
        return index;
    };
    for reg in registries.flatten() {
        let Ok(entries) = fs::read_dir(reg.path()) else {
            continue;
        };
        for e in entries.flatten() {
            if e.path().is_dir() {
                if let Some(name) = e.file_name().to_str() {
                    index.insert(name.to_owned(), e.path());
                }
            }
        }
    }
    index
}

/// `(name, version)` of every package in `Cargo.lock`, sorted.
fn read_lock_packages(lock_path: &Path) -> Vec<(String, String)> {
    let Some(text) = read_to_string(lock_path) else {
        return Vec::new();
    };
    let Ok(value) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(pkgs) = value.get("package").and_then(|p| p.as_array()) {
        for p in pkgs {
            let name = p.get("name").and_then(|v| v.as_str());
            let version = p.get("version").and_then(|v| v.as_str());
            if let (Some(n), Some(v)) = (name, version) {
                out.push((n.to_owned(), v.to_owned()));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Read `license`, `repository`, and `authors` from a crate's `Cargo.toml`.
/// Fields inherited from a workspace (`{ workspace = true }`) can't be resolved
/// here, so they come back as `None`.
fn read_crate_meta(dir: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let Some(text) = read_to_string(&dir.join("Cargo.toml")) else {
        return (None, None, None);
    };
    let Ok(value) = text.parse::<toml::Table>() else {
        return (None, None, None);
    };
    let pkg = match value.get("package").and_then(|p| p.as_table()) {
        Some(p) => p,
        None => return (None, None, None),
    };
    let license = pkg
        .get("license")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let repository = pkg
        .get("repository")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let authors = pkg.get("authors").and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    });
    (license, repository, authors)
}

/// `(filename, text)` for every license-ish file in the crate root.
fn collect_license_files(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    let mut names: Vec<_> = entries
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    for name in names {
        let low = name.to_lowercase();
        if !LICENSE_HINTS.iter().any(|h| low.contains(h)) {
            continue;
        }
        if let Some(text) = read_to_string(&dir.join(&name)) {
            out.push((name, text));
        }
    }
    out
}

// --- canonical / template license texts -----------------------------------

const MIT_TEXT: &str = "\
Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the \"Software\"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.";

const LUCIDE_TEXT: &str = "\
ISC License

Copyright (c) for portions of Lucide are held by Cole Bemis 2013-2022 as part
of Feather (MIT). All other copyright (c) for Lucide are held by Lucide
Contributors 2022.

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED \"AS IS\" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH
REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY
AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT,
INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM
LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR
OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR
PERFORMANCE OF THIS SOFTWARE.";

const ISC_TEXT: &str = "\
Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED \"AS IS\" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH
REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY
AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT,
INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM
LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR
OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR
PERFORMANCE OF THIS SOFTWARE.";

const BSD3_TEXT: &str = "\
Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its contributors
   may be used to endorse or promote products derived from this software
   without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS \"AS IS\"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.";

const BSD2_TEXT: &str = "\
Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS \"AS IS\"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.";

const ZEROBSD_TEXT: &str = "\
Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted.

THE SOFTWARE IS PROVIDED \"AS IS\" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH
REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY
AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT,
INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM
LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR
OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR
PERFORMANCE OF THIS SOFTWARE.";

const ZLIB_TEXT: &str = "\
This software is provided 'as-is', without any express or implied warranty. In
no event will the authors be held liable for any damages arising from the use
of this software.

Permission is granted to anyone to use this software for any purpose,
including commercial applications, and to alter it and redistribute it freely,
subject to the following restrictions:

1. The origin of this software must not be misrepresented; you must not claim
   that you wrote the original software. If you use this software in a product,
   an acknowledgment in the product documentation would be appreciated but is
   not required.

2. Altered source versions must be plainly marked as such, and must not be
   misrepresented as being the original software.

3. This notice may not be removed or altered from any source distribution.";

const UNLICENSE_TEXT: &str = "\
This is free and unencumbered software released into the public domain.

Anyone is free to copy, modify, publish, use, compile, sell, or distribute
this software, either in source code form or as a compiled binary, for any
purpose, commercial or non-commercial, and by any means.

In jurisdictions that recognize copyright laws, the author or authors of this
software dedicate any and all copyright interest in the software to the public
domain. We make this dedication for the benefit of the public at large and to
the detriment of our heirs and successors. We intend this dedication to be an
overt act of relinquishment in perpetuity of all present and future rights to
this software under copyright law.

THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN
ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

For more information, please refer to <https://unlicense.org/>";

const BSL_TEXT: &str = "\
Boost Software License - Version 1.0 - August 17th, 2003

Permission is hereby granted, free of charge, to any person or organization
obtaining a copy of the software and accompanying documentation covered by
this license (the \"Software\") to use, reproduce, display, distribute, execute,
and transmit the Software, and to prepare derivative works of the Software, and
to permit third-parties to whom the Software is furnished to do so, all subject
to the following:

The copyright notices in the Software and this entire statement, including the
above license grant, this restriction and the following disclaimer, must be
included in all copies of the Software, in whole or in part, and all derivative
works of the Software, unless such copies or derivative works are solely in the
form of machine-executable object code generated by a source language
processor.

THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE, TITLE AND NON-INFRINGEMENT. IN NO EVENT
SHALL THE COPYRIGHT HOLDERS OR ANYONE DISTRIBUTING THE SOFTWARE BE LIABLE FOR
ANY DAMAGES OR OTHER LIABILITY, WHETHER IN CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.";

const LLVM_EXCEPTION_TEXT: &str = "\
As an exception, if, as a result of your compiling your source code, portions
of this Software are embedded into an Object form of such source code, you may
redistribute such embedded portions in such Object form without complying with
the conditions of Sections 4(a), 4(b) and 4(d) of the License.

In addition, if you combine or link compiled forms of this Software with
software that is licensed under the GPLv2 (\"Combined Software\") and if a court
of competent jurisdiction determines that the patent provision (Section 3), the
indemnity provision (Section 9) or other Section of the License conflicts with
the conditions of the GPLv2, you may retroactively and prospectively choose to
deem waived or otherwise exclude such Section(s) of the License, but only in
their entirety and only with respect to the Combined Software.";

const CC0_TEXT: &str = "\
CC0 1.0 Universal: Public Domain Dedication

The person who associated a work with this deed has dedicated the work to the
public domain by waiving all of his or her rights to the work worldwide under
copyright law, including all related and neighboring rights, to the extent
allowed by law.

You can copy, modify, distribute and perform the work, even for commercial
purposes, all without asking permission.

THE WORK IS PROVIDED \"AS IS\" WITHOUT WARRANTY OF ANY KIND. The full legal text
of CC0 1.0 is available at:
https://creativecommons.org/publicdomain/zero/1.0/legalcode";

const UNICODE_TEXT: &str = "\
UNICODE LICENSE V3

Copyright © 1991-present Unicode, Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy of
data files and any associated documentation (the \"Data Files\") or software and
any associated documentation (the \"Software\") to deal in the Data Files or
Software without restriction, including without limitation the rights to use,
copy, modify, merge, publish, distribute, and/or sell copies of the Data Files
or Software, and to permit persons to whom the Data Files or Software are
furnished to do so, provided that either (a) this copyright and permission
notice appear with all copies of the Data Files or Software, or (b) this
copyright and permission notice appear in associated Documentation.

THE DATA FILES AND SOFTWARE ARE PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT OF THIRD
PARTY RIGHTS. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR HOLDERS INCLUDED IN
THIS NOTICE BE LIABLE FOR ANY CLAIM, OR ANY SPECIAL INDIRECT OR CONSEQUENTIAL
DAMAGES, OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS,
WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING
OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THE DATA FILES OR
SOFTWARE.

The full text is available at https://www.unicode.org/license.txt";

const CDLA_TEXT: &str = "\
Community Data License Agreement – Permissive – Version 2.0

This is the Community Data License Agreement – Permissive, Version 2.0 (the
\"Agreement\"). Data Provider(s) and Data Recipient(s) agree that any Data
received under this Agreement may be used, modified, and shared, with or
without modification, for any purpose, provided that the text of this Agreement
and any attribution notices are retained.

The full legal text is available at:
https://cdla.dev/permissive-2-0/";
