//! Model families: who trained a model. Cross-family review exists to keep a
//! model from marking its own homework (self-preference bias), so what must
//! differ between the author and the reviewer is the family — not the CLI
//! that happens to run them.
//!
//! The two are not the same thing. Antigravity (`agy`, the `gemini` provider)
//! is a gateway: besides Gemini it serves Claude and open-weight GPT models.
//! Judged by provider, a diff written by Claude could be "cross-family"
//! reviewed by Claude-through-Antigravity.

use crate::event::Provider;

/// Who trained a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelFamily {
    Anthropic,
    OpenAi,
    Google,
    Xai,
}

impl ModelFamily {
    /// The family of `model` as run by `provider`. The model's name decides
    /// when it names a family (a gateway provider serves several); a name that
    /// says nothing — `default`, an alias — falls back to the provider, whose
    /// own models are its vendor's.
    pub fn of(provider: Provider, model: &str) -> ModelFamily {
        Self::named_by(model).unwrap_or(match provider {
            Provider::Claude => ModelFamily::Anthropic,
            Provider::Codex => ModelFamily::OpenAi,
            Provider::Gemini => ModelFamily::Google,
            Provider::Grok => ModelFamily::Xai,
        })
    }

    /// The family of a diff's author as a run reports it — "provider/model",
    /// a bare model name or a bare provider. `None` when it names neither.
    pub fn of_author(author: &str) -> Option<ModelFamily> {
        let author = author.trim();
        let (provider, model) = author.split_once('/').unwrap_or((author, author));
        match provider.parse::<Provider>() {
            Ok(provider) => Some(Self::of(provider, model)),
            Err(_) => Self::named_by(author),
        }
    }

    /// The family a model's name names, if it names one.
    fn named_by(model: &str) -> Option<ModelFamily> {
        let name = model.to_ascii_lowercase();
        NAME_MARKERS
            .iter()
            .find(|(marker, _)| name.contains(marker))
            .map(|(_, family)| *family)
    }
}

/// What a model name must contain to name its family.
const NAME_MARKERS: [(&str, ModelFamily); 4] = [
    ("claude", ModelFamily::Anthropic),
    ("gpt", ModelFamily::OpenAi),
    ("gemini", ModelFamily::Google),
    ("grok", ModelFamily::Xai),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_providers_own_models_belong_to_its_family() {
        let cases = [
            (Provider::Claude, "claude-fable-5-2", ModelFamily::Anthropic),
            (Provider::Claude, "claude-opus-5-5", ModelFamily::Anthropic),
            (Provider::Codex, "gpt-6.1-sol", ModelFamily::OpenAi),
            (Provider::Gemini, "gemini-4-argon-high", ModelFamily::Google),
            (
                Provider::Gemini,
                "gemini-4-hydrogen-high",
                ModelFamily::Google,
            ),
            (Provider::Grok, "grok-4.7", ModelFamily::Xai),
        ];
        for (provider, model, family) in cases {
            assert_eq!(ModelFamily::of(provider, model), family, "{model}");
        }
    }

    /// Ids and display names exactly as `agy models` lists them (agy 1.5.14).
    #[test]
    fn antigravity_serves_other_families_under_the_gemini_provider() {
        let cases = [
            ("claude-opus-5-5-high", ModelFamily::Anthropic),
            ("Claude Opus 5.5 (High)", ModelFamily::Anthropic),
            ("claude-sonnet-5-5-medium", ModelFamily::Anthropic),
            ("gpt-oss-120b-medium", ModelFamily::OpenAi),
            ("GPT-OSS 120B (Medium)", ModelFamily::OpenAi),
            ("Gemini 4 Argon (High)", ModelFamily::Google),
            ("gemini-pro-orchestrator-neon", ModelFamily::Google),
        ];
        for (model, family) in cases {
            assert_eq!(ModelFamily::of(Provider::Gemini, model), family, "{model}");
        }
    }

    /// What the attached conductor passes to `review_diff`: the `model_used`
    /// a `run_worker` call reported ("provider/model"), or less.
    #[test]
    fn an_author_is_read_from_what_a_run_reports() {
        let cases = [
            ("codex/gpt-6-luna", Some(ModelFamily::OpenAi)),
            ("gemini/claude-opus-5-5-high", Some(ModelFamily::Anthropic)),
            ("gemini/default", Some(ModelFamily::Google)),
            ("  grok/grok-4.7 ", Some(ModelFamily::Xai)),
            // a bare model name, or a bare provider
            ("claude-fable-5-2", Some(ModelFamily::Anthropic)),
            ("codex", Some(ModelFamily::OpenAi)),
            // nothing to go by
            ("mystery-model", None),
            ("", None),
        ];
        for (author, family) in cases {
            assert_eq!(ModelFamily::of_author(author), family, "{author:?}");
        }
    }

    /// `default` (the CLI picks) and aliases name no family; each CLI's own
    /// default is one of its vendor's models.
    #[test]
    fn a_name_that_says_nothing_falls_back_to_the_provider() {
        let cases = [
            (Provider::Claude, "default", ModelFamily::Anthropic),
            (Provider::Claude, "sonnet", ModelFamily::Anthropic),
            (Provider::Codex, "default", ModelFamily::OpenAi),
            (Provider::Gemini, "default", ModelFamily::Google),
            (Provider::Grok, "default", ModelFamily::Xai),
        ];
        for (provider, model, family) in cases {
            assert_eq!(ModelFamily::of(provider, model), family, "{model}");
        }
    }
}
