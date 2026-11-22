//! Small text helpers shared by status listings and failure reports.

/// A CLI error squeezed onto one line for a status listing: runs of whitespace
/// (including newlines) become single spaces, and anything beyond `max_chars`
/// characters is cut with an ellipsis. Provider CLIs answer with multi-line
/// text — a pretty-printed JSON blob, or a reason followed by a long model
/// list — which otherwise buries the status lines around it.
pub(crate) fn one_line(detail: &str, max_chars: usize) -> String {
    let flat = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let kept: String = flat.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}
