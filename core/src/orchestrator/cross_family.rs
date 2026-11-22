//! Cross-family review: put a reviewer of a different model family than the
//! diff's author at the front of the ladder, so a model does not mark its own
//! homework (self-preference bias). Used by `conduct`'s review gate and by the
//! MCP server's `review_diff`.

use crate::family::ModelFamily;
use crate::orchestrator::council::CouncilMember;
use crate::orchestrator::resilience::Rung;

/// Cross-family review ladder (Finding 7): a reviewer/arbiter ladder whose front
/// is a DIFFERENT model family than the diff's `author`. Order: the role's own
/// different-family rungs, then different-family worker primaries (borrowed as
/// fallbacks so a single-rung same-family reviewer still gets a cross-family
/// option), then the role's same-family rungs last (fail-open if the
/// cross-family models are all dead at runtime). Returns `(ladder, degraded)`,
/// where `degraded` = no different-family option existed at all (front is
/// same-family) — the review still runs, just same-family, and is marked.
///
/// Family is the model's, not the CLI's (see `crate::family`): a Claude model
/// served through Antigravity is still Claude.
pub(crate) fn cross_family_ladder(
    role_ladder: &[Rung],
    workers: &[CouncilMember],
    author: ModelFamily,
) -> (Vec<Rung>, bool) {
    let same_family = |rung: &&Rung| rung_family(rung) == author;
    let other_family = |rung: &&Rung| rung_family(rung) != author;
    let out: Vec<Rung> = role_ladder
        .iter()
        .filter(other_family)
        .chain(
            workers
                .iter()
                .filter_map(|w| w.ladder.first())
                .filter(other_family),
        )
        .chain(role_ladder.iter().filter(same_family))
        .cloned()
        .collect();
    let degraded = out.first().is_none_or(|front| rung_family(front) == author);
    (out, degraded)
}

fn rung_family(rung: &Rung) -> ModelFamily {
    ModelFamily::of(rung.candidate.provider, &rung.candidate.model)
}

/// The family of the model that wrote a worker's diff: the rung that actually
/// ran (a worker's ladder can cross families — Gemini, then Claude as its
/// fallback), or the worker's primary when none is known to have run.
pub(crate) fn author_family(worker: &CouncilMember, rung_used: Option<usize>) -> ModelFamily {
    rung_used
        .and_then(|i| worker.ladder.get(i))
        .or_else(|| worker.ladder.first())
        .map(rung_family)
        .unwrap_or(ModelFamily::Anthropic)
}

/// How far an examiner stands from the two parties it rules between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Independence {
    /// A family that neither wrote the diff nor reviewed it.
    Independent,
    /// The author's family: inclined to defend the diff.
    AuthorsFamily,
    /// The reviewer's family: inclined to agree with the findings.
    ReviewersFamily,
}

impl Independence {
    /// Where a model of `family` stands between a reviewer and an author.
    pub fn of(family: ModelFamily, reviewer: ModelFamily, author: ModelFamily) -> Self {
        if family == reviewer {
            Independence::ReviewersFamily
        } else if family == author {
            Independence::AuthorsFamily
        } else {
            Independence::Independent
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Independence::Independent => "independent",
            Independence::AuthorsFamily => "authors_family",
            Independence::ReviewersFamily => "reviewers_family",
        }
    }
}

/// The ladder for cross-examining a reviewer's findings: every model the
/// lineup has for reviewing (`role_ladder`, then each worker's primary), each
/// once, ordered by how independent it is — a family that neither wrote the
/// diff nor reviewed it first, then the author's, and the reviewer's last.
/// Returns the ladder and the standing of the model at its front.
pub(crate) fn examiner_ladder(
    role_ladder: &[Rung],
    workers: &[CouncilMember],
    reviewer: ModelFamily,
    author: ModelFamily,
) -> (Vec<Rung>, Independence) {
    let standing = |rung: &Rung| Independence::of(rung_family(rung), reviewer, author);
    let pool = role_ladder
        .iter()
        .chain(workers.iter().filter_map(|worker| worker.ladder.first()));
    let distinct = pool.fold(Vec::<&Rung>::new(), |seen, rung| {
        if seen.iter().any(|kept| kept.candidate == rung.candidate) {
            seen
        } else {
            seen.into_iter().chain(std::iter::once(rung)).collect()
        }
    });
    let ladder: Vec<Rung> = [
        Independence::Independent,
        Independence::AuthorsFamily,
        Independence::ReviewersFamily,
    ]
    .into_iter()
    .flat_map(|wanted| distinct.iter().filter(move |rung| standing(rung) == wanted))
    .map(|rung| (*rung).clone())
    .collect();
    let front = ladder
        .first()
        .map(&standing)
        .unwrap_or(Independence::ReviewersFamily);
    (ladder, front)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubAdapter;
    impl crate::adapters::Adapter for StubAdapter {
        fn provider(&self) -> crate::event::Provider {
            crate::event::Provider::Claude
        }
        fn cli_binary(&self) -> &'static str {
            "stub"
        }
        fn build_command(&self, _req: &crate::adapters::RunRequest) -> tokio::process::Command {
            tokio::process::Command::new("true")
        }
        fn parse_line(&self, _line: &str) -> Vec<crate::event::AgentEvent> {
            Vec::new()
        }
    }
    fn cf_rung(p: crate::event::Provider, model: &str) -> Rung {
        Rung {
            candidate: crate::config::ModelCandidate {
                provider: p,
                model: model.into(),
            },
            adapter: std::sync::Arc::new(StubAdapter),
        }
    }
    fn cf_member(p: crate::event::Provider, label: &str) -> CouncilMember {
        CouncilMember {
            label: label.into(),
            ladder: vec![cf_rung(p, "m")],
        }
    }

    /// Antigravity runs Claude models under the `gemini` provider. Judged by
    /// provider, this reviewer looked cross-family for a Claude-written diff —
    /// and Claude reviewed Claude.
    #[test]
    fn cross_family_sees_through_the_antigravity_gateway() {
        use crate::event::Provider;
        let ladder = vec![
            cf_rung(Provider::Gemini, "claude-opus-5-5-high"),
            cf_rung(Provider::Codex, "gpt-6-luna"),
        ];
        let (l, degraded) = cross_family_ladder(&ladder, &[], ModelFamily::Anthropic);
        assert!(!degraded);
        assert_eq!(l[0].candidate.provider, Provider::Codex);
        assert_eq!(
            l.last().unwrap().candidate.model,
            "claude-opus-5-5-high",
            "the same-family rung goes last, whatever CLI runs it"
        );
    }

    /// A worker's ladder can cross families (Gemini, then Claude as its
    /// fallback): the diff's author is the rung that wrote it, not the first.
    #[test]
    fn the_author_is_the_rung_that_wrote_the_diff() {
        use crate::event::Provider;
        let worker = CouncilMember {
            label: "w".into(),
            ladder: vec![
                cf_rung(Provider::Gemini, "gemini-4-argon-high"),
                cf_rung(Provider::Claude, "claude-opus-5-5"),
            ],
        };
        assert_eq!(author_family(&worker, Some(0)), ModelFamily::Google);
        assert_eq!(author_family(&worker, Some(1)), ModelFamily::Anthropic);
        // no rung is known to have run: assume the worker's primary
        assert_eq!(author_family(&worker, None), ModelFamily::Google);
    }

    #[test]
    fn cross_family_fronts_a_different_family() {
        use crate::event::Provider;
        // reviewer = single Codex rung; worker = Codex; pool has a Gemini worker.
        let (l, degraded) = cross_family_ladder(
            &[cf_rung(Provider::Codex, "rev")],
            &[
                cf_member(Provider::Codex, "w1"),
                cf_member(Provider::Gemini, "w2"),
            ],
            ModelFamily::OpenAi,
        );
        assert!(!degraded);
        assert_eq!(
            l[0].candidate.provider,
            Provider::Gemini,
            "front must be cross-family"
        );
        assert!(
            l.iter().any(|r| r.candidate.provider == Provider::Codex),
            "same-family reviewer kept as a fallback (fail-open)"
        );
    }

    #[test]
    fn cross_family_degrades_when_single_family() {
        use crate::event::Provider;
        let (l, degraded) = cross_family_ladder(
            &[cf_rung(Provider::Codex, "rev")],
            &[cf_member(Provider::Codex, "w1")],
            ModelFamily::OpenAi,
        );
        assert!(degraded, "no other family available → degraded");
        assert_eq!(
            l[0].candidate.provider,
            Provider::Codex,
            "still runs same-family (fail-open, never blocks the review)"
        );
    }

    #[test]
    fn cross_family_noop_when_reviewer_already_different() {
        use crate::event::Provider;
        // reviewer = Claude, worker = Codex → already cross-family.
        let (l, degraded) = cross_family_ladder(
            &[cf_rung(Provider::Claude, "rev")],
            &[cf_member(Provider::Codex, "w1")],
            ModelFamily::OpenAi,
        );
        assert!(!degraded);
        assert_eq!(l[0].candidate.provider, Provider::Claude);
    }

    #[test]
    fn cross_family_reorders_multi_rung_stably() {
        use crate::event::Provider;
        // A multi-family reviewer ladder; worker = Codex. Different-family rungs
        // move to the front (stable), the same-family rung is appended last.
        let ladder = vec![
            cf_rung(Provider::Codex, "a"),
            cf_rung(Provider::Claude, "b"),
            cf_rung(Provider::Gemini, "c"),
        ];
        let (l, degraded) = cross_family_ladder(&ladder, &[], ModelFamily::OpenAi);
        assert!(!degraded);
        assert_eq!(
            l[0].candidate.provider,
            Provider::Claude,
            "stable: Claude first"
        );
        assert_eq!(l[1].candidate.provider, Provider::Gemini, "then Gemini");
        assert_eq!(
            l.last().unwrap().candidate.provider,
            Provider::Codex,
            "same-family appended last (fail-open)"
        );
    }

    fn models(ladder: &[Rung]) -> Vec<String> {
        ladder
            .iter()
            .map(|rung| {
                format!(
                    "{}/{}",
                    rung.candidate.provider.as_str(),
                    rung.candidate.model
                )
            })
            .collect()
    }

    fn worker(p: crate::event::Provider, model: &str) -> CouncilMember {
        CouncilMember {
            label: model.into(),
            ladder: vec![cf_rung(p, model)],
        }
    }

    /// Codex reviewed a diff Claude wrote: Gemini has no stake in either.
    #[test]
    fn the_examiner_is_a_family_that_neither_wrote_nor_reviewed() {
        use crate::event::Provider;
        let reviewer = [
            cf_rung(Provider::Codex, "gpt-6-luna"),
            cf_rung(Provider::Gemini, "gemini-4-argon-high"),
        ];
        let workers = [
            worker(Provider::Claude, "claude-fable-5-2"),
            worker(Provider::Gemini, "claude-opus-5-5-high"),
            worker(Provider::Gemini, "gemini-4-hydrogen-high"),
        ];
        let (ladder, standing) = examiner_ladder(
            &reviewer,
            &workers,
            ModelFamily::OpenAi,
            ModelFamily::Anthropic,
        );
        assert_eq!(standing, Independence::Independent);
        assert_eq!(
            models(&ladder),
            [
                "gemini/gemini-4-argon-high",
                "gemini/gemini-4-hydrogen-high",
                "claude/claude-fable-5-2",
                "gemini/claude-opus-5-5-high",
                "codex/gpt-6-luna",
            ],
            "independent first, then the author's family, the reviewer's last"
        );
    }

    /// With only two families in the lineup, the author's side still beats
    /// the reviewer agreeing with itself — and the result says which it was.
    #[test]
    fn without_a_third_family_the_authors_examines_and_says_so() {
        use crate::event::Provider;
        let reviewer = [cf_rung(Provider::Codex, "gpt-6-luna")];
        let workers = [worker(Provider::Gemini, "gemini-4-argon-high")];
        let (ladder, standing) = examiner_ladder(
            &reviewer,
            &workers,
            ModelFamily::OpenAi,
            ModelFamily::Google,
        );
        assert_eq!(standing, Independence::AuthorsFamily);
        assert_eq!(models(&ladder)[0], "gemini/gemini-4-argon-high");
    }

    #[test]
    fn a_lineup_of_one_family_examines_itself_and_says_so() {
        use crate::event::Provider;
        let reviewer = [
            cf_rung(Provider::Codex, "gpt-6-luna"),
            cf_rung(Provider::Codex, "default"),
        ];
        let (ladder, standing) =
            examiner_ladder(&reviewer, &[], ModelFamily::OpenAi, ModelFamily::Anthropic);
        assert_eq!(standing, Independence::ReviewersFamily);
        assert_eq!(ladder.len(), 2);
    }

    /// A model that is both the reviewer's fallback and a worker is one
    /// examiner, not two rungs retried in a row.
    #[test]
    fn a_model_listed_twice_examines_once() {
        use crate::event::Provider;
        let reviewer = [
            cf_rung(Provider::Codex, "gpt-6-luna"),
            cf_rung(Provider::Gemini, "gemini-4-argon-high"),
        ];
        let workers = [worker(Provider::Gemini, "gemini-4-argon-high")];
        let (ladder, _) = examiner_ladder(
            &reviewer,
            &workers,
            ModelFamily::OpenAi,
            ModelFamily::Anthropic,
        );
        assert_eq!(
            models(&ladder),
            ["gemini/gemini-4-argon-high", "codex/gpt-6-luna"]
        );
    }
}
