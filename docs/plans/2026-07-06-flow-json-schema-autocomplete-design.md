# Flow YAML autocomplete via a generated JSON Schema

Date: 2026-07-06

## Goal

Give the flow YAML editor schema-driven autocomplete, hover docs, and inline
validation — and have it cover **installed plugins**, with **no drift** from
the Rust model or the plugin manifests.

## Decisions

- **Editor**: keep the existing hand-built CodeMirror 6 editor
  (`ui/src/lib/builder/YamlPane.svelte`). Add schema support via
  `codemirror-json-schema`'s `yamlSchema` extension. No Monaco, no web worker,
  no meta-package — preserves the minimal single-file build and the existing
  two-way builder<->editor sync.
- **Schema is generated, served at runtime** from `GET /api/flow.schema.json`,
  assembled from the live `PluginRegistry`. A static bundled file can't reflect
  which plugins are installed in a given binary; an endpoint reading the
  registry can.
- **Envelope** derived from the Rust model with `schemars` (added dependency).
- **Per-plugin `config`** derived from each plugin's existing
  `PluginManifest.fields` — the same `FieldSpec` data the task inspector
  already renders from, so it cannot drift without the inspector also breaking.
- **Schema diagnostics are warnings, non-blocking**: they never set
  `parseError` or block builder sync. The visual builder stays authoritative on
  what counts as a hard error.

## Architecture

```
                 ┌─────────────────────────────────────────────┐
   Rust model ──▶│ schemars derive  → envelope schema ($defs)   │
 (flow.rs types) │                                              │
                 │ PluginRegistry   → per-plugin config schema  │──▶ GET /api/flow.schema.json
 plugin manifests│  (FieldSpec map)   + tasks `oneOf` on `type` │        (assembled per request)
 (mod.rs)        └─────────────────────────────────────────────┘
                                                                        │
                                      fetch once on mount               ▼
                        YamlPane.svelte  ◀── yamlSchema(schema) ── codemirror-json-schema
                        (completion / hover / warning diagnostics)
```

### Two generated sources

**1. Envelope — schemars.** Derive `JsonSchema` on the plain model structs and
enums: `InputDef`, `VarDef`, `TriggerDef`, `OutputDef`, `RetryPolicy`,
`InputType`, `Catchup`, `OnError`, and `FlowDefinition` itself. schemars honors
the existing serde attributes (`rename`, `default`, `deny_unknown_fields`,
`rename_all = "UPPERCASE"`, etc.), so the generated schema matches the wire
shape. Add a field or an enum variant and the schema changes automatically —
additive drift is impossible.

`TaskDef` has hand-written `Serialize`/`Deserialize` (a flat discriminated
union) *and* its real shape depends on the runtime registry, so it can't be
derived at compile time. Give it a one-line manual impl:

```rust
impl JsonSchema for TaskDef {
    fn schema_name() -> String { "Task".into() }
    // emit a bare `$ref` to "#/$defs/Task"; the real Task def is injected at runtime
}
```

The root schema thus references `#/$defs/Task`, which the handler fills in.

**2. Per-plugin config + tasks union — the registry.** In the handler, walk
`registry.manifests()` and build the `Task` `oneOf` from scratch (this is the
only inherently-runtime piece):

- One branch per plugin:
  `{ id, type: const "<type_id>", retry?, timeout_seconds?, on_error?, config, outputs }`
  where `config` is `{ type: object, properties, required }` built from the
  plugin's `FieldSpec`s.
- The built-in `parallel` branch:
  `{ id, type: const "parallel", items, concurrency, tasks: { $ref: "#/$defs/Task" }, outputs }`
  (recursive via the same `$ref`).

The handler injects this assembled `Task` schema into the root's `$defs`, and
adds the top-level `id` field (which lives outside `FlowDefinition`; see
`model/yaml.rs`). Response is served with `Content-Type: application/json`.

### FieldSpec → JSON Schema property

A small pure function, unit-tested independently:

| Widget            | JSON Schema                                            |
|-------------------|--------------------------------------------------------|
| `Toggle`          | `{ "type": "boolean" }`                                |
| `Number`, `Duration` | `{ "type": "number", "minimum": min, "maximum": max }` |
| `Select`          | `{ "type": "string", "enum": options }`                |
| `Keyvalue`        | `{ "type": "array", "items": { key, value } }`         |
| `Template`, `Text`, `Code` | `{ "type": "string" }`                        |

Rules:
- `description` comes from `FieldSpec.help`; `required` keys collected into the
  object's `required` array.
- `template: true` fields stay `string` **even for numbers** — `{{ … }}`
  expressions are strings until rendered, so constraining them to `number`
  would flag valid templates as errors.
- Config objects are `additionalProperties: true` (permissive) so a plugin
  whose manifest doesn't fully enumerate its config never produces false
  errors.

## Frontend wiring

- Add `codemirror-json-schema` to `ui/package.json`.
- In `YamlPane.svelte` `onMount`: `fetch('/api/flow.schema.json')` once, then
  include `yamlSchema(schema)` in the extensions list next to `yaml()`.
- The schema's diagnostics surface as CM6 **warnings**. The existing
  structural-parse gate (`applyEditorText` → `parseError`) is unchanged and
  still owns the `yaml error` status and the builder-sync block. Schema
  warnings render distinctly (e.g. warning severity) and are advisory only.
- If the fetch fails, the editor degrades to today's behavior (plain YAML,
  no autocomplete) — no hard dependency on the endpoint.

## Drift guard (test)

A Rust test builds the schema from the real `PluginRegistry::builtin()` and
validates the shipped example flows against it with the `jsonschema` crate:

- `examples/demo-flow.yaml`
- `ui/src/lib/builder/fixtures/council_alert_pipeline.export.yaml`
- `ui/src/lib/builder/fixtures/edge_case.export.yaml`

schemars covers *additive* drift automatically; this test covers the other
direction — "a real, valid flow the model accepts must still validate against
the schema." If the model or a manifest changes shape such that a real flow no
longer matches, the test fails.

Also unit-test the `FieldSpec` → property mapping directly (each widget), and
assert the assembled `Task` `oneOf` contains one branch per registered plugin
plus `parallel`.

## Out of scope (YAGNI)

- No autocomplete for `{{ … }}` template expression *contents* (that's the
  inspector's expression picker's job; schema treats them as opaque strings).
- No per-plugin custom keywords beyond what `FieldSpec` already expresses.
- No client-side schema caching/versioning; fetch-on-mount is enough for a
  personal-scale tool.

## Files touched

- `Cargo.toml` — add `schemars`; add `jsonschema` (dev-dependency).
- `src/model/flow.rs` — `#[derive(JsonSchema)]` on model types; manual
  `JsonSchema` for `TaskDef`.
- `src/api/misc.rs` (or a new `src/api/schema.rs`) — the
  `flow.schema.json` handler + `FieldSpec` mapping + registry assembly.
- `src/api/mod.rs` — route registration (if a new module).
- `ui/package.json` — add `codemirror-json-schema`.
- `ui/src/lib/builder/YamlPane.svelte` — fetch schema, add `yamlSchema`
  extension.
- Tests: schema drift test (Rust), FieldSpec-mapping unit tests (Rust).
</content>
</invoke>
