use super::*;

/// The owner/writer registry: who may write, and with what authority.
///
/// Keyed by the canonical `zkvid1…` Bech32m pubkey ([`pubkey_bech32`]). The
/// root key (the UFVK-derived signer that broadcast INIT) is inserted as an
/// owner when INIT is honored; further owners and writers are added by
/// `OWNER*`/`WRITER*` management memos.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthRegistry {
    owners: BTreeSet<String>,
    writers: BTreeMap<String, Scope>,
}

impl AuthRegistry {
    pub fn is_empty(&self) -> bool {
        self.owners.is_empty() && self.writers.is_empty()
    }

    pub fn is_owner(&self, pubkey_bech32: &str) -> bool {
        self.owners.contains(pubkey_bech32)
    }

    pub fn owners(&self) -> impl Iterator<Item = &str> {
        self.owners.iter().map(String::as_str)
    }

    pub fn writers(&self) -> impl Iterator<Item = (&str, &Scope)> {
        self.writers.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// The authority a pubkey holds, if any. Owner takes precedence over a
    /// writer entry for the same key (a key promoted to owner is an owner).
    pub fn authority_of(&self, pubkey_bech32: &str) -> Option<Authority> {
        if self.owners.contains(pubkey_bech32) {
            Some(Authority::Owner)
        } else {
            self.writers
                .get(pubkey_bech32)
                .map(|s| Authority::Writer(s.clone()))
        }
    }

    /// Whether `signer` may perform data op `op`, returning the standardized
    /// [`DropReason`] when not. Owners may always write; writers are checked
    /// against their scope and the create-vs-update distinction (judged
    /// against `key_exists`, the *confirmed* existence of the target key).
    pub fn authorize(&self, signer: &str, op: Op, key_exists: bool) -> Result<(), DropReason> {
        match self.authority_of(signer) {
            Some(Authority::Owner) => Ok(()),
            Some(Authority::Writer(scope)) => match op {
                Op::Set | Op::SetL => {
                    let cap = if key_exists {
                        Capability::Update
                    } else {
                        Capability::Create
                    };
                    if scope.contains(cap) {
                        Ok(())
                    } else {
                        Err(DropReason::OutOfScope { capability: cap })
                    }
                }
                Op::Del => {
                    if scope.contains(Capability::Destroy) {
                        Ok(())
                    } else {
                        Err(DropReason::OutOfScope {
                            capability: Capability::Destroy,
                        })
                    }
                }
                _ => Err(DropReason::NoWriteAuthority),
            },
            None => Err(DropReason::NoWriteAuthority),
        }
    }

    /// Boolean shorthand for [`AuthRegistry::authorize`], for hot-path callers
    /// (the write-side pre-check) that don't need the reason.
    pub fn may_write(&self, signer: &str, op: Op, key_exists: bool) -> bool {
        self.authorize(signer, op, key_exists).is_ok()
    }

    /// Promote `pubkey_bech32` to owner. Clears any prior writer scope: an owner
    /// already has full authority, so a scoped entry would only be shadowed.
    pub fn insert_owner(&mut self, pubkey_bech32: String) {
        self.writers.remove(&pubkey_bech32);
        self.owners.insert(pubkey_bech32);
    }

    /// Apply a confirmed, owner-authorized management op to the registry,
    /// returning `Ok(())` if it mutated state or `Err(reason)` if it was a
    /// policy no-op (last-owner protection, writer-targets-owner, bad target,
    /// bad scope). Either way the registry ends in a consistent state; the
    /// `Err` is purely so the history view can report *why* nothing changed.
    ///
    /// The caller is responsible for having checked that the op is confirmed
    /// and signed by a current owner; this method only performs the mutation.
    /// Shared by [`replay_with_seed`] and the snapshot promote path so both
    /// enforce identical semantics.
    pub fn apply_management(
        &mut self,
        op: Op,
        target: &str,
        value: Option<&str>,
    ) -> Result<(), DropReason> {
        let Some(target) = canonical_pubkey(target) else {
            return Err(DropReason::InvalidTargetPubkey);
        };
        match op {
            Op::OwnerSet => {
                self.insert_owner(target);
                Ok(())
            }
            Op::OwnerDel => {
                // Last-owner protection: never leave the registry ownerless.
                // When only one owner remains, removing *that* owner is the
                // protected case; an `OWNERDEL` of any other (absent) key is a
                // harmless no-op we report as applied.
                if self.owners.len() <= 1 {
                    if self.owners.contains(&target) {
                        Err(DropReason::LastOwnerProtected)
                    } else {
                        Ok(())
                    }
                } else {
                    self.owners.remove(&target);
                    Ok(())
                }
            }
            Op::WriterSet => {
                let Some(scope) = value.and_then(Scope::parse) else {
                    return Err(DropReason::InvalidScope);
                };
                // An owner already has full authority; a scoped writer entry
                // would only be shadowed. Demote via `OWNERDEL` first.
                if self.owners.contains(&target) {
                    return Err(DropReason::WriterTargetIsOwner);
                }
                self.writers.insert(target, scope);
                Ok(())
            }
            Op::WriterDel => {
                self.writers.remove(&target);
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// Canonicalize a pubkey string (as carried in a management memo's target
/// field) to the `zkvid1…` form that signers recover to. Accepts the canonical
/// Bech32m form or raw hex (compressed/uncompressed); returns `None` if it isn't
/// a valid secp256k1 public key. This guarantees registry keys always match the
/// signer string computed via [`recover_signer`] + [`pubkey_bech32`].
fn canonical_pubkey(s: &str) -> Option<String> {
    parse_pubkey(s).map(|pk| pubkey_bech32(&pk))
}
