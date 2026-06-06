use clap::{Args, ValueEnum};
use serde::Serialize;

use crate::{commands::connection_args::ConnectionCliArgs, data::resolve_db, db::Database};

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub(crate) enum OutputFormat {
    /// One matching key name per line.
    #[default]
    Friendly,
    /// Machine-readable JSON: `{ "keys": [ ... ] }`.
    Json,
}

/// List key names matching a glob pattern (Redis `KEYS`-style).
///
/// Only the `*` wildcard is supported (it matches any run of characters,
/// including none); a backslash escapes it for a literal match (`\*` matches a
/// literal asterisk, `\\` a literal backslash). Every other character
/// (including `?`, `[`, `]`) is matched literally, and matching is
/// case-sensitive. The default pattern is `*` (every key).
#[derive(Debug, Args)]
pub(crate) struct Command {
    /// Glob pattern; `*` is the only wildcard. Quote it to stop your shell
    /// expanding it. Defaults to every key.
    #[arg(default_value = "*")]
    pattern: String,

    #[command(flatten)]
    connection: ConnectionCliArgs,

    /// Don't sync first; match over last-known local state.
    #[arg(long)]
    offline: bool,

    /// Minimum confirmations for a key's value to count as present.
    #[arg(short = 'c', long, default_value_t = 3)]
    confirmations: u32,

    /// Output format. `friendly` (default) is one key per line on stdout;
    /// `json` emits `{ "keys": [...] }`.
    #[arg(long, value_enum, default_value_t = OutputFormat::Friendly)]
    output: OutputFormat,
}

impl Command {
    pub(crate) async fn run(self, db: Option<String>) -> anyhow::Result<()> {
        let name = resolve_db(db.as_deref())?;
        let connection = self.connection.into_inner();
        let database = Database::open(&name, connection)?;

        if !self.offline {
            database.sync().await?;
        }

        // Pure-local read at the requested depth (merges pending.toml).
        let result = database.read(self.confirmations)?;

        // A key is "present" once it has a confirmed value. The state map is
        // a BTreeMap, so iteration is already sorted; keep matching names.
        let matches: Vec<&String> = result
            .state
            .iter()
            .filter(|(_, ks)| ks.confirmed.is_some())
            .map(|(k, _)| k)
            .filter(|k| glob_match(&self.pattern, k))
            .collect();

        match self.output {
            OutputFormat::Friendly => {
                if matches.is_empty() {
                    eprintln!("(no keys match {:?})", self.pattern);
                } else {
                    for k in matches {
                        println!("{k}");
                    }
                }
            }
            OutputFormat::Json => {
                println!("{}", serde_json::to_string(&KeysJson { keys: matches })?);
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct KeysJson<'a> {
    keys: Vec<&'a String>,
}

/// Match `text` against a Redis-style glob `pattern` supporting only the `*`
/// wildcard (any run of characters, including empty). A backslash escapes the
/// next character so it matches literally: `\*` matches a literal asterisk
/// and `\\` a literal backslash; a lone trailing backslash matches a literal
/// backslash. Every other character (including `?`, `[`, `]`) is matched
/// literally. The whole text must match (anchored), and matching is
/// case-sensitive, like Redis `KEYS`.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();

    // Two-pointer glob with backtracking to the most recent `*`. `star` holds
    // (pattern index just past the last `*`, text index that `*` is currently
    // anchored at); on a later mismatch we let the `*` absorb one more char.
    let mut pi = 0usize;
    let mut ti = 0usize;
    let mut star: Option<(usize, usize)> = None;

    loop {
        if ti < t.len() {
            match p.get(pi) {
                Some('*') => {
                    star = Some((pi + 1, ti));
                    pi += 1;
                    continue;
                }
                Some(&pc) => {
                    // Literal token: an escape (`\x`) consumes two pattern
                    // chars and matches `x`; a lone trailing `\` matches `\`.
                    let (lit, width) = if pc == '\\' {
                        match p.get(pi + 1) {
                            Some(&esc) => (esc, 2),
                            None => ('\\', 1),
                        }
                    } else {
                        (pc, 1)
                    };
                    if t[ti] == lit {
                        pi += width;
                        ti += 1;
                        continue;
                    }
                    // else: fall through to backtrack
                }
                None => { /* pattern exhausted, text remains: backtrack */ }
            }
            // Mismatch (or pattern ran out with text left): resume from the
            // last `*`, extending what it absorbed by one more character.
            match star {
                Some((spi, sti)) => {
                    pi = spi;
                    ti = sti + 1;
                    star = Some((spi, sti + 1));
                }
                None => return false,
            }
        } else {
            // Text consumed: the rest of the pattern must be only `*`.
            while p.get(pi) == Some(&'*') {
                pi += 1;
            }
            return pi == p.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn star_matches_everything() {
        assert!(glob_match("*", ""));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*", "with spaces ? and [brackets]"));
    }

    #[test]
    fn prefix_suffix_infix() {
        assert!(glob_match("foo*", "foo"));
        assert!(glob_match("foo*", "foobar"));
        assert!(!glob_match("foo*", "fobar"));
        assert!(!glob_match("foo*", "xfoo"));

        assert!(glob_match("*bar", "bar"));
        assert!(glob_match("*bar", "foobar"));
        assert!(!glob_match("*bar", "barx"));

        assert!(glob_match("foo*bar", "foobar"));
        assert!(glob_match("foo*bar", "foo_X_bar"));
        assert!(!glob_match("foo*bar", "foobaz"));
    }

    #[test]
    fn multiple_stars() {
        assert!(glob_match("*a*", "a"));
        assert!(glob_match("*a*", "xay"));
        assert!(glob_match("a*b*c", "abc"));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b*c", "aXXc"));
        // adjacent stars collapse to one
        assert!(glob_match("a**b", "ab"));
        assert!(glob_match("a**b", "aZZZb"));
    }

    #[test]
    fn exact_match_without_star() {
        assert!(glob_match("foo", "foo"));
        assert!(!glob_match("foo", "foobar"));
        assert!(!glob_match("foo", "fo"));
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
    }

    #[test]
    fn escaped_star_matches_literal_asterisk() {
        assert!(glob_match(r"foo\*", "foo*"));
        assert!(!glob_match(r"foo\*", "foobar"));
        assert!(glob_match(r"\*", "*"));
        assert!(!glob_match(r"\*", "x"));
        assert!(glob_match(r"a\*b", "a*b"));
        assert!(!glob_match(r"a\*b", "aXb"));
        // an escaped star is literal, so it must NOT also act as a wildcard
        assert!(!glob_match(r"a\*", "axyz"));
    }

    #[test]
    fn escaped_backslash() {
        // `\\` in the pattern is one literal backslash.
        assert!(glob_match(r"a\\b", r"a\b"));
        assert!(!glob_match(r"a\\b", "ab"));
        // a lone trailing backslash matches a literal backslash
        assert!(glob_match("\\", "\\"));
    }

    #[test]
    fn other_glob_chars_are_literal() {
        // We deliberately support ONLY `*`; `?`, `[`, `]` are literal.
        assert!(glob_match("a?b", "a?b"));
        assert!(!glob_match("a?b", "axb"));
        assert!(glob_match("a[bc]d", "a[bc]d"));
        assert!(!glob_match("a[bc]d", "abd"));
    }

    #[test]
    fn case_sensitive() {
        assert!(!glob_match("Foo*", "foobar"));
        assert!(glob_match("Foo*", "Foobar"));
    }

    #[test]
    fn unicode() {
        assert!(glob_match("café*", "café_au_lait"));
        assert!(glob_match("*☃*", "snow☃man"));
        assert!(!glob_match("é*", "e"));
    }
}
