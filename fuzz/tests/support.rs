#[path = "../fuzz_targets/support/mod.rs"]
mod support;

#[test]
fn decodes_reviewable_hex_seeds_with_line_endings() {
    assert_eq!(
        support::bounded_input(b"hex:0A10\n", 16)
            .expect("bounded hex seed")
            .as_ref(),
        &[0x0a, 0x10]
    );
    assert_eq!(
        support::bounded_input(b"hex:0A10\r\n", 16)
            .expect("bounded CRLF hex seed")
            .as_ref(),
        &[0x0a, 0x10]
    );
    assert_eq!(
        support::bounded_input(b"seed:simple\n", 16)
            .expect("bounded marker seed")
            .as_ref(),
        b"seed:simple"
    );
}
