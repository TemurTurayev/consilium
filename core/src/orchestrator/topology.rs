//! Pure dependency-layering for a conduct plan: groups subtasks into waves where
//! every subtask in wave N depends only on subtasks in waves < N (Kahn's
//! algorithm). Sequential execution iterates the waves in order; Phase B runs a
//! wave's members concurrently. No I/O, no engine state — exhaustively unit-tested.

use crate::orchestrator::conduct::Subtask;

/// Group subtasks into dependency waves, returned as vectors of INDICES into
/// `subtasks`. Within a wave, original slice order is preserved (deterministic).
///
/// Errors (the conductor produced an invalid DAG):
/// - a `depends_on` id that no subtask defines,
/// - a self-edge,
/// - duplicate subtask ids (ambiguous edges),
/// - a cycle (no subtask becomes ready while work remains).
pub fn plan_waves(subtasks: &[Subtask]) -> anyhow::Result<Vec<Vec<usize>>> {
    use std::collections::HashSet;

    // Unique-id check: edges reference ids, so duplicate ids make edges ambiguous.
    let mut ids: HashSet<u32> = HashSet::new();
    for s in subtasks {
        if !ids.insert(s.id) {
            anyhow::bail!("plan has duplicate subtask id {}", s.id);
        }
    }

    // Edge validation: every dep must reference a known, non-self id.
    for s in subtasks {
        for d in &s.depends_on {
            if *d == s.id {
                anyhow::bail!("subtask {} depends on itself", s.id);
            }
            if !ids.contains(d) {
                anyhow::bail!("subtask {} depends on unknown subtask {}", s.id, d);
            }
        }
    }

    // Kahn layering: each round, a wave = every not-yet-placed subtask whose deps
    // are all already placed. O(n^2) — n <= 5 in practice.
    let mut placed: HashSet<u32> = HashSet::new();
    let mut done = vec![false; subtasks.len()];
    let mut waves: Vec<Vec<usize>> = Vec::new();

    while placed.len() < subtasks.len() {
        let wave: Vec<usize> = subtasks
            .iter()
            .enumerate()
            .filter(|(i, s)| !done[*i] && s.depends_on.iter().all(|d| placed.contains(d)))
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
        let waves = plan_waves(&s).unwrap();
        assert_eq!(waves, vec![vec![0, 1, 2]], "empty edges => today's order");
    }

    #[test]
    fn linear_chain_is_one_per_wave() {
        // 1 -> 2 -> 3, declared out of order to prove layering, not slice order, wins.
        let s = vec![sub(3, &[2]), sub(1, &[]), sub(2, &[1])];
        let waves = plan_waves(&s).unwrap();
        // indices: 1 is at idx 1, 2 at idx 2, 3 at idx 0.
        assert_eq!(waves, vec![vec![1], vec![2], vec![0]]);
    }

    #[test]
    fn diamond_groups_the_middle_pair() {
        // 1 -> {2,3} -> 4
        let s = vec![sub(1, &[]), sub(2, &[1]), sub(3, &[1]), sub(4, &[2, 3])];
        let waves = plan_waves(&s).unwrap();
        assert_eq!(waves, vec![vec![0], vec![1, 2], vec![3]]);
    }

    #[test]
    fn unknown_dependency_is_an_error() {
        let s = vec![sub(1, &[9])];
        let err = plan_waves(&s).unwrap_err().to_string();
        assert!(err.contains("unknown"), "got: {err}");
    }

    #[test]
    fn self_edge_is_an_error() {
        let s = vec![sub(1, &[1])];
        assert!(plan_waves(&s).unwrap_err().to_string().contains("itself"));
    }

    #[test]
    fn cycle_is_an_error() {
        let s = vec![sub(1, &[2]), sub(2, &[1])];
        assert!(plan_waves(&s).unwrap_err().to_string().contains("cycle"));
    }

    #[test]
    fn duplicate_ids_are_an_error() {
        let s = vec![sub(1, &[]), sub(1, &[])];
        assert!(plan_waves(&s)
            .unwrap_err()
            .to_string()
            .contains("duplicate"));
    }

    #[test]
    fn empty_plan_is_no_waves() {
        assert_eq!(plan_waves(&[]).unwrap(), Vec::<Vec<usize>>::new());
    }
}
