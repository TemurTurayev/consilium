//! Pure dependency-layering for a conduct plan: groups subtasks into waves where
//! every subtask in wave N depends only on subtasks in waves < N (Kahn's
//! algorithm). Sequential execution iterates the waves in order; Phase B runs a
//! wave's members concurrently. No I/O, no engine state — exhaustively unit-tested.

use crate::orchestrator::conduct::Subtask;

/// Group subtasks into dependency waves, returned as vectors of INDICES into
/// `subtasks`. Within a wave, original slice order is preserved (deterministic).
/// `completed` carries ids already shipped by prior (pre-replan) plans: a
/// `depends_on` id in `completed` is a satisfied cross-plan edge (valid, and
/// ready immediately), not an unknown-subtask error.
///
/// Errors (the conductor produced an invalid DAG):
/// - a `depends_on` id that is neither in this plan nor already completed,
/// - a self-edge,
/// - duplicate subtask ids (ambiguous edges),
/// - a cycle (no subtask becomes ready while work remains).
pub fn plan_waves(subtasks: &[Subtask], completed: &[u32]) -> anyhow::Result<Vec<Vec<usize>>> {
    use std::collections::HashSet;

    let mut ids: HashSet<u32> = HashSet::new();
    for s in subtasks {
        if !ids.insert(s.id) {
            anyhow::bail!("plan has duplicate subtask id {}", s.id);
        }
    }

    // Edge validation: a dep must reference a known same-plan id OR an
    // already-completed id from a prior plan (a satisfied cross-plan edge).
    for s in subtasks {
        for d in &s.depends_on {
            if *d == s.id {
                anyhow::bail!("subtask {} depends on itself", s.id);
            }
            if !ids.contains(d) && !completed.contains(d) {
                anyhow::bail!("subtask {} depends on unknown subtask {}", s.id, d);
            }
        }
    }

    // Kahn layering: a subtask is ready when every dep is already placed in an
    // earlier wave OR was completed by a prior plan. O(n^2) — n <= 5 in practice.
    let mut placed: HashSet<u32> = HashSet::new();
    let mut done = vec![false; subtasks.len()];
    let mut waves: Vec<Vec<usize>> = Vec::new();

    while placed.len() < subtasks.len() {
        let wave: Vec<usize> = subtasks
            .iter()
            .enumerate()
            .filter(|(i, s)| {
                !done[*i]
                    && s.depends_on
                        .iter()
                        .all(|d| placed.contains(d) || completed.contains(d))
            })
            .map(|(i, _)| i)
            .collect();
        if wave.is_empty() {
            anyhow::bail!("dependency cycle among subtasks (no subtask is runnable)");
        }
        for &i in &wave {
            done[i] = true;
            placed.insert(subtasks[i].id);
        }
        waves.push(wave);
    }
    Ok(waves)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::conduct::Subtask;

    fn sub(id: u32, deps: &[u32]) -> Subtask {
        Subtask {
            id,
            title: String::new(),
            prompt: String::new(),
            depends_note: String::new(),
            depends_on: deps.to_vec(),
        }
    }

    #[test]
    fn no_deps_is_one_wave_in_original_order() {
        let s = vec![sub(1, &[]), sub(2, &[]), sub(3, &[])];
        let waves = plan_waves(&s, &[]).unwrap();
        assert_eq!(waves, vec![vec![0, 1, 2]], "empty edges => today's order");
    }

    #[test]
    fn linear_chain_is_one_per_wave() {
        // 1 -> 2 -> 3, declared out of order to prove layering, not slice order, wins.
        let s = vec![sub(3, &[2]), sub(1, &[]), sub(2, &[1])];
        let waves = plan_waves(&s, &[]).unwrap();
        // indices: 1 is at idx 1, 2 at idx 2, 3 at idx 0.
        assert_eq!(waves, vec![vec![1], vec![2], vec![0]]);
    }

    #[test]
    fn diamond_groups_the_middle_pair() {
        // 1 -> {2,3} -> 4
        let s = vec![sub(1, &[]), sub(2, &[1]), sub(3, &[1]), sub(4, &[2, 3])];
        let waves = plan_waves(&s, &[]).unwrap();
        assert_eq!(waves, vec![vec![0], vec![1, 2], vec![3]]);
    }

    #[test]
    fn unknown_dependency_is_an_error() {
        let s = vec![sub(1, &[9])];
        let err = plan_waves(&s, &[]).unwrap_err().to_string();
        assert!(err.contains("unknown"), "got: {err}");
    }

    #[test]
    fn self_edge_is_an_error() {
        let s = vec![sub(1, &[1])];
        assert!(plan_waves(&s, &[])
            .unwrap_err()
            .to_string()
            .contains("itself"));
    }

    #[test]
    fn cycle_is_an_error() {
        let s = vec![sub(1, &[2]), sub(2, &[1])];
        assert!(plan_waves(&s, &[])
            .unwrap_err()
            .to_string()
            .contains("cycle"));
    }

    #[test]
    fn duplicate_ids_are_an_error() {
        let s = vec![sub(1, &[]), sub(1, &[])];
        assert!(plan_waves(&s, &[])
            .unwrap_err()
            .to_string()
            .contains("duplicate"));
    }

    #[test]
    fn empty_plan_is_no_waves() {
        assert_eq!(plan_waves(&[], &[]).unwrap(), Vec::<Vec<usize>>::new());
    }

    #[test]
    fn completed_dependency_from_a_prior_plan_is_satisfied() {
        // A replanned subtask depends on id 2, which is NOT in this plan slice but
        // WAS completed in a prior (pre-replan) plan → valid + ready in wave 0.
        let s = vec![sub(3, &[2])];
        assert_eq!(plan_waves(&s, &[2]).unwrap(), vec![vec![0]]);
        // Without the completed context, the same dep is an unknown-subtask error.
        assert!(plan_waves(&s, &[]).is_err());
    }
}
