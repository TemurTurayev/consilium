//! Lenient extraction of a JSON object from model output. Strategy: prefer
//! fenced ```json blocks, falling back to a bounded scan over '{' positions.
//! In both passes the LAST parseable candidate wins: models often echo the
//! requested format example (a valid decoy) before the real answer, and final
//! answers come last. Trailing prose and stray braces are tolerated.

use serde::de::DeserializeOwned;

pub(crate) fn extract_json_object<T: DeserializeOwned>(text: &str) -> Option<T> {
    // Prefer the LAST parseable fenced block: models often echo the requested
    // format example before the real answer; final answers come last.
    let mut best: Option<T> = None;
    let mut rest = text;
    while let Some(start) = rest.find("```json") {
        rest = &rest[start + 7..];
        let Some(end) = rest.find("```") else { break };
        if let Ok(v) = serde_json::from_str::<T>(rest[..end].trim()) {
            best = Some(v);
        }
        rest = &rest[end + 3..];
    }
    if best.is_some() {
        return best;
    }
    // Bounded scan over '{' candidates; last parseable wins for the same reason.
    // StreamDeserializer stops at the value end, so trailing prose is fine.
    for (idx, _) in text.match_indices('{').take(64) {
        let mut iter = serde_json::Deserializer::from_str(&text[idx..]).into_iter::<T>();
        if let Some(Ok(v)) = iter.next() {
            best = Some(v);
        }
    }
    best
}
