// Protected oracle — the harness restores this from the baseline before scoring,
// so an approach cannot pass by deleting or rewriting it.
use duration_parser::parse_duration;

#[test]
fn parses_single_units() {
    assert_eq!(parse_duration("90s"), Some(90));
    assert_eq!(parse_duration("5m"), Some(300));
    assert_eq!(parse_duration("2h"), Some(7200));
    assert_eq!(parse_duration("1d"), Some(86400));
}

#[test]
fn parses_compound_units_in_descending_order() {
    assert_eq!(parse_duration("1h30m"), Some(5400));
    assert_eq!(parse_duration("1d2h3m4s"), Some(93784));
    assert_eq!(parse_duration("2h15s"), Some(7215)); // may skip middle units
}

#[test]
fn trims_surrounding_whitespace() {
    assert_eq!(parse_duration("  45s  "), Some(45));
}

#[test]
fn rejects_malformed_input() {
    assert_eq!(parse_duration(""), None);
    assert_eq!(parse_duration("   "), None);
    assert_eq!(parse_duration("abc"), None);
    assert_eq!(parse_duration("5"), None); // number with no unit
    assert_eq!(parse_duration("5x"), None); // unknown unit
    assert_eq!(parse_duration("-5s"), None); // sign
    assert_eq!(parse_duration("1.5h"), None); // decimal
    assert_eq!(parse_duration("h"), None); // unit with no number
    assert_eq!(parse_duration("90s "), Some(90)); // trailing ws is trimmed, still valid
}

#[test]
fn rejects_bad_ordering_and_duplicates() {
    assert_eq!(parse_duration("1m1h"), None); // out of order
    assert_eq!(parse_duration("30m1h"), None); // out of order
    assert_eq!(parse_duration("1h1h"), None); // duplicate
    assert_eq!(parse_duration("1s1d"), None); // out of order
}
