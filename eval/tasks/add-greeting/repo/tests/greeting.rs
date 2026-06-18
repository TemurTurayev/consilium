// Protected oracle — the harness restores this from the baseline commit before
// scoring, so an approach cannot pass by deleting or rewriting it.
#[test]
fn greeting_says_hello() {
    assert_eq!(add_greeting::greeting(), "hello");
}
