//! JSON object helpers shared by adapter conversion paths.

use serde_json::{Map, Value};

/// Inserts adapter-owned evidence without discarding a colliding provider key.
///
/// When a provider already used `key`, both the original provider value and the
/// adapter value are retained in an array under the same key. This keeps escape
/// hatch data observable without silently overwriting provider evidence.
pub(crate) fn insert_preserving_collision(
    fields: &mut Map<String, Value>,
    key: &str,
    value: Value,
) {
    if let Some(existing) = fields.remove(key) {
        fields.insert(key.to_owned(), Value::Array(vec![existing, value]));
    } else {
        fields.insert(key.to_owned(), value);
    }
}
