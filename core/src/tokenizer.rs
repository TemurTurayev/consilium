//! Heuristic token estimation for CLIs that report no usage.
//!
//! Notably Gemini-via-Antigravity (`agy`) prints plain text with no usage
//! envelope, so its real token counts are unknowable. Recording 0 would blind
//! both quota accounting and least-loaded routing — a provider that always logs
//! 0 looks permanently idle, so the router hands it every subtask. Instead the
//! runner records a heuristic estimate, flagged `estimated` in the quota store
//! so it stays distinguishable from CLI-measured usage.
//!
//! The ~4-chars-per-token ratio is the standard rough heuristic for mixed
//! English + code. It is intentionally simple; a per-provider/real tokenizer is
//! a future refinement. (Pattern borrowed from HarnessLab/claw-code-agent's
//! `tokenizer_runtime` heuristic fallback.)

/// Estimate token count for `text` at ~4 chars/token. Empty text → 0;
/// any non-empty text → at least 1.
pub fn estimate_tokens(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    if chars == 0 {
        0
    } else {
        chars.div_ceil(4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn rounds_up_quarter_chars() {
        assert_eq!(estimate_tokens("a"), 1); // ceil(1/4)
        assert_eq!(estimate_tokens("abcd"), 1); // 4/4
        assert_eq!(estimate_tokens("abcde"), 2); // ceil(5/4)
    }

    #[test]
    fn counts_unicode_scalars_not_bytes() {
        // 4 multi-byte chars → 1 token (by chars, not bytes).
        assert_eq!(estimate_tokens("é€中🦀"), 1);
    }
}
