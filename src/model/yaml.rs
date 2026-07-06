//! YAML export/import of flow definitions.
//!
//! The YAML shape is the JSON wire shape plus one extra top-level key: the
//! flow `id`, written as the first line on export and extracted on import.

use serde_json::Value;

use super::{FlowDefinition, ModelError};

/// Serialize a flow definition to YAML with `id: <flow_id>` as the first
/// key.
pub fn to_yaml(flow_id: &str, def: &FlowDefinition) -> Result<String, ModelError> {
    let id_scalar = serde_yaml_ng::to_string(flow_id)
        .map_err(|e| ModelError::new(format!("cannot serialize flow id: {e}")))?;
    let id_scalar = id_scalar.trim_end();
    if id_scalar.contains('\n') {
        return Err(ModelError::new("flow id must be a single-line string"));
    }
    let body = serde_yaml_ng::to_string(def)
        .map_err(|e| ModelError::new(format!("cannot serialize flow definition: {e}")))?;
    Ok(format!("id: {id_scalar}\n{body}"))
}

/// Parse a YAML document into its top-level flow `id` and the definition.
pub fn from_yaml(yaml: &str) -> Result<(String, FlowDefinition), ModelError> {
    let mut doc: Value =
        serde_yaml_ng::from_str(yaml).map_err(|e| ModelError::new(format!("invalid YAML: {e}")))?;
    let Some(fields) = doc.as_object_mut() else {
        return Err(ModelError::new("flow document must be a mapping"));
    };
    let id = match fields.remove("id") {
        Some(Value::String(s)) => s,
        Some(_) => return Err(ModelError::new("top-level `id` must be a string")),
        None => return Err(ModelError::new("missing top-level `id`")),
    };
    let def: FlowDefinition = serde_json::from_value(doc)
        .map_err(|e| ModelError::new(format!("invalid flow definition: {e}")))?;
    Ok((id, def))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins `to_yaml` to the fixtures shared with the UI builder's
    /// client-side YAML renderer (`ui/src/lib/builder/yaml.ts`), which aims
    /// for byte-identical output. If the serialization here ever changes,
    /// this test fails first; regenerating the fixtures from a live server
    /// then makes the TS test (`yaml.test.ts`) catch the renderer's drift.
    #[test]
    fn to_yaml_matches_ui_builder_fixtures() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/ui/src/lib/builder/fixtures");
        for name in ["council_alert_pipeline", "edge_case"] {
            let get = std::fs::read_to_string(format!("{dir}/{name}.get.json"))
                .unwrap_or_else(|e| panic!("read {name}.get.json: {e}"));
            let expected = std::fs::read_to_string(format!("{dir}/{name}.export.yaml"))
                .unwrap_or_else(|e| panic!("read {name}.export.yaml: {e}"));
            let doc: serde_json::Value = serde_json::from_str(&get).unwrap();
            let id = doc["id"].as_str().expect("fixture has a string id");
            let def: FlowDefinition = serde_json::from_value(doc["definition"].clone())
                .unwrap_or_else(|e| panic!("fixture {name} definition: {e}"));
            assert_eq!(
                to_yaml(id, &def).unwrap(),
                expected,
                "to_yaml drifted from fixture {name} — regenerate the \
                 ui/src/lib/builder/fixtures pair from a live server export"
            );
        }
    }
}
