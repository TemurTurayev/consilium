//! Stagnation detection (Finding P1.5): a circuit breaker for ungrounded rework
//! loops. Each rework attempt is fingerprinted by its captured changes + verify
//! result; if an attempt reproduces a prior attempt's exact fingerprint, the
//! worker is making no progress — reworking again would only spin — so the
//! conduct loop stops early instead of burning the remaining rework budget.
//!
//! Research basis: ungrounded reflection loops degrade without a stop signal
//! (OpenHands stuck-detection; fingerprint loop detection). Conservative v1: any
//! exact repeat of a prior fingerprint is a stall (with MAX_REWORKS=2, a single
//! repeat already means one of ≤3 attempts produced nothing new).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// A fingerprint of one rework attempt: its captured diff plus the verifier's
/// summary. Two attempts that produce the identical diff AND the identical
/// verify outcome share a fingerprint. Stable within a run (used only for
/// in-loop comparison, never persisted across processes).
pub fn fingerprint(changes: &str, verify_summary: &str) -> u64 {
    let mut h = DefaultHasher::new();
    changes.hash(&mut h);
    // Separator so ("ab","c") and ("a","bc") can't collide.
    0xFFu8.hash(&mut h);
    verify_summary.hash(&mut h);
    h.finish()
}

/// True when `current` repeats a fingerprint already seen this subtask — i.e.
/// the worker reproduced an earlier attempt's exact diff + verify result.
pub fn is_stalled(history: &[u64], current: u64) -> bool {
    history.contains(&current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_inputs_share_a_fingerprint() {
        assert_eq!(fingerprint("diff", "ok"), fingerprint("diff", "ok"));
    }

    #[test]
    fn different_changes_or_verify_differ() {
        assert_ne!(fingerprint("diff a", "ok"), fingerprint("diff b", "ok"));
        assert_ne!(fingerprint("diff", "passed"), fingerprint("diff", "failed"));
    }

    #[test]
    fn the_changes_verify_boundary_is_unambiguous() {
        assert_ne!(fingerprint("ab", "c"), fingerprint("a", "bc"));
    }

    #[test]
    fn stall_is_a_repeat_of_a_prior_attempt() {
        let a = fingerprint("d0", "fail");
        let b = fingerprint("d1", "fail");
        assert!(!is_stalled(&[], a), "the first attempt is never stalled");
        assert!(!is_stalled(&[a], b), "distinct progress is not stalled");
        assert!(
            is_stalled(&[a, b], a),
            "reproducing an earlier attempt stalls"
        );
    }
}
