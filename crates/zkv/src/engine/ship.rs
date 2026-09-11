//! Broadcasting signed memos: sending one prepared transaction request.
//!
//! A zkv write is a shielded self-send carrying a signed memo, at zero value:
//! Zcash has no dust floor on shielded outputs, so the only cost is the
//! ZIP-317 fee. A batch is the same thing with several outputs, one fee, one
//! txid.
//!
//! There is no conversion step. zkv's write path already produces a
//! [`zip321::TransactionRequest`] (`internal::write`), which is the same type
//! `z_sendmany` parses its JSON into, and the node's send seam takes it
//! directly. So a memo goes from the signer to the chain without being
//! rendered to JSON and parsed back.
//!
//! The send is synchronous: it returns the txid rather than an operation id to
//! poll, so there is no in-flight operation to lose track of if the process
//! dies. The wallet commits the transaction to `data.sqlite` *before*
//! broadcasting, which is what keeps a write readable immediately afterwards,
//! and it rebroadcasts unmined transactions on a timer, so a transport failure
//! is not a lost write.
//!
//! A batch may pay the same address more than once. The RPC refuses that for
//! zcashd parity, but this seam accepts it, which is what lets N memos ride to
//! one wallet in a single transaction.

use zecd::config::SendPrivacy;
use zecd::node::SendOptions;
use zecd::zip321::TransactionRequest;

use super::{config::WALLET, Engine, EngineError};

/// The privacy policy zkv sends under.
///
/// Deliberately not the strictest setting: full privacy rejects any
/// transaction that touches more than one pool, and past NU6.3 an ordinary
/// send spends legacy Orchard notes into an Ironwood output, which is exactly
/// such a crossing. Nothing is revealed by this that a zkv write does not
/// already publish, since every output goes to the database's own wallet.
const PRIVACY: SendPrivacy = SendPrivacy::AllowRevealedRecipients;

impl Engine {
    /// Send one transaction carrying these memos, and return its txid.
    ///
    /// The request is the one `internal::write` built and signed; nothing here
    /// inspects or rebuilds it.
    pub async fn ship(&self, request: TransactionRequest) -> Result<String, EngineError> {
        if request.payments().is_empty() {
            return Err(EngineError::Other(anyhow::anyhow!(
                "internal: asked to send a transaction with no memos",
            )));
        }

        // `SendOptions` is `#[non_exhaustive]`, which bars a struct expression
        // from outside zecd (`..Default::default()` included: that syntax only
        // helps within the defining crate). Taking the defaults and assigning
        // the one field zkv cares about is the supported spelling, and it is
        // what keeps this compiling when upstream adds a field.
        let mut opts = SendOptions::default();
        opts.privacy = Some(PRIVACY);

        let node = self.node().await?;
        let txid = node.send(Some(WALLET), request, opts).await?;
        Ok(txid.to_string())
    }
}

#[cfg(test)]
mod tests {
    /// A zkv memo has to survive the wire byte for byte: its signature covers
    /// its bytes, so a memo that comes back altered recovers a different
    /// signer and is dropped during replay.
    #[test]
    fn a_memo_zkv_builds_fits_the_wire_limit() {
        use crate::internal::protocol::{build_memo, Op, SIG_LEN};
        use zcash_protocol::memo::Memo;

        let sig = [7u8; SIG_LEN];
        // A value big enough that the memo is near the cap: the header, the
        // value, and the 130-hex-character signature line together.
        let value = "v".repeat(300);
        let memo = build_memo(Op::Set, "some/key", Some(&value), 0, &sig).expect("build");
        let Ok(Memo::Text(text)) = Memo::try_from(memo) else {
            panic!("a zkv memo is a text memo");
        };
        assert!(
            text.to_string().len() <= 512,
            "a zkv memo must fit the wire: {} bytes",
            text.to_string().len(),
        );

        // And zkv refuses to build one that would not fit, rather than
        // producing something the send would reject.
        let too_big = "v".repeat(600);
        assert!(
            build_memo(Op::Set, "some/key", Some(&too_big), 0, &sig).is_err(),
            "an oversize value is refused before it reaches the wire",
        );
    }
}
