// Protected oracle — restored from the baseline commit before scoring.
use dedupe_stable::{dedupe_stable, tag_line};

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn keeps_the_first_occurrence_in_first_seen_order() {
    assert_eq!(dedupe_stable(&[3, 1, 3, 2, 1]), vec![3, 1, 2]);
}

#[test]
fn duplicates_need_not_be_adjacent_or_sorted() {
    assert_eq!(dedupe_stable(&[9, 8, 9, 7, 8, 9]), vec![9, 8, 7]);
}

#[test]
fn works_for_any_hashable_cloneable_type() {
    assert_eq!(
        dedupe_stable(&strings(&["b", "a", "b"])),
        strings(&["b", "a"])
    );
    assert_eq!(dedupe_stable(&['x', 'y', 'x']), vec!['x', 'y']);
    assert_eq!(dedupe_stable(&[(1, "a"), (1, "a"), (2, "a")]), vec![(1, "a"), (2, "a")]);
}

#[test]
fn empty_and_already_unique_inputs_come_back_unchanged() {
    assert_eq!(dedupe_stable::<u8>(&[]), Vec::<u8>::new());
    assert_eq!(dedupe_stable(&[1, 2, 3]), vec![1, 2, 3]);
}

#[test]
fn leaves_its_input_untouched() {
    let input = vec![2, 2, 1];
    let _ = dedupe_stable(&input);
    assert_eq!(input, vec![2, 2, 1]);
}

#[test]
fn tag_line_lists_each_tag_once_in_first_seen_order() {
    assert_eq!(
        tag_line(&strings(&["rust", "cli", "rust", "ai", "cli"])),
        "rust, cli, ai"
    );
    assert_eq!(tag_line(&[]), "");
}
