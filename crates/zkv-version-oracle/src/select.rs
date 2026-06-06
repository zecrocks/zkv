//! Pure, network-free version selection: turn a repo's raw Docker Hub tag list
//! into the single version to publish for a channel.
//!
//! Pipeline: drop tags containing any ignore substring (matched on the *raw*
//! tag, before parsing) -> strip an optional leading `v` -> parse semver (drop
//! anything that isn't semver, e.g. `latest`, `sha-…`) -> for `stable` keep
//! only non-pre-release versions -> take the maximum.

use semver::Version;

use crate::config::Channel;

/// Strip an optional single leading `v`/`V`, then parse as semver. Returns
/// `None` for tags that aren't semver (`latest`, `sha-abc123`, `main`, …).
fn parse_tag(tag: &str) -> Option<Version> {
    let stripped = tag
        .strip_prefix('v')
        .or_else(|| tag.strip_prefix('V'))
        .unwrap_or(tag);
    Version::parse(stripped).ok()
}

/// Select the version to publish for `channel` from `tags`.
///
/// `ignore` is matched as a **plain substring** against the raw tag (no regex
/// dependency; nothing in the requirements needs anchoring or character
/// classes). This raw-tag filter is load-bearing: `0.4.0-rc.2-no-tls` *is*
/// valid semver and sorts **above** `0.4.0-rc.2` — per the semver spec a
/// numeric pre-release identifier (`2`) ranks below an alphanumeric one
/// (`2-no-tls`) — so without dropping `-no-tls` here, "latest" would pick the
/// `-no-tls` build.
///
/// Returns `None` when nothing qualifies — notably `stable` when only
/// pre-releases exist (the caller then skips that key).
pub fn select(tags: &[String], channel: Channel, ignore: &[String]) -> Option<Version> {
    tags.iter()
        .filter(|t| {
            !ignore
                .iter()
                .any(|pat| !pat.is_empty() && t.contains(pat.as_str()))
        })
        .filter_map(|t| parse_tag(t))
        .filter(|v| match channel {
            Channel::Latest => true,
            Channel::Stable => v.pre.is_empty(),
        })
        .max()
}

/// The canonical on-chain value for a selected version: `Version::to_string()`,
/// e.g. `"5.0.0"` or `"0.4.0-rc.2"` (no `v` prefix).
pub fn value_string(v: &Version) -> String {
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Channel::{Latest, Stable};

    fn tags(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn stable_picks_highest_non_pre() {
        let t = tags("1.0.0 1.2.0 1.2.1 2.0.0-rc.1");
        assert_eq!(select(&t, Stable, &[]).unwrap().to_string(), "1.2.1");
    }

    #[test]
    fn latest_picks_highest_incl_pre() {
        let t = tags("1.2.1 2.0.0-rc.1");
        assert_eq!(select(&t, Latest, &[]).unwrap().to_string(), "2.0.0-rc.1");
    }

    #[test]
    fn strips_leading_v() {
        let t = tags("v0.4.0 v0.4.1 latest");
        assert_eq!(select(&t, Stable, &[]).unwrap().to_string(), "0.4.1");
    }

    #[test]
    fn drops_non_semver_tags() {
        let t = tags("latest sha-deadbeef main 5.0.0 nightly");
        assert_eq!(select(&t, Latest, &[]).unwrap().to_string(), "5.0.0");
    }

    #[test]
    fn stable_is_none_when_only_prereleases() {
        // zaino today: only release candidates exist.
        let t = tags("0.4.0-rc.1 0.4.0-rc.2");
        assert!(select(&t, Stable, &[]).is_none());
    }

    #[test]
    fn latest_works_when_only_prereleases() {
        let t = tags("0.4.0-rc.1 0.4.0-rc.2");
        assert_eq!(select(&t, Latest, &[]).unwrap().to_string(), "0.4.0-rc.2");
    }

    #[test]
    fn no_tls_sorts_above_plain_rc() {
        // Proves the ignore filter is necessary, not cosmetic.
        let with = Version::parse("0.4.0-rc.2-no-tls").unwrap();
        let without = Version::parse("0.4.0-rc.2").unwrap();
        assert!(with > without);
    }

    #[test]
    fn no_tls_ignored_for_latest() {
        let t = tags("0.4.0-rc.2 0.4.0-rc.2-no-tls");
        let ig = vec!["-no-tls".to_string()];
        assert_eq!(select(&t, Latest, &ig).unwrap().to_string(), "0.4.0-rc.2");
    }

    #[test]
    fn no_tls_filter_keeps_higher_clean_tag() {
        let t = tags("0.4.0-rc.2 0.4.0-rc.3 0.4.0-rc.3-no-tls");
        let ig = vec!["-no-tls".to_string()];
        assert_eq!(select(&t, Latest, &ig).unwrap().to_string(), "0.4.0-rc.3");
    }

    #[test]
    fn lightwalletd_v_prefixed_stable() {
        let t = tags("v0.4.16 v0.4.17 latest");
        assert_eq!(select(&t, Stable, &[]).unwrap().to_string(), "0.4.17");
    }

    #[test]
    fn empty_input_is_none() {
        assert!(select(&[], Latest, &[]).is_none());
        assert!(select(&[], Stable, &[]).is_none());
    }
}
