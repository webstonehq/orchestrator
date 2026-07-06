//! Built-in `http.request` plugin.
//!
//! Sends a single HTTP request and returns `{status, headers, body}` as the
//! task result. Success is decided by the `success_codes` field (comma list
//! of exact codes and `Nxx` classes, default `2xx`); redirects are *not*
//! followed, so 3xx responses can be matched explicitly.

use std::time::Instant;

use async_trait::async_trait;
use reqwest::header::{CONTENT_TYPE, HeaderName, HeaderValue};
use serde_json::{Map, Value, json};

use super::{FieldSpec, PluginManifest, TaskContext, TaskError, TaskPlugin, Widget};

const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

/// The `http.request` task plugin.
pub struct HttpPlugin {
    client: reqwest::Client,
}

impl HttpPlugin {
    /// Create the plugin with its shared HTTP client (no timeout — the
    /// engine owns timeouts; no redirect following — see module docs).
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client construction cannot fail with these options");
        Self { client }
    }
}

impl Default for HttpPlugin {
    fn default() -> Self {
        Self::new()
    }
}

/// One entry of a parsed `success_codes` spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodeSpec {
    /// A status class, e.g. `2xx` → `Class(2)`.
    Class(u16),
    /// One exact status code, e.g. `301`.
    Exact(u16),
}

impl CodeSpec {
    fn matches(self, status: u16) -> bool {
        match self {
            CodeSpec::Class(n) => status / 100 == n,
            CodeSpec::Exact(c) => status == c,
        }
    }
}

/// Parse a `success_codes` spec: a comma list of exact codes (`404`) and
/// classes (`2xx`). Blank input means the default, `2xx`.
fn parse_success_codes(spec: &str) -> Result<Vec<CodeSpec>, String> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Ok(vec![CodeSpec::Class(2)]);
    }
    let mut out = Vec::new();
    for part in trimmed.split(',') {
        let entry = part.trim().to_ascii_lowercase();
        let parsed = if let Some(class) = entry.strip_suffix("xx") {
            class
                .parse::<u16>()
                .ok()
                .and_then(|n| (1..=5).contains(&n).then_some(CodeSpec::Class(n)))
        } else {
            entry
                .parse::<u16>()
                .ok()
                .and_then(|c| (100..=599).contains(&c).then_some(CodeSpec::Exact(c)))
        };
        match parsed {
            Some(p) => out.push(p),
            None => {
                return Err(format!(
                    "success_codes: invalid entry \"{}\" (expected a status code like 404 or a class like 2xx)",
                    part.trim()
                ));
            }
        }
    }
    Ok(out)
}

/// String form of a JSON value for header values and the URL: strings pass
/// through, everything else becomes compact JSON.
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// An error message including the full source chain (reqwest hides the
/// interesting connect/DNS detail in `source()`).
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut msg = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        src = s.source();
    }
    msg
}

/// Extract `[{key, value}]` pairs from an optional config field.
fn kv_pairs(field: &str, v: Option<&Value>) -> Result<Vec<(String, Value)>, String> {
    let arr = match v {
        None | Some(Value::Null) => return Ok(vec![]),
        Some(Value::Array(a)) => a,
        Some(_) => {
            return Err(format!(
                "{field} must be an array of {{key, value}} objects"
            ));
        }
    };
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let obj = item
            .as_object()
            .ok_or_else(|| format!("{field}[{i}] must be a {{key, value}} object"))?;
        let key = match obj.get("key").and_then(Value::as_str) {
            Some(k) if !k.is_empty() => k.to_string(),
            _ => return Err(format!("{field}[{i}].key must be a non-empty string")),
        };
        let value = obj.get("value").cloned().unwrap_or(Value::Null);
        out.push((key, value));
    }
    Ok(out)
}

/// First `n` characters of `s` (char-boundary safe).
fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn field(key: &str, label: &str, widget: Widget, required: bool, template: bool) -> FieldSpec {
    FieldSpec {
        key: key.to_string(),
        label: label.to_string(),
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

#[async_trait]
impl TaskPlugin for HttpPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            type_id: "http.request".to_string(),
            label: "HTTP request".to_string(),
            description: "Send an HTTP request and capture status, headers, and body".to_string(),
            icon: "globe".to_string(),
            color: "#58a6ff".to_string(),
            fields: vec![
                FieldSpec {
                    default: json!("GET"),
                    options: Some(METHODS.iter().map(|m| m.to_string()).collect()),
                    help: "HTTP method".to_string(),
                    ..field("method", "Method", Widget::Select, true, false)
                },
                FieldSpec {
                    help: "Request URL; supports {{ }} references to inputs, outputs, variables, and secrets".to_string(),
                    ..field("url", "URL", Widget::Template, true, true)
                },
                FieldSpec {
                    help: "Request headers; values support {{ }} templates".to_string(),
                    ..field("headers", "Headers", Widget::Keyvalue, false, true)
                },
                FieldSpec {
                    help: "sent as a JSON object; ignored if raw body set".to_string(),
                    ..field("body", "Body params", Widget::Keyvalue, false, true)
                },
                FieldSpec {
                    help: "raw request body — overrides body params".to_string(),
                    ..field("raw_body", "Raw body", Widget::Code, false, true)
                },
                FieldSpec {
                    default: json!("2xx"),
                    help: "comma list: codes or classes, e.g. 2xx,301".to_string(),
                    ..field("success_codes", "Success codes", Widget::Text, false, false)
                },
            ],
        }
    }

    fn validate(&self, config: &Value) -> Vec<String> {
        let Some(obj) = config.as_object() else {
            return vec!["config must be an object".to_string()];
        };
        let mut errs = Vec::new();
        match obj.get("url") {
            Some(Value::String(s)) if !s.trim().is_empty() => {}
            _ => errs.push("url is required".to_string()),
        }
        if let Some(m) = obj.get("method")
            && !m.is_null()
        {
            let ok = m
                .as_str()
                .is_some_and(|s| METHODS.contains(&s.to_ascii_uppercase().as_str()));
            if !ok {
                errs.push("method must be one of GET, POST, PUT, PATCH, DELETE".to_string());
            }
        }
        if let Some(sc) = obj.get("success_codes")
            && !sc.is_null()
        {
            match sc.as_str() {
                Some(s) => {
                    if let Err(e) = parse_success_codes(s) {
                        errs.push(e);
                    }
                }
                None => errs.push("success_codes must be a string".to_string()),
            }
        }
        match kv_pairs("headers", obj.get("headers")) {
            Ok(pairs) => {
                // Check header names eagerly so bad names fail at save time,
                // not at run time. Names containing template expressions
                // can't be checked until render, so they're skipped here.
                for (k, _) in &pairs {
                    if !k.contains("{{") && HeaderName::from_bytes(k.as_bytes()).is_err() {
                        errs.push(format!("headers: \"{k}\" is not a valid header name"));
                    }
                }
            }
            Err(e) => errs.push(e),
        }
        match kv_pairs("body", obj.get("body")) {
            Ok(pairs) => {
                // Surface duplicate keys: execute keeps last-wins behavior,
                // but silently dropping an earlier value is surprising.
                let mut seen = std::collections::HashSet::new();
                let mut dups = std::collections::BTreeSet::new();
                for (k, _) in &pairs {
                    if !seen.insert(k.as_str()) {
                        dups.insert(k.as_str());
                    }
                }
                for k in dups {
                    errs.push(format!("body: duplicate key \"{k}\" (last value wins)"));
                }
            }
            Err(e) => errs.push(e),
        }
        errs
    }

    async fn execute(&self, ctx: &TaskContext, config: Value) -> Result<Value, TaskError> {
        let obj = config
            .as_object()
            .ok_or_else(|| TaskError::fatal("config must be an object"))?;

        let method_str = match obj.get("method") {
            None | Some(Value::Null) => "GET".to_string(),
            Some(v) => v
                .as_str()
                .ok_or_else(|| TaskError::fatal("method must be a string"))?
                .to_ascii_uppercase(),
        };
        if !METHODS.contains(&method_str.as_str()) {
            return Err(TaskError::fatal(format!(
                "method must be one of GET, POST, PUT, PATCH, DELETE (got \"{method_str}\")"
            )));
        }
        let method = reqwest::Method::from_bytes(method_str.as_bytes())
            .expect("method already checked against the allowed list");

        let url = match obj.get("url") {
            Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
            Some(v) if !v.is_null() && !v.is_string() => value_to_string(v),
            _ => return Err(TaskError::fatal("url is required")),
        };

        let success_codes = match obj.get("success_codes") {
            None | Some(Value::Null) => vec![CodeSpec::Class(2)],
            Some(v) => parse_success_codes(&value_to_string(v)).map_err(TaskError::fatal)?,
        };

        let headers = kv_pairs("headers", obj.get("headers")).map_err(TaskError::fatal)?;
        let body_params = kv_pairs("body", obj.get("body")).map_err(TaskError::fatal)?;

        if ctx.cancel.is_cancelled() {
            return Err(TaskError::fatal("canceled"));
        }

        let mut req = self.client.request(method, &url);
        let mut has_content_type = false;
        for (k, v) in &headers {
            if k.eq_ignore_ascii_case("content-type") {
                has_content_type = true;
            }
            let name = HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| TaskError::fatal(format!("invalid header name \"{k}\": {e}")))?;
            let value = HeaderValue::from_str(&value_to_string(v))
                .map_err(|e| TaskError::fatal(format!("invalid value for header \"{k}\": {e}")))?;
            req = req.header(name, value);
        }

        match obj.get("raw_body") {
            Some(Value::String(s)) if !s.is_empty() => {
                if !has_content_type {
                    // Deliberate: only a top-level JSON object/array counts as
                    // JSON here; bare strings/numbers stay text/plain.
                    let ct = match serde_json::from_str::<Value>(s) {
                        Ok(Value::Object(_)) | Ok(Value::Array(_)) => "application/json",
                        _ => "text/plain",
                    };
                    req = req.header(CONTENT_TYPE, ct);
                }
                req = req.body(s.clone());
            }
            Some(v) if !v.is_null() && !v.is_string() => {
                // Templates preserve types: a raw_body that rendered to a
                // JSON value is sent as compact JSON.
                if !has_content_type {
                    req = req.header(CONTENT_TYPE, "application/json");
                }
                req = req.body(v.to_string());
            }
            _ => {
                if !body_params.is_empty() {
                    let map: Map<String, Value> = body_params.into_iter().collect();
                    req = req.json(&Value::Object(map));
                }
            }
        }

        ctx.info(format!("{method_str} {url}"));
        let start = Instant::now();

        let round_trip = async {
            let resp = req.send().await?;
            let status = resp.status();
            let headers = resp.headers().clone();
            let bytes = resp.bytes().await?;
            Ok::<_, reqwest::Error>((status, headers, bytes))
        };
        let (status, resp_headers, bytes) = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return Err(TaskError::fatal("canceled")),
            r = round_trip => r.map_err(|e| {
                if e.is_builder() {
                    TaskError::fatal(error_chain(&e))
                } else {
                    // Connect / DNS / timeout / mid-flight failures are transient.
                    TaskError::retryable(error_chain(&e))
                }
            })?,
        };
        let elapsed_ms = start.elapsed().as_millis();
        let status_code = status.as_u16();

        let mut header_map = Map::new();
        // Multi-value headers are comma-joined; lossy for Set-Cookie (accepted v1).
        for name in resp_headers.keys() {
            let joined = resp_headers
                .get_all(name)
                .iter()
                .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
                .collect::<Vec<_>>()
                .join(", ");
            header_map.insert(name.as_str().to_string(), Value::String(joined));
        }

        if success_codes.iter().any(|s| s.matches(status_code)) {
            ctx.ok(format!("{status} ({elapsed_ms} ms)"));
            let body = serde_json::from_slice::<Value>(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
            Ok(json!({
                "status": status_code,
                "headers": Value::Object(header_map),
                "body": body,
            }))
        } else {
            let retryable = status_code >= 500;
            let line = format!("{status} ({elapsed_ms} ms)");
            if retryable {
                ctx.warn(line);
            } else {
                // Terminal (4xx-class) failure: this attempt is final.
                ctx.err(line);
            }
            let snippet = truncate_chars(&String::from_utf8_lossy(&bytes), 200);
            Err(TaskError {
                message: format!("unexpected status {status_code}: {snippet}"),
                retryable,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;
    use wiremock::matchers::{body_json, body_string, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::plugins::LogLevel;

    fn test_ctx() -> TaskContext {
        TaskContext::new(1, "t1", CancellationToken::new(), Box::new(|_, _| {}))
    }

    type LogLines = Arc<Mutex<Vec<(LogLevel, String)>>>;

    fn logging_ctx() -> (TaskContext, LogLines) {
        let logs: LogLines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&logs);
        let ctx = TaskContext::new(
            1,
            "t1",
            CancellationToken::new(),
            Box::new(move |level, line| sink.lock().unwrap().push((level, line))),
        );
        (ctx, logs)
    }

    #[tokio::test]
    async fn get_json_body_is_parsed_and_logged() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/data"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"a": 1, "b": [true]}))
                    .insert_header("x-thing", "one")
                    .append_header("x-thing", "two"),
            )
            .mount(&server)
            .await;

        let (ctx, logs) = logging_ctx();
        let url = format!("{}/data", server.uri());
        let out = HttpPlugin::new()
            .execute(&ctx, json!({"method": "GET", "url": url}))
            .await
            .unwrap();

        assert_eq!(out["status"], json!(200));
        assert_eq!(out["body"], json!({"a": 1, "b": [true]}));
        assert_eq!(out["headers"]["x-thing"], json!("one, two"));

        let logs = logs.lock().unwrap();
        assert!(
            logs.iter()
                .any(|(l, m)| *l == LogLevel::Info && m.starts_with("GET http"))
        );
        assert!(
            logs.iter()
                .any(|(l, m)| *l == LogLevel::Ok && m.contains("200") && m.contains("ms"))
        );
    }

    #[tokio::test]
    async fn post_body_params_sent_as_json_object() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/submit"))
            .and(header("content-type", "application/json"))
            .and(body_json(json!({"since": "2024-01-01", "n": 5})))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let out = HttpPlugin::new()
            .execute(
                &test_ctx(),
                json!({
                    "method": "POST",
                    "url": format!("{}/submit", server.uri()),
                    "body": [
                        {"key": "since", "value": "2024-01-01"},
                        {"key": "n", "value": 5},
                    ],
                }),
            )
            .await
            .unwrap();
        assert_eq!(out["status"], json!(200));
    }

    #[tokio::test]
    async fn raw_body_json_overrides_body_params() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string(r#"{"raw":true}"#))
            .and(header("content-type", "application/json"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let out = HttpPlugin::new()
            .execute(
                &test_ctx(),
                json!({
                    "method": "POST",
                    "url": server.uri(),
                    "body": [{"key": "ignored", "value": "yes"}],
                    "raw_body": r#"{"raw":true}"#,
                }),
            )
            .await
            .unwrap();
        assert_eq!(out["status"], json!(200));
    }

    #[tokio::test]
    async fn raw_body_plain_text_sent_as_text_plain() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string("hello world"))
            .and(header("content-type", "text/plain"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let out = HttpPlugin::new()
            .execute(
                &test_ctx(),
                json!({
                    "method": "POST",
                    "url": server.uri(),
                    "raw_body": "hello world",
                }),
            )
            .await
            .unwrap();
        assert_eq!(out["status"], json!(200));
    }

    #[tokio::test]
    async fn headers_are_sent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let out = HttpPlugin::new()
            .execute(
                &test_ctx(),
                json!({
                    "url": server.uri(),
                    "headers": [{"key": "Authorization", "value": "Bearer tok"}],
                }),
            )
            .await
            .unwrap();
        assert_eq!(out["status"], json!(200));
    }

    #[tokio::test]
    async fn non_string_header_value_is_stringified() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("x-num", "42"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let out = HttpPlugin::new()
            .execute(
                &test_ctx(),
                json!({
                    "url": server.uri(),
                    "headers": [{"key": "x-num", "value": 42}],
                }),
            )
            .await
            .unwrap();
        assert_eq!(out["status"], json!(200));
    }

    #[tokio::test]
    async fn status_500_is_retryable_with_body_snippet() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let (ctx, logs) = logging_ctx();
        let err = HttpPlugin::new()
            .execute(&ctx, json!({"url": server.uri()}))
            .await
            .unwrap_err();
        assert!(err.retryable);
        assert!(err.message.contains("500"));
        assert!(err.message.contains("boom"));
        // Retryable (5xx) non-success logs at Warn.
        assert!(
            logs.lock()
                .unwrap()
                .iter()
                .any(|(l, m)| *l == LogLevel::Warn && m.contains("500"))
        );
    }

    #[tokio::test]
    async fn status_404_with_default_codes_is_not_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
            .mount(&server)
            .await;

        let (ctx, logs) = logging_ctx();
        let err = HttpPlugin::new()
            .execute(&ctx, json!({"url": server.uri()}))
            .await
            .unwrap_err();
        assert!(!err.retryable);
        assert!(err.message.contains("404"));
        // Terminal (4xx) non-success logs at Err.
        assert!(
            logs.lock()
                .unwrap()
                .iter()
                .any(|(l, m)| *l == LogLevel::Err && m.contains("404"))
        );
    }

    #[tokio::test]
    async fn success_codes_404_accepts_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404).set_body_string("missing but fine"))
            .mount(&server)
            .await;

        let out = HttpPlugin::new()
            .execute(
                &test_ctx(),
                json!({"url": server.uri(), "success_codes": "404"}),
            )
            .await
            .unwrap();
        assert_eq!(out["status"], json!(404));
        assert_eq!(out["body"], json!("missing but fine"));
    }

    #[tokio::test]
    async fn success_codes_class_list_accepts_301() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(301))
            .mount(&server)
            .await;

        let out = HttpPlugin::new()
            .execute(
                &test_ctx(),
                json!({"url": server.uri(), "success_codes": "2xx,301"}),
            )
            .await
            .unwrap();
        assert_eq!(out["status"], json!(301));
    }

    #[tokio::test]
    async fn invalid_success_codes_is_non_retryable_error() {
        let err = HttpPlugin::new()
            .execute(
                &test_ctx(),
                json!({"url": "http://localhost", "success_codes": "banana"}),
            )
            .await
            .unwrap_err();
        assert!(!err.retryable);
        assert!(err.message.contains("success_codes"));
    }

    #[tokio::test]
    async fn non_json_response_body_is_a_string() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("plain old text"))
            .mount(&server)
            .await;

        let out = HttpPlugin::new()
            .execute(&test_ctx(), json!({"url": server.uri()}))
            .await
            .unwrap();
        assert_eq!(out["body"], json!("plain old text"));
    }

    #[tokio::test]
    async fn connect_error_is_retryable() {
        // Port 1 is virtually never listening.
        let err = HttpPlugin::new()
            .execute(&test_ctx(), json!({"url": "http://127.0.0.1:1/"}))
            .await
            .unwrap_err();
        assert!(err.retryable);
    }

    #[tokio::test]
    async fn cancelled_token_short_circuits_before_any_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let token = CancellationToken::new();
        token.cancel();
        let ctx = TaskContext::new(1, "t1", token, Box::new(|_, _| {}));
        let err = HttpPlugin::new()
            .execute(&ctx, json!({"url": server.uri()}))
            .await
            .unwrap_err();
        assert_eq!(err.message, "canceled");
        assert!(!err.retryable);
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn mid_flight_cancellation_aborts_promptly() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;

        let ctx = test_ctx();
        let token = ctx.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            token.cancel();
        });

        let start = Instant::now();
        let err = HttpPlugin::new()
            .execute(&ctx, json!({"url": server.uri()}))
            .await
            .unwrap_err();
        assert_eq!(err.message, "canceled");
        assert!(!err.retryable);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "execute did not return promptly after cancellation: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn validate_catches_bad_configs() {
        let p = HttpPlugin::new();

        let errs = p.validate(&json!({}));
        assert!(errs.iter().any(|e| e == "url is required"));

        let errs = p.validate(&json!({"url": ""}));
        assert!(errs.iter().any(|e| e == "url is required"));

        let errs = p.validate(&json!({"url": "http://x", "method": "BREW"}));
        assert!(errs.iter().any(|e| e.contains("method")));

        let errs = p.validate(&json!({"url": "http://x", "success_codes": "2xx,nope"}));
        assert!(errs.iter().any(|e| e.contains("success_codes")));

        let errs = p.validate(&json!({"url": "http://x", "headers": [{"key": "", "value": "v"}]}));
        assert!(errs.iter().any(|e| e.contains("headers")));

        let errs = p.validate(&json!({"url": "http://x", "body": "not-an-array"}));
        assert!(errs.iter().any(|e| e.contains("body")));

        let errs = p.validate(&json!("not an object"));
        assert_eq!(errs, vec!["config must be an object".to_string()]);
    }

    #[test]
    fn validate_checks_header_names_eagerly() {
        let p = HttpPlugin::new();

        // A space makes an invalid header name; caught at save time.
        let errs =
            p.validate(&json!({"url": "http://x", "headers": [{"key": "bad name", "value": "v"}]}));
        assert!(
            errs.iter()
                .any(|e| e.contains("bad name") && e.contains("header name")),
            "unexpected errors: {errs:?}"
        );

        // Template-bearing names can't be checked until render: skipped.
        let errs = p.validate(
            &json!({"url": "http://x", "headers": [{"key": "{{ inputs.h }}", "value": "v"}]}),
        );
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn validate_reports_duplicate_body_keys() {
        let p = HttpPlugin::new();
        let errs = p.validate(&json!({
            "url": "http://x",
            "body": [
                {"key": "since", "value": "a"},
                {"key": "since", "value": "b"},
                {"key": "other", "value": "c"},
            ],
        }));
        assert_eq!(
            errs,
            vec!["body: duplicate key \"since\" (last value wins)".to_string()]
        );
    }

    #[test]
    fn validate_accepts_good_config() {
        let p = HttpPlugin::new();
        let errs = p.validate(&json!({
            "method": "POST",
            "url": "{{ vars.server }}/api",
            "headers": [{"key": "Authorization", "value": "Bearer {{ secrets.T }}"}],
            "body": [{"key": "since", "value": "{{ inputs.since }}"}],
            "raw_body": null,
            "success_codes": "2xx,301",
        }));
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn success_codes_grammar() {
        assert_eq!(
            parse_success_codes("2xx").unwrap(),
            vec![CodeSpec::Class(2)]
        );
        assert_eq!(
            parse_success_codes(" 2xx , 301 ").unwrap(),
            vec![CodeSpec::Class(2), CodeSpec::Exact(301)]
        );
        assert_eq!(parse_success_codes("").unwrap(), vec![CodeSpec::Class(2)]);
        assert!(parse_success_codes("6xx").is_err());
        assert!(parse_success_codes("42").is_err());
        assert!(parse_success_codes("2xx,,301").is_err());
        assert!(parse_success_codes("banana").is_err());
    }

    #[test]
    fn manifest_shape() {
        let m = HttpPlugin::new().manifest();
        assert_eq!(m.type_id, "http.request");
        assert_eq!(m.label, "HTTP request");
        assert_eq!(m.icon, "globe");
        assert_eq!(m.color, "#58a6ff");
        let keys: Vec<&str> = m.fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "method",
                "url",
                "headers",
                "body",
                "raw_body",
                "success_codes"
            ]
        );
        let method_field = &m.fields[0];
        assert_eq!(method_field.default, json!("GET"));
        assert_eq!(
            method_field.options.as_deref().unwrap(),
            ["GET", "POST", "PUT", "PATCH", "DELETE"]
        );
        let url_field = &m.fields[1];
        assert!(url_field.required);
        assert!(url_field.template);
        assert!(url_field.help.contains("{{ }}"));
    }
}
