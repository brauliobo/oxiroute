use oxiroute_config::CipherList;

#[test]
fn keywords_and_operators_are_macros_and_suite_names_are_not() {
    for value in [
        "HIGH",
        "HIGH:!aNULL:!MD5",
        "ALL:!eNULL",
        "-ALL:+HIGH",
        "ECDHE+AESGCM",
        "kEECDH:@STRENGTH",
        "DEFAULT:@SECLEVEL=1",
    ] {
        assert!(CipherList::parse(value).has_macros(), "{value}");
        assert!(CipherList::parse(value).suites().is_empty(), "{value}");
    }
    for value in [
        "ECDHE-RSA-AES128-GCM-SHA256",
        "DHE-RSA-AES256-GCM-SHA384:AES128-SHA",
        "TLS_AES_128_GCM_SHA256",
    ] {
        assert!(!CipherList::parse(value).has_macros(), "{value}");
    }
}

#[test]
fn a_mixed_list_separates_suites_from_macros() {
    let list = CipherList::parse("ECDHE-RSA-AES128-GCM-SHA256:HIGH:!aNULL");

    assert_eq!(list.suites(), ["ECDHE-RSA-AES128-GCM-SHA256"]);
    assert_eq!(list.macros(), ["HIGH", "!aNULL"]);
}

#[test]
fn dhe_rsa_and_unserved_suites_are_found_among_the_named_suites_only() {
    let list = CipherList::parse("ECDHE-RSA-AES128-GCM-SHA256:EDH-RSA-DES-CBC3-SHA:AES128-SHA");

    assert!(list.lists_dhe_rsa());
    assert!(list.lists_rustls_tls12_suite());
    assert_eq!(
        list.suites_rustls_cannot_serve().collect::<Vec<_>>(),
        ["EDH-RSA-DES-CBC3-SHA", "AES128-SHA"]
    );
    assert!(!CipherList::parse("HIGH:!DHE-RSA-AES128-GCM-SHA256").lists_dhe_rsa());
}
