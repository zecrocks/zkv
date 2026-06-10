use super::*;

/// Canonical memo text up to (but not including) the signature line.
///
/// For `SET`/`DEL`/`INIT` and the registry-management ops this is the single
/// header line `ZKV0 <OP> <key> [value]`. For `SETL` it is the two-line,
/// length-framed body `ZKV0 SETL <key> <byte_len>\n<value bytes>`; callers
/// append the signature after a further `\n`, so the value stays fully
/// delimited even when empty or containing newlines.
///
/// Total over every `(op, value)` combination so it can back both
/// [`build_memo`] (which validates the pairing first) and
/// [`render_memo_text`] (which reconstructs a memo from already-validated
/// stored fields). For INIT, `key` is the embedded zkv_addr and `value` is the
/// optional reserved-tokens string; for the management ops `key` is the
/// target's canonical `zkvid1…` pubkey and (for WRITERADD) `value` is the scope.
fn memo_line1(op: Op, key: &str, value: Option<&str>) -> String {
    match op {
        Op::Set => match value {
            Some(v) => format!("{WIRE_MAGIC} SET {key} {v}"),
            None => format!("{WIRE_MAGIC} SET {key}"),
        },
        // A missing SETL value is unreachable via `build_memo` (it bails
        // first); render it as a zero-length frame to keep this total.
        Op::SetL => match value {
            Some(v) => format!("{WIRE_MAGIC} SETL {key} {}\n{v}", v.len()),
            None => format!("{WIRE_MAGIC} SETL {key} 0\n"),
        },
        Op::Del => format!("{WIRE_MAGIC} DEL {key}"),
        Op::Init => match value {
            Some(v) => format!("{WIRE_MAGIC} INIT {key} {v}"),
            None => format!("{WIRE_MAGIC} INIT {key}"),
        },
        // Registry-management ops. WRITERADD carries the scope as its value;
        // the others carry no value (a missing WRITERADD value is unreachable
        // via `build_memo`, which bails first).
        Op::OwnerAdd => format!("{WIRE_MAGIC} OWNERADD {key}"),
        Op::OwnerDel => format!("{WIRE_MAGIC} OWNERDEL {key}"),
        Op::WriterAdd => match value {
            Some(v) => format!("{WIRE_MAGIC} WRITERADD {key} {v}"),
            None => format!("{WIRE_MAGIC} WRITERADD {key}"),
        },
        Op::WriterDel => format!("{WIRE_MAGIC} WRITERDEL {key}"),
        // FINALIZE is header-only: no key, no value.
        Op::Finalize => format!("{WIRE_MAGIC} FINALIZE"),
        // VERSION carries the block-flags token as its value (a missing value
        // is unreachable via `build_memo`, which bails first).
        Op::Version => match value {
            Some(v) => format!("{WIRE_MAGIC} VERSION {key} {v}"),
            None => format!("{WIRE_MAGIC} VERSION {key}"),
        },
    }
}

/// Render the canonical wire-format memo text for a command plus its
/// signature hex. Inverse of [`parse_text_memo`] for well-formed inputs;
/// produces the same text [`build_memo`] would. Used to reconstruct the
/// raw on-chain memo for history entries read back from the snapshot,
/// where only `(op, key, value, sig_hex)` are stored.
pub fn render_memo_text(op: Op, key: &str, value: Option<&str>, seq: u64, sig_hex: &str) -> String {
    render_memo_with_comment(op, key, value, seq, sig_hex, None)
}

/// [`render_memo_text`] with an optional first-line comment (`#…`). When
/// `comment` is `Some`, a `#<comment>\n` line is prepended above the `ZKV0`
/// header; when `None`, the output is byte-identical to [`render_memo_text`].
pub fn render_memo_with_comment(
    op: Op,
    key: &str,
    value: Option<&str>,
    seq: u64,
    sig_hex: &str,
    comment: Option<&str>,
) -> String {
    format!(
        "{}{}\n{}",
        comment_line(comment),
        memo_line1(op, key, value),
        encode_sig_line(seq, sig_hex)
    )
}

/// The leading comment line, terminated by its newline, or the empty string
/// when there is no comment. Inverse of the comment strip in
/// [`parse_text_memo_detailed`].
fn comment_line(comment: Option<&str>) -> String {
    match comment {
        Some(c) => format!("#{c}\n"),
        None => String::new(),
    }
}

/// Build a two-line text memo for a zkv command. `sig` is the 65-byte
/// recoverable signature returned by [`sign_command`]; `seq` is the
/// per-entity replay-protection sequence the writer signed over (0 for
/// INIT/VERSION and first writes; see [`encode_sig_line`]).
pub fn build_memo(
    op: Op,
    key: &str,
    value: Option<&str>,
    seq: u64,
    sig: &[u8; SIG_LEN],
) -> anyhow::Result<MemoBytes> {
    build_memo_with_comment(op, key, value, seq, sig, None)
}

/// [`build_memo`] with an optional first-line comment (`#…`). The comment, when
/// present, is emitted as a `#<comment>\n` line above the `ZKV0` header and is
/// covered by the signature (the caller must have signed over the payload from
/// [`payload_for`] / [`bind_comment`] for the same comment). A comment must not
/// contain a newline; it is, by definition, a single line.
pub fn build_memo_with_comment(
    op: Op,
    key: &str,
    value: Option<&str>,
    seq: u64,
    sig: &[u8; SIG_LEN],
    comment: Option<&str>,
) -> anyhow::Result<MemoBytes> {
    if key.contains(char::is_whitespace) {
        bail!("zkv keys must not contain whitespace");
    }
    // Reject control characters (NUL included) so the NUL-delimited
    // `signed_payload` stays injective across the key/value boundary: a key
    // carrying a NUL could otherwise be re-split into a different (key, value)
    // that the same signature also covers. (Whitespace is already rejected
    // above; this additionally bars the non-whitespace control bytes.)
    if key.contains(char::is_control) {
        bail!("zkv keys must not contain control characters");
    }
    if let Some(c) = comment {
        if c.contains('\n') {
            bail!("zkv comments must not contain newlines (a comment is a single line)");
        }
    }
    // `Op::Set` is the compact trailing-token form: it can't represent
    // newlines (the parser splits on \n) and can't represent an empty
    // value (it would encode as a trailing space before the \n and any
    // whitespace-stripping transport silently drops it). `Op::SetL` is
    // the escape hatch for both. Other ops don't carry user-supplied
    // values, so their value strings (if any) are protocol-controlled.
    if let (Op::Set, Some(v)) = (op, value) {
        if v.contains('\n') {
            bail!("zkv SET values must not contain newlines (use SETL for framed values)");
        }
        if v.is_empty() {
            bail!("zkv SET values must not be empty (use SETL for framed values)");
        }
    }
    match (op, value) {
        (Op::Set, None) => bail!("SET requires a value"),
        (Op::SetL, None) => bail!("SETL requires a value"),
        (Op::Del, Some(_)) => bail!("DEL takes no value"),
        (Op::OwnerAdd, Some(_)) => bail!("OWNERADD takes no value"),
        (Op::OwnerDel, Some(_)) => bail!("OWNERDEL takes no value"),
        (Op::WriterAdd, None) => bail!("WRITERADD requires a scope value"),
        (Op::WriterDel, Some(_)) => bail!("WRITERDEL takes no value"),
        (Op::Finalize, Some(_)) => bail!("FINALIZE takes no value"),
        (Op::Version, None) => bail!("VERSION requires a block-flags value"),
        _ => {}
    }

    let line1 = memo_line1(op, key, value);
    let text = format!(
        "{}{line1}\n{}",
        comment_line(comment),
        encode_sig_line(seq, &hex::encode(sig))
    );
    let memo = Memo::from_str(&text)
        .map_err(|e| anyhow!("zkv memo too large for a Zcash text memo: {e}"))?;
    Ok(MemoBytes::from(memo))
}

#[derive(Debug, PartialEq, Eq)]
pub struct ZkvCommand {
    pub op: Op,
    pub key: String,
    pub value: Option<String>,
    /// The per-entity replay-protection sequence the writer signed over,
    /// decoded from the compact prefix on the signature line (0 if absent /
    /// non-versioned). The reader rebuilds the signing domain from this and
    /// compares it against the entity's current version (see
    /// [`signing_domain`] / [`encode_sig_line`]).
    pub seq: u64,
    pub sig_hex: String,
    /// An optional first-line comment carried above the `ZKV0` header (the
    /// `#…` line). `None` when the memo had no comment; `Some(text)` holds the
    /// comment body verbatim (the leading `#` and trailing newline stripped).
    /// The comment is **signed**: it is folded into the signing domain via
    /// [`bind_comment`], so it round-trips and cannot be added, altered, or
    /// stripped without invalidating the recovered signer.
    pub comment: Option<String>,
}

/// Parse a `Memo::Text` payload into a zkv command, if it is one.
///
/// Wire forms (`<sig>` is the 130-char hex of a 65-byte recoverable
/// signature; see [`SIG_LEN`]):
///
/// - `SET` / `DEL` / `INIT` / `OWNERADD` / `OWNERDEL` / `WRITERADD` /
///   `WRITERDEL`: `"ZKV0 OP KEY [VALUE]\n<sig>"`. Some broadcaster wallets
///   normalize newlines into whitespace; for these ops we fall back to taking
///   the trailing hex run as the signature. For `OWNER*`/`WRITER*`, `KEY` is
///   the target's canonical `zkvid1…` pubkey and `VALUE` (WRITERADD only) is
///   the capability scope.
/// - `FINALIZE`: `"ZKV0 FINALIZE\n<sig>"` (header-only, carrying no key and
///   no value; the same collapsed-newline fallback applies).
/// - `SETL`: `"ZKV0 SETL KEY <byte_len>\n<value bytes>\n<sig>"`.
///   The value is read by exact byte count and may contain any UTF-8 (including
///   newlines and zero bytes of content). The collapsed-newline fallback is
///   *not* available for `SETL`; length-prefix framing can't survive
///   whitespace mangling.
pub fn parse_text_memo(text: &str) -> Option<ZkvCommand> {
    parse_text_memo_detailed(text).ok()
}

/// Why a candidate memo did not parse. Distinguishes "not a zkv memo at all"
/// (foreign traffic; the history view filters these out) from a structurally
/// broken zkv memo (someone tried to write and got the wire form wrong).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoReject {
    /// No `ZKV0` magic prefix; not a zkv command at all.
    NotZkv,
    /// `ZKV0`-prefixed but structurally invalid; carries the precise cause.
    Malformed(MemoFormat),
    /// A zkv memo from a *newer* protocol version than this build supports
    /// (the magic is `ZKV<n>` with `n` > [`ZKV_VERSION`]). Carries `n`.
    UnsupportedVersion(u32),
}

/// Parse a `Memo::Text` payload into a zkv command, reporting *why* it failed.
///
/// [`parse_text_memo`] is the `Option`-returning shim over this for callers
/// that don't need the reason. The form discriminator is the newline: a memo
/// containing `\n` is parsed as the two-section form (and that path's error is
/// reported); otherwise the newline-collapsed fallback is tried.
pub fn parse_text_memo_detailed(text: &str) -> Result<ZkvCommand, MemoReject> {
    // Optional first-line comment: `#<comment>\n` above the `ZKV0` header. The
    // comment ends at the first newline (it is inherently single-line) and is
    // bound into the signature (see [`bind_comment`]). A `#`-led memo with no
    // terminating newline carries no command at all; treat it as foreign.
    let (comment, body) = match text.strip_prefix('#') {
        Some(after_hash) => match after_hash.split_once('\n') {
            Some((c, rest)) => (Some(c.to_owned()), rest),
            None => return Err(MemoReject::NotZkv),
        },
        None => (None, text),
    };
    // Two-section form (preferred): header line, then a body that depends on op.
    let mut cmd = if let Some((line1, rest)) = body.split_once('\n') {
        parse_two_section(line1, rest)?
    } else {
        // No newline left; fallback for broadcaster wallets that flatten the
        // memo to a single line. `SETL` is rejected here on principle (above).
        parse_collapsed(body)?
    };
    cmd.comment = comment;
    Ok(cmd)
}

/// True if a memo's text begins with the ZKV0 wire marker, i.e. its first
/// whitespace-delimited token is exactly `WIRE_MAGIC` (`"ZKV0"`).
///
/// Unlike [`parse_text_memo`], this is a *prefix* test, not a parse: it matches
/// malformed or unknown-opcode ZKV0 memos (e.g. `"ZKV0 BOGUS …"`, or a bare
/// `"ZKV0"`) as well as valid ones, and it deliberately does **not** match a
/// token that merely starts with those bytes like `"ZKV0234 …"`. Used to
/// exclude *all* zkv protocol traffic (valid or not) from views that only want
/// non-zkv transactions, such as the GUI's funding tab.
pub fn looks_like_zkv(text: &str) -> bool {
    // Skip an optional first-line comment (`#…\n`) so a commented zkv memo is
    // still recognized as zkv traffic (mirrors the comment strip in
    // [`parse_text_memo_detailed`]).
    let body = match text.strip_prefix('#') {
        Some(after_hash) => match after_hash.split_once('\n') {
            Some((_, rest)) => rest,
            None => return false,
        },
        None => text,
    };
    body.split_whitespace().next() == Some(WIRE_MAGIC)
}

/// The trailing signature line for the two-section header-only / `SET` /
/// `WRITERADD` / `INIT` forms: `rest` (trimmed) is the compact `(seq, sig)`
/// blob: any leading big-endian sequence prefix followed by the fixed
/// [`SIG_HEX_LEN`]-char signature (see [`parse_sig_line`]).
fn parse_trailing_sig(rest: &str) -> Result<(u64, String), MemoReject> {
    if rest.trim().is_empty() {
        return Err(MemoReject::Malformed(MemoFormat::MissingSignature));
    }
    parse_sig_line(rest)
}

/// Inspect a candidate magic token (`ZKV<n>`). `Ok(())` if it's exactly this
/// build's version; `Err(UnsupportedVersion)` if it's a newer zkv version;
/// `Err(NotZkv)` if it isn't a zkv magic at all (or an older/unknown version
/// we never emitted).
fn check_magic(tok: Option<&str>) -> Result<(), MemoReject> {
    let Some(ver) = tok
        .and_then(|t| t.strip_prefix(MAGIC_PREFIX))
        .and_then(|s| s.parse::<u32>().ok())
    else {
        return Err(MemoReject::NotZkv);
    };
    if ver == ZKV_VERSION {
        Ok(())
    } else if ver > ZKV_VERSION {
        Err(MemoReject::UnsupportedVersion(ver))
    } else {
        Err(MemoReject::NotZkv)
    }
}

/// Validate a parsed key: non-empty and free of control characters (NUL
/// included). The control-char rule keeps [`signed_payload`] injective: its
/// fields are NUL-delimited and `value` is the sole unbounded trailing field, so
/// a key carrying a NUL (or any control byte) could otherwise re-split the
/// `key`/`value` boundary and let one signature serve two distinct commands.
/// Whitespace can't reach a parsed key (it is the token delimiter), so this is
/// only reached by a hand-crafted memo.
fn validate_parsed_key(key: &str) -> Result<(), MemoReject> {
    if key.is_empty() {
        return Err(MemoReject::Malformed(MemoFormat::EmptyKey));
    }
    if key.chars().any(|c| c.is_control()) {
        return Err(MemoReject::Malformed(MemoFormat::ControlCharInKey));
    }
    Ok(())
}

fn parse_two_section(line1: &str, rest: &str) -> Result<ZkvCommand, MemoReject> {
    let mut tokens = line1.splitn(4, ' ');
    check_magic(tokens.next())?;
    let op = tokens
        .next()
        .and_then(Op::parse)
        .ok_or(MemoReject::Malformed(MemoFormat::UnknownOpcode))?;
    // FINALIZE is header-only (no key, no value). Handle it before the
    // empty-key guard that every other op requires.
    if op == Op::Finalize {
        if tokens.next().is_some() {
            return Err(MemoReject::Malformed(MemoFormat::WrongArity { op }));
        }
        let (seq, sig_hex) = parse_trailing_sig(rest)?;
        return Ok(ZkvCommand {
            op,
            key: String::new(),
            value: None,
            seq,
            sig_hex,
            comment: None,
        });
    }
    let key = tokens.next().unwrap_or("").to_owned();
    validate_parsed_key(&key)?;

    let (value, seq, sig_hex) = match op {
        Op::SetL => {
            let len_tok = tokens
                .next()
                .ok_or(MemoReject::Malformed(MemoFormat::WrongArity { op }))?;
            // No trailing tokens allowed past the length.
            if tokens.next().is_some() {
                return Err(MemoReject::Malformed(MemoFormat::WrongArity { op }));
            }
            let (value, seq, sig) = parse_setl_body(rest, len_tok)?;
            (value, seq, sig)
        }
        Op::Set => {
            // Reject empty values: a trailing-space encoding ("ZKV0 SET k ")
            // is ambiguous with whitespace mangling, so we drop it rather
            // than admit a value the writer might not have intended. (Empty
            // values belong in SETL.)
            let v = tokens
                .next()
                .ok_or(MemoReject::Malformed(MemoFormat::MissingValue))?;
            if v.is_empty() {
                return Err(MemoReject::Malformed(MemoFormat::MissingValue));
            }
            let (seq, sig) = parse_trailing_sig(rest)?;
            (Some(v.to_owned()), seq, sig)
        }
        Op::Del | Op::OwnerAdd | Op::OwnerDel | Op::WriterDel => {
            // Header-only ops: `key` (target pubkey for OWNER*/WRITER*) and
            // nothing past it. Any trailing token is a protocol violation.
            if tokens.next().is_some() {
                return Err(MemoReject::Malformed(MemoFormat::WrongArity { op }));
            }
            let (seq, sig) = parse_trailing_sig(rest)?;
            (None, seq, sig)
        }
        // WRITERADD carries a non-empty scope value as its 4th token; VERSION
        // carries its block-flags token in the same position. The token's
        // *content* is validated later (Scope::parse / BlockSet::parse); here
        // we only require it to be present and non-empty.
        Op::WriterAdd => {
            let v = tokens
                .next()
                .ok_or(MemoReject::Malformed(MemoFormat::MissingScope))?;
            if v.is_empty() {
                return Err(MemoReject::Malformed(MemoFormat::MissingScope));
            }
            let (seq, sig) = parse_trailing_sig(rest)?;
            (Some(v.to_owned()), seq, sig)
        }
        Op::Version => {
            let v = tokens
                .next()
                .ok_or(MemoReject::Malformed(MemoFormat::MissingVersionFlag))?;
            if v.is_empty() {
                return Err(MemoReject::Malformed(MemoFormat::MissingVersionFlag));
            }
            let (seq, sig) = parse_trailing_sig(rest)?;
            (Some(v.to_owned()), seq, sig)
        }
        // INIT: trailing tokens (reserved for future config) are optional.
        Op::Init => {
            let echo = tokens.next().map(|s| s.to_owned());
            let (seq, sig) = parse_trailing_sig(rest)?;
            (echo, seq, sig)
        }
        // FINALIZE returned early above (it has no key to reach this match).
        Op::Finalize => unreachable!("FINALIZE is handled before the key parse"),
    };

    Ok(ZkvCommand {
        op,
        key,
        value,
        seq,
        sig_hex,
        comment: None,
    })
}

/// Slice the `SETL` body into `(value, seq, sig_hex)`. The body is exactly
/// `<byte_len>` bytes of value, then `\n`, then the compact signature line
/// (an optional big-endian sequence prefix followed by the fixed
/// [`SIG_HEX_LEN`]-char signature; trailing whitespace tolerated for transport
/// robustness, but the hex run itself must be intact). A `byte_len` of `0` is
/// valid; it is the canonical empty-value encoding.
fn parse_setl_body(rest: &str, len_tok: &str) -> Result<(Option<String>, u64, String), MemoReject> {
    let byte_len: usize = len_tok
        .parse()
        .map_err(|_| MemoReject::Malformed(MemoFormat::SetlNonNumericLength))?;
    let rest_bytes = rest.as_bytes();
    if rest_bytes.len() < byte_len {
        return Err(MemoReject::Malformed(MemoFormat::SetlLengthOverrun));
    }
    let (value_bytes, after) = rest_bytes.split_at(byte_len);
    // Reject mid-codepoint splits: `Memo::Text` is UTF-8, but a forged memo
    // could declare a `byte_len` that lands mid-character.
    let value = std::str::from_utf8(value_bytes)
        .map_err(|_| MemoReject::Malformed(MemoFormat::SetlValueNotUtf8))?;
    // The byte after the value must be the `\n` separator.
    let (sep, sig_region) = after
        .split_first()
        .ok_or(MemoReject::Malformed(MemoFormat::SetlMissingSeparator))?;
    if *sep != b'\n' {
        return Err(MemoReject::Malformed(MemoFormat::SetlMissingSeparator));
    }
    let sig_str = std::str::from_utf8(sig_region)
        .map_err(|_| MemoReject::Malformed(MemoFormat::BadSignatureFraming))?
        .trim();
    if sig_str.is_empty() {
        return Err(MemoReject::Malformed(MemoFormat::MissingSignature));
    }
    let (seq, sig_hex) = parse_sig_line(sig_str)?;
    Ok((Some(value.to_owned()), seq, sig_hex))
}

fn parse_collapsed(text: &str) -> Result<ZkvCommand, MemoReject> {
    let trimmed = text.trim_end();
    // Family/version gate on the leading token first, so a newer-version memo
    // reports UnsupportedVersion rather than being mistaken for non-zkv or
    // short-circuited by the signature-framing checks below.
    check_magic(trimmed.split(' ').next())?;
    // The signature line is the final whitespace-delimited token: an optional
    // big-endian sequence prefix folded onto the fixed-length signature (see
    // [`encode_sig_line`]). The header is everything before it. Splitting on the
    // last space (rather than a fixed offset) is what lets the variable-width
    // sequence prefix survive a newline-collapsing transport.
    let (head, tail) = trimmed
        .rsplit_once(|c: char| c.is_ascii_whitespace())
        .ok_or(MemoReject::Malformed(MemoFormat::MissingSignature))?;
    let (seq, sig_hex) = parse_sig_line(tail)?;
    let line1 = head.trim_end();

    let mut tokens = line1.splitn(4, ' ');
    check_magic(tokens.next())?;
    let op = tokens
        .next()
        .and_then(Op::parse)
        .ok_or(MemoReject::Malformed(MemoFormat::UnknownOpcode))?;
    // FINALIZE is header-only (no key, no value; see `parse_two_section`).
    if op == Op::Finalize {
        if tokens.next().is_some() {
            return Err(MemoReject::Malformed(MemoFormat::WrongArity { op }));
        }
        return Ok(ZkvCommand {
            op,
            key: String::new(),
            value: None,
            seq,
            sig_hex,
            comment: None,
        });
    }
    let key = tokens.next().unwrap_or("").to_owned();
    validate_parsed_key(&key)?;
    let value = match op {
        // SETL cannot be recovered from a collapsed memo: the length-prefix
        // framing assumes byte-exact transport, and a wallet that collapsed
        // the newlines may have done worse things to the value too. Drop it.
        Op::SetL => return Err(MemoReject::Malformed(MemoFormat::SetlCollapsedUnsupported)),
        Op::Set => {
            let v = tokens
                .next()
                .ok_or(MemoReject::Malformed(MemoFormat::MissingValue))?;
            if v.is_empty() {
                return Err(MemoReject::Malformed(MemoFormat::MissingValue));
            }
            Some(v.to_owned())
        }
        Op::WriterAdd => {
            let v = tokens
                .next()
                .ok_or(MemoReject::Malformed(MemoFormat::MissingScope))?;
            if v.is_empty() {
                return Err(MemoReject::Malformed(MemoFormat::MissingScope));
            }
            Some(v.to_owned())
        }
        Op::Version => {
            let v = tokens
                .next()
                .ok_or(MemoReject::Malformed(MemoFormat::MissingVersionFlag))?;
            if v.is_empty() {
                return Err(MemoReject::Malformed(MemoFormat::MissingVersionFlag));
            }
            Some(v.to_owned())
        }
        Op::Del | Op::OwnerAdd | Op::OwnerDel | Op::WriterDel => {
            if tokens.next().is_some() {
                return Err(MemoReject::Malformed(MemoFormat::WrongArity { op }));
            }
            None
        }
        Op::Init => tokens.next().map(|s| s.to_owned()),
        // FINALIZE returned early above (it has no key to reach this match).
        Op::Finalize => unreachable!("FINALIZE is handled before the key parse"),
    };

    Ok(ZkvCommand {
        op,
        key,
        value,
        seq,
        sig_hex,
        comment: None,
    })
}
