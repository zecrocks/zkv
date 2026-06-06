//! External-crate smoke test for the library surface.
//!
//! This file compiles as if it were a separate crate that depends on `zkv`,
//! so it proves that everything required to validate a ZKV address is
//! reachable through the public API.

use zkv::protocol::{parse_zkv_addr, zkv_addr_to_uview, TC_ZKV_META};

fn expect_err(input: &str) -> String {
    match parse_zkv_addr(input) {
        Ok(_) => panic!("expected error parsing {input:?}, got Ok"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn meta_typecode_is_in_private_use_range() {
    // The zkv-meta unified item (which carries the birthday inside the address)
    // must sit in ZIP-316's unknown-item range so conforming wallets ignore it.
    assert!((0x04..=0x0200_0000).contains(&TC_ZKV_META));
}

#[test]
fn parse_rejects_non_bech32() {
    let err = expect_err("not an address");
    assert!(err.contains("zkv address"), "got: {err}");
}

#[test]
fn parse_rejects_colon_separated_input() {
    // A zkv address is a single bech32m token under a `zkv` HRP; it has no
    // colons, so a colon-separated string is not an address.
    let err = expect_err("zkv1:uview1abc:1234");
    assert!(!err.is_empty(), "colon-separated input must be rejected");
}

#[test]
fn parse_rejects_plain_uview_hrp() {
    // A standard `uview…` viewing key is not a zkv address: wrong HRP, and no
    // zkv-meta item.
    let err = expect_err("uview1qqqqqqqqqqqq");
    assert!(!err.is_empty(), "a plain uview HRP must be rejected");
}

#[test]
fn view_key_export_rejects_non_zkv() {
    // `zkv_addr_to_uview` (the `--view-key` export) is reachable and refuses a
    // non-zkv input rather than mis-converting it.
    assert!(zkv_addr_to_uview("definitely-not-a-zkv-address").is_err());
}
