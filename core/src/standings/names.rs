//! One name per model across everything a run records about it.

/// The provider prefixes a worker label starts with (`provider-model`).
const PROVIDERS: [&str; 4] = ["claude", "codex", "gemini", "grok"];

/// The name a model goes by in the standings: "provider/model". Newer runs
/// record that outright (`model_used`); older ones only the worker label
/// (`provider-model`, with `#2` on a duplicate), which is read back into it.
pub(super) fn model_name(model_used: Option<&str>, label: &str) -> String {
    if let Some(model) = model_used.map(str::trim).filter(|model| !model.is_empty()) {
        return model.to_string();
    }
    let label = label.trim();
    let base = label
        .rsplit_once('#')
        .filter(|(_, count)| !count.is_empty() && count.chars().all(|c| c.is_ascii_digit()))
        .map_or(label, |(base, _)| base);
    PROVIDERS
        .iter()
        .find_map(|provider| {
            let model = base.strip_prefix(provider)?.strip_prefix('-')?;
            (!model.is_empty()).then(|| format!("{provider}/{model}"))
        })
        .unwrap_or_else(|| base.to_string())
}

/// A string field of a saved run, if it holds text.
pub(super) fn text<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// An array field of a saved run; empty when it is absent.
pub(super) fn list<'a>(value: &'a serde_json::Value, key: &str) -> &'a [serde_json::Value] {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Whether a saved run is of `kind`.
pub(super) fn is_kind(run: &serde_json::Value, kind: &str) -> bool {
    text(run, "kind") == Some(kind)
}
