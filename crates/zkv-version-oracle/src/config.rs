//! The watch list: which Docker Hub repos to poll, which zkv key prefixes to
//! publish under, which channels (latest/stable), and per-repo tag-ignore
//! patterns.
//!
//! Defaults are baked in ([`default_projects`]) so the oracle runs with zero
//! configuration; `--config <path>` replaces the list wholesale from a TOML
//! file.

use std::path::Path;

use serde::Deserialize;

/// Which release channel to publish for a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// Highest tag that parses as semver with an empty pre-release.
    Stable,
    /// Highest tag that parses as semver, including pre-releases.
    Latest,
}

impl Channel {
    /// The trailing key segment for this channel (`versions/<name>/<suffix>`).
    pub fn key_suffix(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Latest => "latest",
        }
    }
}

/// One watched project.
#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    /// Human-readable label used in log lines, e.g. `"zebra"`.
    pub name: String,
    /// Docker Hub repository as `namespace/name`, e.g. `"zfnd/zebra"`.
    pub repo: String,
    /// zkv key prefix, e.g. `"versions/zebra"`. The published key is
    /// `"<key_prefix>/<channel suffix>"`.
    pub key_prefix: String,
    /// Channels to publish for this project.
    pub channels: Vec<Channel>,
    /// Raw-tag substrings that disqualify a tag *before* semver parsing
    /// (e.g. `"-no-tls"`). Matched as a plain substring — see [`crate::select`].
    #[serde(default)]
    pub ignore: Vec<String>,
}

impl Project {
    /// The published key for `channel`.
    pub fn key_for(&self, channel: Channel) -> String {
        format!("{}/{}", self.key_prefix, channel.key_suffix())
    }
}

/// The full watch list.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub projects: Vec<Project>,
}

/// Baked-in default watch list, so the oracle runs with no config file.
pub fn default_projects() -> Vec<Project> {
    use Channel::{Latest, Stable};
    vec![
        Project {
            name: "zebra".into(),
            repo: "zfnd/zebra".into(),
            key_prefix: "versions/zebra".into(),
            channels: vec![Stable],
            ignore: vec![],
        },
        Project {
            name: "zcashd".into(),
            repo: "electriccoinco/zcashd".into(),
            key_prefix: "versions/zcashd".into(),
            channels: vec![Stable],
            ignore: vec![],
        },
        Project {
            name: "lightwalletd".into(),
            repo: "electriccoinco/lightwalletd".into(),
            key_prefix: "versions/lightwalletd".into(),
            channels: vec![Stable],
            ignore: vec![],
        },
        Project {
            name: "zallet".into(),
            repo: "electriccoinco/zallet".into(),
            key_prefix: "versions/zallet".into(),
            // stable is listed too: it auto-activates the moment a stable
            // tag exists, with no code change (see `select` returning None).
            channels: vec![Latest, Stable],
            ignore: vec![],
        },
        Project {
            name: "zaino".into(),
            repo: "zingodevops/zaino".into(),
            key_prefix: "versions/zaino".into(),
            channels: vec![Latest, Stable],
            ignore: vec!["-no-tls".into()],
        },
    ]
}

/// Load the watch list. `None` returns the baked-in defaults; `Some(path)`
/// parses a TOML file and replaces the list wholesale.
pub fn load(path: Option<&Path>) -> anyhow::Result<Config> {
    match path {
        None => Ok(Config {
            projects: default_projects(),
        }),
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|e| anyhow::anyhow!("read config {}: {e}", p.display()))?;
            let cfg: Config = toml::from_str(&text)
                .map_err(|e| anyhow::anyhow!("parse config {}: {e}", p.display()))?;
            if cfg.projects.is_empty() {
                anyhow::bail!("config {} has no [[projects]]", p.display());
            }
            Ok(cfg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_well_formed() {
        let projects = default_projects();
        assert_eq!(projects.len(), 5);
        // zaino watches both channels and ignores the -no-tls builds.
        let zaino = projects.iter().find(|p| p.name == "zaino").unwrap();
        assert!(zaino.channels.contains(&Channel::Latest));
        assert!(zaino.channels.contains(&Channel::Stable));
        assert_eq!(zaino.ignore, vec!["-no-tls".to_string()]);
        assert_eq!(zaino.key_for(Channel::Latest), "versions/zaino/latest");
        assert_eq!(zaino.key_for(Channel::Stable), "versions/zaino/stable");
    }

    #[test]
    fn parses_toml_config() {
        let toml = r#"
            [[projects]]
            name = "zebra"
            repo = "zfnd/zebra"
            key_prefix = "versions/zebra"
            channels = ["stable"]

            [[projects]]
            name = "zaino"
            repo = "zingodevops/zaino"
            key_prefix = "versions/zaino"
            channels = ["latest", "stable"]
            ignore = ["-no-tls"]
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.projects.len(), 2);
        assert_eq!(cfg.projects[0].channels, vec![Channel::Stable]);
        assert_eq!(cfg.projects[1].ignore, vec!["-no-tls".to_string()]);
    }
}
