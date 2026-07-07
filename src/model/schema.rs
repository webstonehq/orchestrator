//! The flow JSON Schema, assembled at runtime for editor autocomplete.
//!
//! Served at `GET /api/flow.schema.json` and fed to the YAML editor's
//! `codemirror-json-schema` extension. Two generated sources, neither
//! hand-maintained:
//!
//! - The **envelope** (`name`, `inputs`, `variables`, `triggers`, `tasks`
//!   scaffolding, `outputs`, retry/on_error/…) comes from `schemars` derives
//!   on the model types, so it can't drift from the Rust structs.
//! - Each plugin's **`config`** schema is derived from its
//!   [`PluginManifest`] fields — the same data the task inspector renders
//!   from — so it can't drift from the real config shape. The `tasks` list
//!   becomes a discriminated `oneOf` on `type`, assembled from the live
//!   [`PluginRegistry`], so installed plugins appear in autocomplete for free.

use schemars::SchemaGenerator;
use serde_json::{Map, Value, json};

use crate::model::{FlowDefinition, OnError, OutputDef, RetryPolicy};
use crate::plugins::{FieldSpec, PluginManifest, PluginRegistry, Widget};

/// Assemble the complete flow JSON Schema for a registry's installed plugins.
///
/// The result describes a whole flow YAML document (including the top-level
/// `id` key that lives outside [`crate::model::FlowDefinition`]). The `tasks`
/// list is a discriminated `oneOf` on `type`: one branch per registered
/// plugin plus the built-in `parallel` fan-out.
pub fn flow_json_schema(registry: &PluginRegistry) -> Value {
    let mut generator = SchemaGenerator::default();

    // Types the Task branches reference but that `FlowDefinition` doesn't pull
    // into `$defs` on its own (its `tasks` field resolves to the runtime
    // `Task` placeholder). Registering them first lands them in the shared
    // `$defs` so the branches can `$ref` them — no drift from the model.
    let retry_ref = to_value(generator.subschema_for::<RetryPolicy>());
    let output_ref = to_value(generator.subschema_for::<OutputDef>());
    let on_error_ref = to_value(generator.subschema_for::<OnError>());

    // Envelope: the whole FlowDefinition schema (properties + nested `$defs`)
    // straight from the derives.
    let mut root = to_value(generator.into_root_schema_for::<FlowDefinition>());

    let defs = root
        .get_mut("$defs")
        .and_then(Value::as_object_mut)
        .expect("FlowDefinition references named types, so $defs exists");

    // The runtime piece: replace the `Task` placeholder with a discriminated
    // union over installed plugins plus the built-in `parallel` fan-out.
    let mut branches: Vec<Value> = registry
        .manifests()
        .iter()
        .map(|m| plugin_task_branch(m, &retry_ref, &on_error_ref, &output_ref))
        .collect();
    branches.push(parallel_task_branch(&output_ref));
    defs.insert("Task".to_string(), json!({ "oneOf": branches }));

    // The top-level `id` key lives in the YAML wrapper, outside FlowDefinition
    // (see `model::yaml`). Add it to the root object and require it.
    let props = root
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .expect("FlowDefinition is an object schema");
    props.insert(
        "id".to_string(),
        json!({ "type": "string", "description": "Unique flow id (`[a-z][a-z0-9_]*`)." }),
    );
    match root.get_mut("required").and_then(Value::as_array_mut) {
        Some(required) => required.insert(0, json!("id")),
        None => {
            root["required"] = json!(["id"]);
        }
    }

    root
}

/// One plugin task branch: the flat `id`/`type` envelope plus the plugin's
/// config schema. Mirrors `TaskDef`'s `Serialize` impl for a plugin task.
fn plugin_task_branch(
    manifest: &PluginManifest,
    retry_ref: &Value,
    on_error_ref: &Value,
    output_ref: &Value,
) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "id": { "type": "string", "description": "Task id (`[a-z][a-z0-9_]*`)." },
            "type": { "const": manifest.type_id, "description": manifest.description },
            "retry": retry_ref,
            "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 3600 },
            "on_error": on_error_ref,
            "config": plugin_config_schema(manifest),
            "outputs": { "type": "array", "items": output_ref },
        },
        "required": ["id", "type"],
    })
}

/// The built-in `parallel` fan-out branch. Recurses into `#/$defs/Task`.
fn parallel_task_branch(output_ref: &Value) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "id": { "type": "string", "description": "Task id (`[a-z][a-z0-9_]*`)." },
            "type": { "const": "parallel", "description": "Fan out child tasks over an items array." },
            "items": { "type": "string", "description": "Template that renders to the array to fan out over." },
            "concurrency": { "type": "integer", "minimum": 1, "maximum": 256 },
            "tasks": { "type": "array", "items": { "$ref": "#/$defs/Task" } },
            "outputs": { "type": "array", "items": output_ref },
        },
        "required": ["id", "type", "items", "concurrency"],
    })
}

/// Wrap a property schema so an explicit `null` also validates, keeping any
/// `description` on the outer schema so hover docs survive.
fn nullable(schema: Value) -> Value {
    let mut inner = schema.as_object().cloned().unwrap_or_default();
    let description = inner.remove("description");
    let mut wrapped = json!({ "anyOf": [Value::Object(inner), { "type": "null" }] });
    if let Some(d) = description {
        wrapped["description"] = d;
    }
    wrapped
}

/// Convert a schemars `Schema` to a plain JSON value.
fn to_value(schema: schemars::Schema) -> Value {
    schema.to_value()
}

/// Map one plugin config field to a JSON Schema property.
///
/// `template: true` fields stay `string` even for numbers — `{{ … }}`
/// expressions are strings until the engine renders them, so constraining
/// them to `number` would flag valid templates as errors.
fn field_to_schema(field: &FieldSpec) -> Value {
    let mut schema: Map<String, Value> = match field.widget {
        Widget::Toggle => json!({ "type": "boolean" }),
        Widget::Number | Widget::Duration => {
            let mut m = json!({ "type": "number" });
            if let Some(min) = field.min {
                m["minimum"] = json!(min);
            }
            if let Some(max) = field.max {
                m["maximum"] = json!(max);
            }
            m
        }
        Widget::Select => json!({
            "type": "string",
            "enum": field.options.clone().unwrap_or_default(),
        }),
        Widget::Keyvalue => json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "key": { "type": "string" },
                    "value": { "type": "string" },
                },
                "required": ["key", "value"],
                "additionalProperties": false,
            },
        }),
        // Plain / template / multi-line strings. Template-enabled numeric-looking
        // fields stay `string` on purpose (see the doc comment).
        Widget::Template | Widget::Text | Widget::Code => json!({ "type": "string" }),
    }
    .as_object()
    .cloned()
    .expect("widget schema literals are objects");

    if !field.help.is_empty() {
        schema.insert("description".to_string(), json!(field.help));
    }
    Value::Object(schema)
}

/// Build the `config` object schema for a plugin from its manifest fields.
fn plugin_config_schema(manifest: &PluginManifest) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for field in &manifest.fields {
        let mut prop = field_to_schema(field);
        if field.required {
            required.push(Value::String(field.key.clone()));
        } else {
            // An optional field may be explicitly `null` (an authored "unset",
            // e.g. `raw_body: null`), so don't pin its type.
            prop = nullable(prop);
        }
        properties.insert(field.key.clone(), prop);
    }
    let mut schema = json!({
        "type": "object",
        "properties": properties,
        // Permissive: a plugin whose manifest under-describes its config must
        // never produce false "unknown key" diagnostics.
        "additionalProperties": true,
    });
    if !required.is_empty() {
        schema["required"] = Value::Array(required);
    }
    schema
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Minimal FieldSpec for a widget, mirroring the plugin `field` helper.
    fn field(key: &str, widget: Widget, required: bool, template: bool) -> FieldSpec {
        FieldSpec {
            key: key.to_string(),
            label: key.to_string(),
            widget,
            required,
            default: Value::Null,
            help: String::new(),
            options: None,
            min: None,
            max: None,
            template,
        }
    }

    #[test]
    fn toggle_maps_to_boolean() {
        let s = field_to_schema(&field("dry", Widget::Toggle, false, false));
        assert_eq!(s["type"], json!("boolean"));
    }

    #[test]
    fn number_maps_to_number_with_bounds() {
        let mut f = field("n", Widget::Number, false, false);
        f.min = Some(1.0);
        f.max = Some(10.0);
        let s = field_to_schema(&f);
        assert_eq!(s["type"], json!("number"));
        assert_eq!(s["minimum"], json!(1.0));
        assert_eq!(s["maximum"], json!(10.0));
    }

    #[test]
    fn duration_maps_to_number() {
        let s = field_to_schema(&field("timeout", Widget::Duration, false, false));
        assert_eq!(s["type"], json!("number"));
    }

    #[test]
    fn select_maps_to_string_enum() {
        let mut f = field("method", Widget::Select, true, false);
        f.options = Some(vec!["GET".into(), "POST".into()]);
        let s = field_to_schema(&f);
        assert_eq!(s["type"], json!("string"));
        assert_eq!(s["enum"], json!(["GET", "POST"]));
    }

    #[test]
    fn keyvalue_maps_to_array_of_pairs() {
        let s = field_to_schema(&field("headers", Widget::Keyvalue, false, true));
        assert_eq!(s["type"], json!("array"));
        assert_eq!(s["items"]["properties"]["key"]["type"], json!("string"));
        assert_eq!(s["items"]["properties"]["value"]["type"], json!("string"));
    }

    #[test]
    fn template_text_code_map_to_string() {
        for w in [Widget::Template, Widget::Text, Widget::Code] {
            let s = field_to_schema(&field("x", w, false, true));
            assert_eq!(s["type"], json!("string"));
        }
    }

    #[test]
    fn help_becomes_description() {
        let mut f = field("url", Widget::Template, true, true);
        f.help = "Request URL".to_string();
        let s = field_to_schema(&f);
        assert_eq!(s["description"], json!("Request URL"));
    }

    #[test]
    fn config_schema_collects_properties_and_required() {
        let manifest = PluginManifest {
            type_id: "t".into(),
            label: "T".into(),
            description: String::new(),
            icon: "box".into(),
            color: "#fff".into(),
            fields: vec![
                field("url", Widget::Template, true, true),
                field("dry", Widget::Toggle, false, false),
            ],
        };
        let s = plugin_config_schema(&manifest);
        assert_eq!(s["type"], json!("object"));
        // Required field: pinned to its type.
        assert_eq!(s["properties"]["url"]["type"], json!("string"));
        // Optional field: nullable, so its type sits under anyOf.
        assert_eq!(s["properties"]["dry"]["anyOf"][0]["type"], json!("boolean"));
        assert_eq!(s["properties"]["dry"]["anyOf"][1]["type"], json!("null"));
        assert_eq!(s["required"], json!(["url"]));
        // Permissive: a plugin whose manifest under-describes config must not
        // trigger false "unknown key" errors.
        assert_eq!(s["additionalProperties"], json!(true));
    }

    // -- full assembly --------------------------------------------------------

    /// The closed set of values a schema node allows, whether expressed as a
    /// flat `enum` array or (schemars' shape for documented enums) a `oneOf` of
    /// `const`s. Empty when the node isn't a closed value set — which is also
    /// what happens if the node is an unresolved `$ref`, the exact case that
    /// breaks value autocomplete.
    fn value_set(node: &Value) -> Vec<String> {
        if let Some(values) = node.get("enum").and_then(Value::as_array) {
            return values.iter().filter_map(|v| v.as_str().map(String::from)).collect();
        }
        if let Some(branches) = node.get("oneOf").and_then(Value::as_array) {
            return branches
                .iter()
                .filter_map(|b| b.get("const").and_then(Value::as_str).map(String::from))
                .collect();
        }
        Vec::new()
    }

    #[test]
    fn catchup_values_are_inline_for_completion() {
        // `codemirror-json-schema` can't resolve `$ref`s when proposing values,
        // so enum-typed properties must carry their values inline at the use
        // site — here, a trigger's `catchup`.
        let s = flow_json_schema(&PluginRegistry::builtin());
        let catchup = &s["properties"]["triggers"]["items"]["properties"]["catchup"];
        let mut values = value_set(catchup);
        values.sort();
        assert_eq!(values, vec!["all", "latest", "none"]);
    }

    #[test]
    fn input_type_values_are_inline_for_completion() {
        let s = flow_json_schema(&PluginRegistry::builtin());
        let ty = &s["properties"]["inputs"]["items"]["properties"]["type"];
        let mut values = value_set(ty);
        values.sort();
        assert_eq!(values, vec!["ARRAY", "BOOLEAN", "DATE", "INT", "JSON", "STRING"]);
    }

    #[test]
    fn on_error_values_are_inline_for_completion() {
        let s = flow_json_schema(&PluginRegistry::builtin());
        let http = task_branch(&s, "http.request").expect("http.request branch");
        let mut values = value_set(&http["properties"]["on_error"]);
        values.sort();
        assert_eq!(values, vec!["continue", "fail"]);
    }

    #[test]
    fn select_config_values_are_inline_for_completion() {
        // Config Select fields are emitted inline already; guard that path too.
        let s = flow_json_schema(&PluginRegistry::builtin());
        let http = task_branch(&s, "http.request").expect("http.request branch");
        let method = &http["properties"]["config"]["properties"]["method"];
        assert!(value_set(method).contains(&"GET".to_string()));
    }

    /// Find the `oneOf` task branch whose discriminating `type` const is `id`.
    fn task_branch<'a>(schema: &'a Value, id: &str) -> Option<&'a Value> {
        schema["$defs"]["Task"]["oneOf"]
            .as_array()?
            .iter()
            .find(|b| b["properties"]["type"]["const"] == json!(id))
    }

    #[test]
    fn root_describes_a_flow_document() {
        let s = flow_json_schema(&PluginRegistry::builtin());
        assert_eq!(s["type"], json!("object"));
        // Top-level `id` (YAML-only, outside FlowDefinition) is present + required.
        assert_eq!(s["properties"]["id"]["type"], json!("string"));
        assert_eq!(s["properties"]["name"]["type"], json!("string"));
        let required = s["required"].as_array().expect("root has required");
        assert!(required.contains(&json!("id")));
        assert!(required.contains(&json!("name")));
    }

    #[test]
    fn tasks_reference_the_task_def() {
        let s = flow_json_schema(&PluginRegistry::builtin());
        assert_eq!(
            s["properties"]["tasks"]["items"]["$ref"],
            json!("#/$defs/Task")
        );
        assert!(s["$defs"]["Task"]["oneOf"].is_array());
    }

    #[test]
    fn one_task_branch_per_plugin_plus_parallel() {
        let registry = PluginRegistry::builtin();
        let s = flow_json_schema(&registry);
        let branches = s["$defs"]["Task"]["oneOf"].as_array().unwrap();
        assert_eq!(branches.len(), registry.manifests().len() + 1);
    }

    #[test]
    fn plugin_branch_carries_config_and_common_fields() {
        let s = flow_json_schema(&PluginRegistry::builtin());
        let http = task_branch(&s, "http.request").expect("http.request branch");
        // Discriminator pinned to a const so autocomplete offers the type.
        assert_eq!(http["properties"]["type"]["const"], json!("http.request"));
        // Config shape flows from the manifest.
        assert_eq!(
            http["properties"]["config"]["properties"]["url"]["type"],
            json!("string")
        );
        // Common task fields present.
        assert!(http["properties"]["id"].is_object());
        assert!(http["properties"]["outputs"].is_object());
        assert!(http["properties"]["on_error"].is_object());
    }

    /// The drift guard: every real, valid flow we ship must validate against
    /// the assembled schema. schemars covers *additive* drift automatically;
    /// this covers the other direction — a model/manifest change that makes a
    /// real flow stop matching the schema fails here first.
    #[test]
    fn real_flows_validate_against_the_schema() {
        let schema = flow_json_schema(&PluginRegistry::builtin());
        let validator = jsonschema::validator_for(&schema).expect("schema compiles");

        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let flows = [
            format!("{manifest_dir}/examples/demo-flow.yaml"),
            format!(
                "{manifest_dir}/ui/src/lib/builder/fixtures/council_alert_pipeline.export.yaml"
            ),
            format!("{manifest_dir}/ui/src/lib/builder/fixtures/edge_case.export.yaml"),
        ];

        for path in flows {
            let yaml =
                std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            let doc: Value =
                serde_yaml_ng::from_str(&yaml).unwrap_or_else(|e| panic!("parse {path}: {e}"));
            let errors: Vec<String> = validator.iter_errors(&doc).map(|e| e.to_string()).collect();
            assert!(
                errors.is_empty(),
                "{path} does not validate against the flow schema:\n  {}",
                errors.join("\n  ")
            );
        }
    }

    #[test]
    fn parallel_branch_recurses_into_task() {
        let s = flow_json_schema(&PluginRegistry::builtin());
        let par = task_branch(&s, "parallel").expect("parallel branch");
        assert_eq!(par["properties"]["type"]["const"], json!("parallel"));
        assert_eq!(
            par["properties"]["tasks"]["items"]["$ref"],
            json!("#/$defs/Task")
        );
        assert!(par["properties"]["items"].is_object());
        assert!(par["properties"]["concurrency"].is_object());
    }

    #[test]
    fn config_schema_omits_required_when_none() {
        let manifest = PluginManifest {
            type_id: "t".into(),
            label: "T".into(),
            description: String::new(),
            icon: "box".into(),
            color: "#fff".into(),
            fields: vec![field("opt", Widget::Text, false, false)],
        };
        let s = plugin_config_schema(&manifest);
        assert!(s.get("required").is_none());
    }
}
