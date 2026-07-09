//! Integration tests for the flow definition model (Task B5): wire shape,
//! YAML/JSON round-trip, and definition-time validation.

use orchestrator::model::{
    Catchup, FlowDefinition, InputDef, InputType, OnError, OutputDef, ParallelTask, PluginTask,
    RetryPolicy, TaskDef, TaskKind, TriggerDef, ValidationErr, VarDef, from_yaml, to_yaml,
    validate,
};
use orchestrator::plugins::PluginRegistry;
use serde_json::json;

// ---------- helpers ----------

fn registry() -> PluginRegistry {
    orchestrator::plugins::testing::manifest_registry()
}

/// A minimal valid http.request task.
fn http_task(id: &str, config: serde_json::Value) -> TaskDef {
    TaskDef {
        id: id.to_string(),
        kind: TaskKind::Plugin(PluginTask {
            type_id: "http.request".to_string(),
            retry: None,
            timeout_seconds: None,
            on_error: OnError::Fail,
            config,
            outputs: vec![],
        }),
    }
}

fn output(name: &str, extract: &str) -> OutputDef {
    OutputDef {
        name: name.to_string(),
        output_type: InputType::Json,
        extract: extract.to_string(),
    }
}

fn flow_with_tasks(tasks: Vec<TaskDef>) -> FlowDefinition {
    FlowDefinition {
        name: "test-flow".to_string(),
        namespace: "default".to_string(),
        queue: "local".to_string(),
        on_worker_loss: None,
        description: String::new(),
        inputs: vec![],
        variables: vec![],
        env: vec![],
        triggers: vec![],
        tasks,
    }
}

#[track_caller]
fn assert_err(errs: &[ValidationErr], path: &str, needle: &str) {
    assert!(
        errs.iter()
            .any(|e| e.path == path && e.message.contains(needle)),
        "expected an error at `{path}` containing `{needle}`, got: {errs:#?}"
    );
}

/// The design-doc example flow (council-alert-pipeline), built in code.
fn example_flow() -> FlowDefinition {
    FlowDefinition {
        name: "council-alert-pipeline".to_string(),
        namespace: "default".to_string(),
        queue: "local".to_string(),
        on_worker_loss: None,
        description: String::new(),
        inputs: vec![InputDef {
            id: "provinces".to_string(),
            input_type: InputType::Array,
            required: true,
            default: Some("[\"ON\",\"QC\"]".to_string()),
        }],
        variables: vec![VarDef {
            id: "server".to_string(),
            value: "https://api.example.com".to_string(),
        }],
        env: vec![],
        triggers: vec![TriggerDef {
            id: "nightly".to_string(),
            trigger_type: "schedule".to_string(),
            cron: "0 3 * * *".to_string(),
            timezone: "America/Toronto".to_string(),
            catchup: Catchup::Latest,
            enabled: true,
        }],
        tasks: vec![
            TaskDef {
                id: "discover".to_string(),
                kind: TaskKind::Plugin(PluginTask {
                    type_id: "http.request".to_string(),
                    retry: Some(RetryPolicy {
                        retry_type: "exponential".to_string(),
                        max_attempts: 3,
                        base_seconds: 5,
                    }),
                    timeout_seconds: Some(60),
                    on_error: OnError::Fail,
                    config: json!({
                        "method": "GET",
                        "url": "{{ vars.server }}/api/municipalities",
                        "headers": [],
                        "body": [],
                        "raw_body": null,
                        "success_codes": "2xx"
                    }),
                    outputs: vec![OutputDef {
                        name: "ids".to_string(),
                        output_type: InputType::Array,
                        extract: "result.body.ids".to_string(),
                    }],
                }),
            },
            TaskDef {
                id: "fetch_all".to_string(),
                kind: TaskKind::Parallel(ParallelTask {
                    items: "{{ outputs.discover.ids }}".to_string(),
                    concurrency: 8,
                    tasks: vec![TaskDef {
                        id: "fetch_one".to_string(),
                        kind: TaskKind::Plugin(PluginTask {
                            type_id: "http.request".to_string(),
                            retry: None,
                            timeout_seconds: None,
                            on_error: OnError::Continue,
                            config: json!({
                                "method": "GET",
                                "url": "{{ vars.server }}/api/m/{{ taskrun.value }}"
                            }),
                            outputs: vec![],
                        }),
                    }],
                    outputs: vec![OutputDef {
                        name: "results".to_string(),
                        output_type: InputType::Array,
                        extract: "result.items".to_string(),
                    }],
                }),
            },
        ],
    }
}

// ---------- round-trip ----------

#[test]
fn yaml_round_trip_design_doc_example() {
    let def = example_flow();
    let yaml = to_yaml("council-alert-pipeline", &def).expect("to_yaml");
    let (id, back) = from_yaml(&yaml).expect("from_yaml");
    assert_eq!(id, "council-alert-pipeline");
    assert_eq!(back, def);
}

#[test]
fn yaml_export_has_id_as_first_line() {
    let yaml = to_yaml("council-alert-pipeline", &example_flow()).expect("to_yaml");
    assert!(
        yaml.starts_with("id: council-alert-pipeline\n"),
        "yaml did not start with the id line:\n{yaml}"
    );
}

#[test]
fn json_round_trip_design_doc_example() {
    let def = example_flow();
    let text = serde_json::to_string(&def).expect("serialize");
    let back: FlowDefinition = serde_json::from_str(&text).expect("deserialize");
    assert_eq!(back, def);
}

#[test]
fn from_yaml_missing_id_is_model_error() {
    let err = from_yaml("name: no-id-here\n").expect_err("must fail");
    assert!(
        err.message.contains("id"),
        "message should mention the missing id: {}",
        err.message
    );
}

// ---------- wire-shape lock ----------

#[test]
fn wire_shape_plugin_task_is_flat_with_type_discriminator() {
    let task = TaskDef {
        id: "discover".to_string(),
        kind: TaskKind::Plugin(PluginTask {
            type_id: "http.request".to_string(),
            retry: Some(RetryPolicy {
                retry_type: "exponential".to_string(),
                max_attempts: 3,
                base_seconds: 5,
            }),
            timeout_seconds: Some(60),
            on_error: OnError::Fail,
            config: json!({ "url": "https://x" }),
            outputs: vec![OutputDef {
                name: "ids".to_string(),
                output_type: InputType::Array,
                extract: "result.body.ids".to_string(),
            }],
        }),
    };
    assert_eq!(
        serde_json::to_value(&task).unwrap(),
        json!({
            "id": "discover",
            "type": "http.request",
            "retry": { "type": "exponential", "max_attempts": 3, "base_seconds": 5 },
            "timeout_seconds": 60,
            "on_error": "fail",
            "config": { "url": "https://x" },
            "outputs": [ { "name": "ids", "type": "ARRAY", "extract": "result.body.ids" } ]
        })
    );
}

#[test]
fn wire_shape_plugin_task_omits_absent_optionals() {
    let value = serde_json::to_value(http_task("t", json!({ "url": "https://x" }))).unwrap();
    let obj = value.as_object().unwrap();
    assert!(!obj.contains_key("retry"));
    assert!(!obj.contains_key("timeout_seconds"));
    assert_eq!(obj["on_error"], json!("fail"));
}

#[test]
fn wire_shape_parallel_task_is_flat_with_type_parallel() {
    let task = TaskDef {
        id: "fan".to_string(),
        kind: TaskKind::Parallel(ParallelTask {
            items: "{{ outputs.a.ids }}".to_string(),
            concurrency: 8,
            tasks: vec![http_task("child", json!({ "url": "https://x" }))],
            outputs: vec![OutputDef {
                name: "results".to_string(),
                output_type: InputType::Array,
                extract: "result.items".to_string(),
            }],
        }),
    };
    assert_eq!(
        serde_json::to_value(&task).unwrap(),
        json!({
            "id": "fan",
            "type": "parallel",
            "items": "{{ outputs.a.ids }}",
            "concurrency": 8,
            "tasks": [ {
                "id": "child",
                "type": "http.request",
                "on_error": "fail",
                "config": { "url": "https://x" },
                "outputs": []
            } ],
            "outputs": [ { "name": "results", "type": "ARRAY", "extract": "result.items" } ]
        })
    );
}

// ---------- deny unknown fields ----------

#[test]
fn deny_unknown_field_at_flow_level() {
    let err = serde_json::from_str::<FlowDefinition>(r#"{ "name": "x", "bogus": 1 }"#)
        .expect_err("must reject");
    assert!(
        err.to_string().contains("bogus"),
        "error should name the field: {err}"
    );
}

#[test]
fn deny_unknown_field_at_task_level() {
    let err = serde_json::from_str::<FlowDefinition>(
        r#"{ "name": "x", "tasks": [
            { "id": "a", "type": "http.request", "config": {}, "wat": true } ] }"#,
    )
    .expect_err("must reject");
    assert!(
        err.to_string().contains("wat"),
        "error should name the field: {err}"
    );
}

#[test]
fn deny_unknown_field_at_parallel_child_level() {
    let err = serde_json::from_str::<FlowDefinition>(
        r#"{ "name": "x", "tasks": [
            { "id": "fan", "type": "parallel", "items": "{{ inputs.a }}", "concurrency": 2,
              "tasks": [ { "id": "c", "type": "http.request", "config": {}, "sneaky": 1 } ] } ] }"#,
    )
    .expect_err("must reject");
    assert!(
        err.to_string().contains("sneaky"),
        "error should name the field: {err}"
    );
}

// ---------- validation: happy path ----------

#[test]
fn validate_design_doc_example_is_clean() {
    let errs = validate(&example_flow(), &registry());
    assert_eq!(errs, vec![], "expected no errors, got: {errs:#?}");
}

// ---------- validation: rule classes ----------

#[test]
fn validate_rejects_empty_name() {
    let mut def = flow_with_tasks(vec![]);
    def.name = "  ".to_string();
    assert_err(&validate(&def, &registry()), "name", "must not be empty");
}

#[test]
fn queue_defaults_to_local_and_omits_from_serialization() {
    // Absent `queue` deserializes to "local".
    let def: FlowDefinition =
        serde_json::from_value(json!({ "name": "f", "tasks": [] })).expect("deserialize");
    assert_eq!(def.queue, "local");
    // A default-queue flow omits `queue` on serialize (keeps YAML fixtures
    // byte-identical).
    let back = serde_json::to_value(&def).expect("serialize");
    assert!(back.get("queue").is_none(), "local queue should be omitted");
}

#[test]
fn non_local_queue_round_trips() {
    let def: FlowDefinition =
        serde_json::from_value(json!({ "name": "f", "queue": "gpu", "tasks": [] }))
            .expect("deserialize");
    assert_eq!(def.queue, "gpu");
    let back = serde_json::to_value(&def).expect("serialize");
    assert_eq!(back["queue"], json!("gpu"));
}

#[test]
fn validate_rejects_bad_queue_label() {
    let mut def = flow_with_tasks(vec![]);
    def.queue = "GPU!".to_string();
    assert_err(&validate(&def, &registry()), "queue", "invalid queue");
    // A valid label passes.
    def.queue = "gpu_1".to_string();
    assert!(
        !validate(&def, &registry())
            .iter()
            .any(|e| e.path == "queue"),
        "gpu_1 should be a valid queue label"
    );
}

#[test]
fn validate_rejects_duplicate_task_ids_globally() {
    // Top-level duplicate.
    let def = flow_with_tasks(vec![
        http_task("a", json!({ "url": "https://x" })),
        http_task("a", json!({ "url": "https://x" })),
    ]);
    assert_err(
        &validate(&def, &registry()),
        "tasks[1].id",
        "duplicate task id `a`",
    );

    // Parallel child colliding with a top-level task id.
    let def = flow_with_tasks(vec![
        http_task("a", json!({ "url": "https://x" })),
        TaskDef {
            id: "fan".to_string(),
            kind: TaskKind::Parallel(ParallelTask {
                items: "{{ outputs.a.x }}".to_string(),
                concurrency: 2,
                tasks: vec![http_task("a", json!({ "url": "https://x" }))],
                outputs: vec![],
            }),
        },
    ]);
    assert_err(
        &validate(&def, &registry()),
        "tasks[1].tasks[0].id",
        "duplicate task id `a`",
    );
}

#[test]
fn validate_rejects_bad_id_casing() {
    let mut def = flow_with_tasks(vec![http_task("BadTask", json!({ "url": "https://x" }))]);
    def.inputs = vec![InputDef {
        id: "Bad-Input".to_string(),
        input_type: InputType::String,
        required: false,
        default: None,
    }];
    let errs = validate(&def, &registry());
    assert_err(&errs, "inputs[0].id", "invalid id `Bad-Input`");
    assert_err(&errs, "tasks[0].id", "invalid id `BadTask`");
}

#[test]
fn validate_rejects_bad_cron() {
    let mut def = flow_with_tasks(vec![]);
    def.triggers = vec![
        TriggerDef {
            id: "t1".to_string(),
            trigger_type: "schedule".to_string(),
            cron: "99 * * * *".to_string(),
            timezone: "UTC".to_string(),
            catchup: Catchup::Latest,
            enabled: true,
        },
        // 6-field (seconds) patterns are not allowed: crons are 5-field.
        TriggerDef {
            id: "t2".to_string(),
            trigger_type: "schedule".to_string(),
            cron: "0 0 3 * * *".to_string(),
            timezone: "UTC".to_string(),
            catchup: Catchup::Latest,
            enabled: true,
        },
    ];
    let errs = validate(&def, &registry());
    assert_err(&errs, "triggers[0].cron", "invalid cron");
    assert_err(&errs, "triggers[1].cron", "invalid cron");
}

#[test]
fn validate_rejects_unknown_timezone() {
    let mut def = flow_with_tasks(vec![]);
    def.triggers = vec![TriggerDef {
        id: "t".to_string(),
        trigger_type: "schedule".to_string(),
        cron: "0 3 * * *".to_string(),
        timezone: "Mars/Olympus".to_string(),
        catchup: Catchup::Latest,
        enabled: true,
    }];
    assert_err(
        &validate(&def, &registry()),
        "triggers[0].timezone",
        "unknown timezone `Mars/Olympus`",
    );
}

#[test]
fn validate_rejects_non_schedule_trigger_type() {
    let mut def = flow_with_tasks(vec![]);
    def.triggers = vec![TriggerDef {
        id: "t".to_string(),
        trigger_type: "webhook".to_string(),
        cron: "0 3 * * *".to_string(),
        timezone: "UTC".to_string(),
        catchup: Catchup::Latest,
        enabled: true,
    }];
    assert_err(
        &validate(&def, &registry()),
        "triggers[0].type",
        "must be `schedule`",
    );
}

/// A schedule trigger firing on `cron` with `enabled` set, for the
/// required-input/default interplay tests.
fn schedule_trigger(id: &str, enabled: bool) -> TriggerDef {
    TriggerDef {
        id: id.to_string(),
        trigger_type: "schedule".to_string(),
        cron: "0 3 * * *".to_string(),
        timezone: "UTC".to_string(),
        catchup: Catchup::Latest,
        enabled,
    }
}

fn required_input(id: &str, default: Option<&str>) -> InputDef {
    InputDef {
        id: id.to_string(),
        input_type: InputType::String,
        required: true,
        default: default.map(str::to_string),
    }
}

#[test]
fn validate_rejects_enabled_schedule_with_required_input_lacking_default() {
    let mut def = flow_with_tasks(vec![]);
    def.inputs = vec![required_input("city", None)];
    def.triggers = vec![schedule_trigger("nightly", true)];
    assert_err(
        &validate(&def, &registry()),
        "triggers[0]",
        "scheduled runs will fail: required input `city` has no default",
    );
}

#[test]
fn validate_names_every_defaultless_required_input_per_enabled_trigger() {
    let mut def = flow_with_tasks(vec![]);
    def.inputs = vec![
        required_input("city", None),
        required_input("country", None),
        required_input("planet", Some("earth")),
    ];
    def.triggers = vec![
        schedule_trigger("nightly", true),
        schedule_trigger("noon", true),
    ];
    let errs = validate(&def, &registry());
    assert_err(
        &errs,
        "triggers[0]",
        "required inputs `city`, `country` have no default",
    );
    assert_err(
        &errs,
        "triggers[1]",
        "required inputs `city`, `country` have no default",
    );
}

#[test]
fn validate_allows_disabled_trigger_or_defaults_with_required_inputs() {
    // Disabled trigger: never fires, so a defaultless required input is fine.
    let mut def = flow_with_tasks(vec![]);
    def.inputs = vec![required_input("city", None)];
    def.triggers = vec![schedule_trigger("nightly", false)];
    assert_eq!(validate(&def, &registry()), vec![]);

    // Enabled trigger but every required input has a default: fine.
    let mut def = flow_with_tasks(vec![]);
    def.inputs = vec![required_input("city", Some("berlin"))];
    def.triggers = vec![schedule_trigger("nightly", true)];
    assert_eq!(validate(&def, &registry()), vec![]);

    // Enabled trigger with an optional defaultless input: fine.
    let mut def = flow_with_tasks(vec![]);
    def.inputs = vec![InputDef {
        id: "city".to_string(),
        input_type: InputType::String,
        required: false,
        default: None,
    }];
    def.triggers = vec![schedule_trigger("nightly", true)];
    assert_eq!(validate(&def, &registry()), vec![]);
}

#[test]
fn validate_rejects_unknown_plugin_type() {
    let def = flow_with_tasks(vec![TaskDef {
        id: "a".to_string(),
        kind: TaskKind::Plugin(PluginTask {
            type_id: "no.such.plugin".to_string(),
            retry: None,
            timeout_seconds: None,
            on_error: OnError::Fail,
            config: json!({}),
            outputs: vec![],
        }),
    }]);
    assert_err(
        &validate(&def, &registry()),
        "tasks[0].type",
        "unknown task type `no.such.plugin`",
    );
}

#[test]
fn validate_surfaces_plugin_config_errors_at_config_path() {
    // http.request requires `url`.
    let def = flow_with_tasks(vec![http_task("a", json!({}))]);
    assert_err(
        &validate(&def, &registry()),
        "tasks[0].config",
        "url is required",
    );
}

#[test]
fn validate_rejects_forward_output_reference() {
    let mut later = http_task("later", json!({ "url": "https://x" }));
    if let TaskKind::Plugin(p) = &mut later.kind {
        p.outputs = vec![output("val", "result.body.val")];
    }
    let def = flow_with_tasks(vec![
        http_task("early", json!({ "url": "{{ outputs.later.val }}" })),
        later,
    ]);
    assert_err(
        &validate(&def, &registry()),
        "tasks[0].config.url",
        "not an earlier task",
    );
}

#[test]
fn validate_rejects_undeclared_output_name_reference() {
    let mut early = http_task("early", json!({ "url": "https://x" }));
    if let TaskKind::Plugin(p) = &mut early.kind {
        p.outputs = vec![output("val", "result.body.val")];
    }
    let def = flow_with_tasks(vec![
        early,
        http_task("late", json!({ "url": "{{ outputs.early.nope }}" })),
    ]);
    assert_err(
        &validate(&def, &registry()),
        "tasks[1].config.url",
        "no declared output `nope`",
    );
}

#[test]
fn validate_rejects_taskrun_outside_parallel_children() {
    let def = flow_with_tasks(vec![http_task(
        "a",
        json!({ "url": "{{ taskrun.value }}" }),
    )]);
    assert_err(
        &validate(&def, &registry()),
        "tasks[0].config.url",
        "taskrun is only available inside parallel child tasks",
    );
}

#[test]
fn validate_scopes_sibling_outputs_inside_parallel() {
    let mut first = http_task("c1", json!({ "url": "https://x" }));
    if let TaskKind::Plugin(p) = &mut first.kind {
        p.outputs = vec![output("val", "result.body.val")];
    }
    // Second child referencing the first sibling's output: valid.
    let good = flow_with_tasks(vec![TaskDef {
        id: "fan".to_string(),
        kind: TaskKind::Parallel(ParallelTask {
            items: "literal".to_string(),
            concurrency: 2,
            tasks: vec![
                first.clone(),
                http_task("c2", json!({ "url": "{{ outputs.c1.val }}" })),
            ],
            outputs: vec![],
        }),
    }]);
    let errs = validate(&good, &registry());
    assert_eq!(
        errs,
        vec![],
        "sibling back-reference must be valid: {errs:#?}"
    );

    // First child referencing a later sibling: invalid.
    let bad = flow_with_tasks(vec![TaskDef {
        id: "fan".to_string(),
        kind: TaskKind::Parallel(ParallelTask {
            items: "literal".to_string(),
            concurrency: 2,
            tasks: vec![
                http_task("c0", json!({ "url": "{{ outputs.c1.val }}" })),
                first,
            ],
            outputs: vec![],
        }),
    }]);
    assert_err(
        &validate(&bad, &registry()),
        "tasks[0].tasks[0].config.url",
        "not an earlier task",
    );
}

#[test]
fn validate_rejects_nested_parallel() {
    let def = flow_with_tasks(vec![TaskDef {
        id: "outer".to_string(),
        kind: TaskKind::Parallel(ParallelTask {
            items: "literal".to_string(),
            concurrency: 2,
            tasks: vec![TaskDef {
                id: "inner".to_string(),
                kind: TaskKind::Parallel(ParallelTask {
                    items: "literal".to_string(),
                    concurrency: 2,
                    tasks: vec![],
                    outputs: vec![],
                }),
            }],
            outputs: vec![],
        }),
    }]);
    assert_err(
        &validate(&def, &registry()),
        "tasks[0].tasks[0].type",
        "nested parallel",
    );
}

#[test]
fn validate_rejects_concurrency_out_of_bounds() {
    for (concurrency, id) in [(0u32, "fan0"), (257u32, "fan257")] {
        let def = flow_with_tasks(vec![TaskDef {
            id: id.to_string(),
            kind: TaskKind::Parallel(ParallelTask {
                items: "literal".to_string(),
                concurrency,
                tasks: vec![http_task("c", json!({ "url": "https://x" }))],
                outputs: vec![],
            }),
        }]);
        assert_err(
            &validate(&def, &registry()),
            "tasks[0].concurrency",
            "between 1 and 256",
        );
    }
}

#[test]
fn validate_requires_at_least_one_parallel_child() {
    let def = flow_with_tasks(vec![TaskDef {
        id: "fan".to_string(),
        kind: TaskKind::Parallel(ParallelTask {
            items: "literal".to_string(),
            concurrency: 2,
            tasks: vec![],
            outputs: vec![],
        }),
    }]);
    assert_err(
        &validate(&def, &registry()),
        "tasks[0].tasks",
        "at least one child",
    );
}

#[test]
fn validate_rejects_extract_not_rooted_at_result() {
    let mut task = http_task("a", json!({ "url": "https://x" }));
    if let TaskKind::Plugin(p) = &mut task.kind {
        p.outputs = vec![output("ids", "body.ids"), output("empty", "  ")];
    }
    let errs = validate(&flow_with_tasks(vec![task]), &registry());
    assert_err(
        &errs,
        "tasks[0].outputs[0].extract",
        "must start with `result`",
    );
    assert_err(&errs, "tasks[0].outputs[1].extract", "must not be empty");
}

#[test]
fn validate_rejects_duplicate_output_names() {
    let mut task = http_task("a", json!({ "url": "https://x" }));
    if let TaskKind::Plugin(p) = &mut task.kind {
        p.outputs = vec![output("x", "result.a"), output("x", "result.b")];
    }
    assert_err(
        &validate(&flow_with_tasks(vec![task]), &registry()),
        "tasks[0].outputs[1].name",
        "duplicate output name `x`",
    );
}

#[test]
fn validate_rejects_retry_out_of_bounds() {
    let mut task = http_task("a", json!({ "url": "https://x" }));
    if let TaskKind::Plugin(p) = &mut task.kind {
        p.retry = Some(RetryPolicy {
            retry_type: "linear".to_string(),
            max_attempts: 0,
            base_seconds: 0,
        });
    }
    let errs = validate(&flow_with_tasks(vec![task]), &registry());
    assert_err(&errs, "tasks[0].retry.type", "must be `exponential`");
    assert_err(&errs, "tasks[0].retry.max_attempts", "between 1 and 20");
    assert_err(&errs, "tasks[0].retry.base_seconds", "between 1 and 3600");

    let mut task = http_task("a", json!({ "url": "https://x" }));
    if let TaskKind::Plugin(p) = &mut task.kind {
        p.retry = Some(RetryPolicy {
            retry_type: "exponential".to_string(),
            max_attempts: 21,
            base_seconds: 3601,
        });
    }
    let errs = validate(&flow_with_tasks(vec![task]), &registry());
    assert_err(&errs, "tasks[0].retry.max_attempts", "between 1 and 20");
    assert_err(&errs, "tasks[0].retry.base_seconds", "between 1 and 3600");
}

#[test]
fn validate_rejects_timeout_out_of_bounds() {
    for timeout in [0u64, 3601u64] {
        let mut task = http_task("a", json!({ "url": "https://x" }));
        if let TaskKind::Plugin(p) = &mut task.kind {
            p.timeout_seconds = Some(timeout);
        }
        assert_err(
            &validate(&flow_with_tasks(vec![task]), &registry()),
            "tasks[0].timeout_seconds",
            "between 1 and 3600",
        );
    }
}

#[test]
fn validate_rejects_unknown_template_root() {
    let def = flow_with_tasks(vec![http_task("a", json!({ "url": "{{ foo.bar }}" }))]);
    assert_err(
        &validate(&def, &registry()),
        "tasks[0].config.url",
        "unknown reference root `foo`",
    );
}

#[test]
fn validate_reports_unparseable_template_with_path() {
    let def = flow_with_tasks(vec![http_task(
        "a",
        json!({ "url": "https://x", "headers": [ { "key": "k", "value": "{{ inputs. }}" } ] }),
    )]);
    assert_err(
        &validate(&def, &registry()),
        "tasks[0].config.headers[0].value",
        "invalid template",
    );
}

#[test]
fn validate_checks_input_default_templates() {
    let mut def = flow_with_tasks(vec![]);
    def.inputs = vec![InputDef {
        id: "since".to_string(),
        input_type: InputType::Date,
        required: false,
        default: Some("{{ vars.nope }}".to_string()),
    }];
    assert_err(
        &validate(&def, &registry()),
        "inputs[0].default",
        "unknown variable `nope`",
    );
}

#[test]
fn validate_accepts_declared_env_reference() {
    let mut def = flow_with_tasks(vec![http_task(
        "fetch",
        json!({ "url": "https://{{ env.API_HOST }}/x" }),
    )]);
    def.env = vec!["API_HOST".to_string()];
    let errs = validate(&def, &registry());
    assert_eq!(errs, vec![], "declared env ref should validate, got: {errs:#?}");
}

#[test]
fn validate_rejects_undeclared_env_reference() {
    // Referenced but never declared in `env:` → the declaration is the contract.
    let def = flow_with_tasks(vec![http_task(
        "fetch",
        json!({ "url": "https://{{ env.API_HOST }}/x" }),
    )]);
    assert_err(
        &validate(&def, &registry()),
        "tasks[0].config.url",
        "undeclared env var `API_HOST`",
    );
}

#[test]
fn validate_rejects_incomplete_env_reference() {
    let mut def = flow_with_tasks(vec![http_task(
        "fetch",
        json!({ "url": "{{ env }}" }),
    )]);
    def.env = vec!["API_HOST".to_string()];
    assert_err(
        &validate(&def, &registry()),
        "tasks[0].config.url",
        "expected `env.<NAME>`",
    );
}

#[test]
fn validate_rejects_bad_env_name_and_duplicates() {
    let mut def = flow_with_tasks(vec![]);
    def.env = vec![
        "ok_NAME".to_string(),
        "bad-name".to_string(),
        "ok_NAME".to_string(),
    ];
    let errs = validate(&def, &registry());
    assert_err(&errs, "env[1]", "invalid env var name `bad-name`");
    assert_err(&errs, "env[2]", "duplicate env var `ok_NAME`");
}

#[test]
fn env_omitted_from_serialization_when_empty() {
    let def: FlowDefinition =
        serde_json::from_value(json!({ "name": "f", "tasks": [] })).expect("deserialize");
    assert!(def.env.is_empty());
    let back = serde_json::to_value(&def).expect("serialize");
    assert!(back.get("env").is_none(), "empty env should be omitted");
}

#[test]
fn env_round_trips_when_present() {
    let def: FlowDefinition =
        serde_json::from_value(json!({ "name": "f", "env": ["API_HOST"], "tasks": [] }))
            .expect("deserialize");
    assert_eq!(def.env, vec!["API_HOST".to_string()]);
    let back = serde_json::to_value(&def).expect("serialize");
    assert_eq!(back["env"], json!(["API_HOST"]));
}

// ---------- TaskDef deserialize error branches ----------

#[test]
fn task_deserialize_missing_id_names_the_field() {
    let err =
        serde_json::from_str::<TaskDef>(r#"{ "type": "http.request" }"#).expect_err("must reject");
    assert!(
        err.to_string()
            .contains("task is missing required field `id`"),
        "unexpected message: {err}"
    );
}

#[test]
fn task_deserialize_missing_type_names_the_task() {
    let err = serde_json::from_str::<TaskDef>(r#"{ "id": "a" }"#).expect_err("must reject");
    assert!(
        err.to_string()
            .contains("task `a` is missing required field `type`"),
        "unexpected message: {err}"
    );
}

#[test]
fn task_deserialize_non_string_id_is_rejected() {
    let err = serde_json::from_str::<TaskDef>(r#"{ "id": 5, "type": "http.request" }"#)
        .expect_err("must reject");
    assert!(
        err.to_string().contains("task field `id` must be a string"),
        "unexpected message: {err}"
    );
}

#[test]
fn task_deserialize_non_string_type_is_rejected() {
    let err =
        serde_json::from_str::<TaskDef>(r#"{ "id": "a", "type": 5 }"#).expect_err("must reject");
    assert!(
        err.to_string()
            .contains("task `a`: field `type` must be a string"),
        "unexpected message: {err}"
    );
}

// ---------- taskrun field scoping ----------

#[test]
fn validate_taskrun_value_paths_are_valid_inside_parallel() {
    for url in [
        "{{ taskrun.value }}",
        "{{ taskrun.value.id }}",
        "{{ taskrun.value[0] }}",
    ] {
        let def = flow_with_tasks(vec![TaskDef {
            id: "fan".to_string(),
            kind: TaskKind::Parallel(ParallelTask {
                items: "literal".to_string(),
                concurrency: 2,
                tasks: vec![http_task("c", json!({ "url": url }))],
                outputs: vec![],
            }),
        }]);
        let errs = validate(&def, &registry());
        assert_eq!(errs, vec![], "`{url}` must be valid, got: {errs:#?}");
    }
}

#[test]
fn validate_rejects_non_value_taskrun_field_inside_parallel() {
    let def = flow_with_tasks(vec![TaskDef {
        id: "fan".to_string(),
        kind: TaskKind::Parallel(ParallelTask {
            items: "literal".to_string(),
            concurrency: 2,
            tasks: vec![http_task("c", json!({ "url": "{{ taskrun.foo }}" }))],
            outputs: vec![],
        }),
    }]);
    assert_err(
        &validate(&def, &registry()),
        "tasks[0].tasks[0].config.url",
        "unknown taskrun field `foo`",
    );
}

// ---------- outputs of a parallel task are visible downstream ----------

#[test]
fn validate_allows_referencing_parallel_outputs_downstream() {
    let def = flow_with_tasks(vec![
        TaskDef {
            id: "fan".to_string(),
            kind: TaskKind::Parallel(ParallelTask {
                items: "literal".to_string(),
                concurrency: 2,
                tasks: vec![http_task("c", json!({ "url": "https://x" }))],
                outputs: vec![OutputDef {
                    name: "results".to_string(),
                    output_type: InputType::Array,
                    extract: "result.items".to_string(),
                }],
            }),
        },
        http_task("after", json!({ "url": "{{ outputs.fan.results }}" })),
    ]);
    let errs = validate(&def, &registry());
    assert_eq!(errs, vec![], "expected no errors, got: {errs:#?}");
}

// ---------- id length boundaries ----------

#[test]
fn validate_id_length_boundaries() {
    // 64 chars: accepted.
    let id64 = "a".repeat(64);
    let def = flow_with_tasks(vec![http_task(&id64, json!({ "url": "https://x" }))]);
    let errs = validate(&def, &registry());
    assert_eq!(errs, vec![], "64-char id must be valid, got: {errs:#?}");

    // 65 chars: rejected.
    let id65 = "a".repeat(65);
    let def = flow_with_tasks(vec![http_task(&id65, json!({ "url": "https://x" }))]);
    assert_err(&validate(&def, &registry()), "tasks[0].id", "invalid id");

    // Empty: rejected.
    let def = flow_with_tasks(vec![http_task("", json!({ "url": "https://x" }))]);
    assert_err(&validate(&def, &registry()), "tasks[0].id", "invalid id");
}
