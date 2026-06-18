// Protected oracle — restored from the baseline commit before scoring.
#[test]
fn splits_on_first_eq() {
    assert_eq!(
        parse_kv::parse_kv("key=value"),
        Some(("key".to_string(), "value".to_string()))
    );
    // The value may itself contain '=' — split on the FIRST one only.
    assert_eq!(
        parse_kv::parse_kv("a=b=c"),
        Some(("a".to_string(), "b=c".to_string()))
    );
    assert_eq!(parse_kv::parse_kv("noequals"), None);
}
