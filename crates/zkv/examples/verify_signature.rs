//! Pure-protocol demo: build a canonical signed payload, sign it with a
//! freshly generated secp256k1 key, and verify the signature.
//!
//! No network, no wallet, no filesystem. Runs as a deterministic
//! roundtrip you can use to sanity-check a `zkv` build:
//!
//! ```text
//! cargo run -p zkv --example verify_signature
//! ```

use rand::{rngs::OsRng, RngCore};
use secp256k1::{Secp256k1, SecretKey};

use zkv::protocol::{sign_command, signed_payload, verify_command, Op};

fn main() {
    // Any string works for the payload-binding role; in production this is the
    // database's *signing domain*: its shielded receiver hex plus the per-key
    // version (see `zkv::protocol::signing_domain` / `receiver_domain`), not the
    // `zkv1…` address string. The signing keypair would be derived from the
    // UFVK's transparent component (see `zkv::protocol::zkv_verifying_pubkey`).
    let domain = "demo-receiver-domain:0";

    // Generate an ad-hoc keypair for the demo.
    let secp = Secp256k1::new();
    let mut sk_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut sk_bytes);
    let sk = SecretKey::from_slice(&sk_bytes).expect("32 random bytes");
    let pk = sk.public_key(&secp);

    // Canonical payload-to-sign and signature.
    let payload = signed_payload(domain, Op::Set, "temperature", Some("72F"));
    let sig = sign_command(&sk, &payload);
    let sig_hex = hex::encode(sig);

    // Verify the good signature.
    assert!(
        verify_command(&pk, &payload, &sig_hex),
        "valid signature must verify",
    );
    println!("✓ valid signature verified ({} hex chars)", sig_hex.len());

    // Tamper with the payload; the same signature must no longer verify.
    let tampered = signed_payload(domain, Op::Set, "temperature", Some("90F"));
    assert!(
        !verify_command(&pk, &tampered, &sig_hex),
        "tampered payload must not verify",
    );
    println!("✓ tampered payload correctly rejected");
}
