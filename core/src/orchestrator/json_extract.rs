//! Lenient extraction of a JSON object from model output. Strategy: prefer a
//! fenced ```json block; otherwise scan '{' positions and take the FIRST
//! complete JSON value that deserializes into T (tolerates trailing prose and
//! stray braces before the object — exactly what unfenced models produce).

use serde::de::DeserializeOwned;

pub(crate) fn extract_json_object<T: DeserializeOwned>(text: &str) -> Option<T> {
    if let Some(start) = text.find("```json") {
        let rest = &text[start + 7..];
        if let Some(end) = rest.find("```") {
            if let Ok(v) = serde_json::from_str::<T>(rest[..end].trim()) {
                return Some(v);
            }
        }
        // fall through: fence existed but didn't parse — try the lenient scan
    }
    // Bounded scan over '{' candidates; StreamDeserializer stops at the value
    // end, so trailing prose after the object is fine.
    for (idx, _) in text.match_indices('{').take(32) {
        let mut iter = serde_json::Deserializer::from_str(&text[idx..]).into_iter::<T>();
        if let Some(Ok(v)) = iter.next() {
            return Some(v);
        }
    }
    None
}
