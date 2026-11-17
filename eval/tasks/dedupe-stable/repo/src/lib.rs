/// Joins tags into one comma-separated line, each tag once.
///
/// TASK (refactor): duplicates are removed here by sorting, which throws away
/// the order the tags were first seen in, and the logic cannot be reused for
/// anything but strings. Extract a generic `dedupe_stable` (see task.json) and
/// build `tag_line` on it. tests/dedupe.rs asserts the result — do not edit it.
pub fn tag_line(tags: &[String]) -> String {
    let mut unique = tags.to_vec();
    unique.sort();
    unique.dedup();
    unique.join(", ")
}
