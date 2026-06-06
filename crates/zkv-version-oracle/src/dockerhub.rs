//! Minimal Docker Hub tag lister.
//!
//! Hits the public registry API (`/v2/repositories/<repo>/tags`) and follows
//! pagination, returning every tag name. No auth: anonymous listing is enough
//! for public images and stays well under Docker Hub's rate limits at the
//! oracle's polling cadence.

use serde::Deserialize;

/// Default Docker Hub API base. Injectable so tests can point elsewhere.
pub const DEFAULT_BASE: &str = "https://hub.docker.com/v2";

const PAGE_SIZE: u32 = 100;
/// Hard stop on pagination (100 tags/page), so a pathological repo or a broken
/// `next` chain can't loop forever. 50 pages = 5000 tags, far beyond any
/// watched repo.
const MAX_PAGES: u32 = 50;

#[derive(Debug, Deserialize)]
struct TagsPage {
    next: Option<String>,
    results: Vec<TagResult>,
}

#[derive(Debug, Deserialize)]
struct TagResult {
    name: String,
}

/// Build the shared HTTP client: a descriptive user-agent (Docker Hub is
/// friendlier to identified clients) and a per-request timeout so one hung
/// connection can't stall a poll tick.
pub fn build_client() -> anyhow::Result<reqwest::Client> {
    // reqwest is built with `rustls-no-provider`, so it uses the process-wide
    // default rustls `CryptoProvider`. Install ring (matching the lightwalletd
    // TLS stack) instead of pulling in aws-lc-rs. Ignore the error: a non-empty
    // result just means a provider was already installed, which is fine.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .user_agent(concat!("zkv-version-oracle/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| anyhow::anyhow!("build http client: {e}"))
}

/// Fetch every tag name for `repo` (`namespace/name`), following pagination.
pub async fn fetch_tags(
    client: &reqwest::Client,
    base: &str,
    repo: &str,
) -> anyhow::Result<Vec<String>> {
    let mut url = format!("{base}/repositories/{repo}/tags?page_size={PAGE_SIZE}");
    let mut out = Vec::new();
    for _ in 0..MAX_PAGES {
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("GET {url}: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("Docker Hub returned {status} for repo {repo:?}");
        }
        let page: TagsPage = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("decode tags page for {repo:?}: {e}"))?;
        out.extend(page.results.into_iter().map(|t| t.name));
        match page.next {
            // Docker Hub returns `next` as an absolute URL; follow it verbatim.
            Some(next) if !next.is_empty() => url = next,
            _ => return Ok(out),
        }
    }
    anyhow::bail!("repo {repo:?} exceeded {MAX_PAGES} pages of tags")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tags_page_fixture() {
        // Locks the field names we depend on (`next`, `results[].name`) and
        // that unknown fields (`count`, `full_size`, …) are ignored.
        let json = r#"{
            "count": 2,
            "next": "https://hub.docker.com/v2/repositories/zfnd/zebra/tags?page=2",
            "previous": null,
            "results": [
                {"name": "5.0.0", "full_size": 1},
                {"name": "latest", "full_size": 2}
            ]
        }"#;
        let page: TagsPage = serde_json::from_str(json).unwrap();
        assert_eq!(
            page.next.as_deref(),
            Some("https://hub.docker.com/v2/repositories/zfnd/zebra/tags?page=2")
        );
        let names: Vec<_> = page.results.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["5.0.0", "latest"]);
    }

    #[test]
    fn last_page_has_null_next() {
        let json = r#"{ "next": null, "results": [{"name": "1.0.0"}] }"#;
        let page: TagsPage = serde_json::from_str(json).unwrap();
        assert!(page.next.is_none());
    }
}
