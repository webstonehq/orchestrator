//! Expression engine: template parsing, rendering, and secret redaction.
//!
//! # Grammar (v1)
//!
//! A template is a sequence of literal text and `{{ <expr> }}` segments.
//!
//! ```text
//! expr    = path { "|" filter }
//! path    = "now()" | ident { "." ident | "[" uint "]" }
//! ident   = (ALPHA | "_") { ALPHA | DIGIT | "_" }
//! filter  = "dateAdd" "(" int "," "'" unit "'" ")"
//! unit    = "DAYS" | "HOURS" | "MINUTES"
//! ```
//!
//! Whitespace inside `{{ }}` (around the path, `|`, and filter arguments) is
//! insignificant; [`serialize`] canonicalizes to the single-space form
//! `{{ path | filter(args) }}`. The grammar is intentionally regular and
//! hand-rolled so the UI can reimplement it verbatim in TypeScript.
mod parse;
mod render;

use std::fmt;

/// One piece of a parsed template: literal text or a `{{ ... }}` expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    Ref(RefExpr),
}

/// A `{{ path | filters... }}` expression. `path` is stored in canonical
/// serialized form (e.g. `outputs.discover.ids[0].name` or `now()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefExpr {
    pub path: String,
    pub filters: Vec<Filter>,
}

/// A filter applied to a reference value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    DateAdd { n: i64, unit: DateUnit },
}

/// Unit argument for `dateAdd`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateUnit {
    Days,
    Hours,
    Minutes,
}

impl DateUnit {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            DateUnit::Days => "DAYS",
            DateUnit::Hours => "HOURS",
            DateUnit::Minutes => "MINUTES",
        }
    }
}

/// Error from parsing or rendering a template. `offset` is the byte offset
/// into the template for parse errors; `None` for render-time errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprError {
    pub message: String,
    pub offset: Option<usize>,
}

impl ExprError {
    pub(crate) fn at(message: impl Into<String>, offset: usize) -> Self {
        ExprError {
            message: message.into(),
            offset: Some(offset),
        }
    }

    pub(crate) fn render(message: impl Into<String>) -> Self {
        ExprError {
            message: message.into(),
            offset: None,
        }
    }
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.offset {
            Some(offset) => write!(f, "{} (at byte offset {offset})", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for ExprError {}

/// Parse a template into segments. Fails with a byte offset on malformed
/// `{{ }}` expressions.
pub fn parse(template: &str) -> Result<Vec<Segment>, ExprError> {
    parse::parse_template(template)
}

/// Serialize segments back to a template string in canonical form.
/// `serialize(&parse(t)?)` yields the canonical spelling of `t`.
pub fn serialize(segments: &[Segment]) -> String {
    let mut out = String::new();
    for segment in segments {
        match segment {
            Segment::Text(text) => out.push_str(text),
            Segment::Ref(re) => {
                out.push_str("{{ ");
                out.push_str(&re.path);
                for filter in &re.filters {
                    match filter {
                        Filter::DateAdd { n, unit } => {
                            out.push_str(&format!(" | dateAdd({n}, '{}')", unit.as_str()));
                        }
                    }
                }
                out.push_str(" }}");
            }
        }
    }
    out
}

/// Render a template against a context object.
///
/// A template that is exactly one `{{ ref }}` segment resolves to the
/// referenced JSON value with its type preserved; any mix of text and
/// references produces a JSON string (non-string values serialized as
/// compact JSON).
pub fn render(template: &str, ctx: &serde_json::Value) -> Result<serde_json::Value, ExprError> {
    render::render(template, ctx)
}

/// Deep-walk a JSON config value, rendering every string as a template.
pub fn render_config(
    config: &serde_json::Value,
    ctx: &serde_json::Value,
) -> Result<serde_json::Value, ExprError> {
    render::render_config(config, ctx)
}

/// Reference paths (without filters) in order of appearance, duplicates
/// included; `now()` is a function, not a context reference, and is omitted.
pub fn referenced_paths(template: &str) -> Result<Vec<String>, ExprError> {
    Ok(parse(template)?
        .into_iter()
        .filter_map(|segment| match segment {
            Segment::Ref(re) if re.path != "now()" => Some(re.path),
            _ => None,
        })
        .collect())
}

/// Replace every occurrence of each secret string inside any string value
/// (recursively) with `***`.
pub fn redact(value: &mut serde_json::Value, secret_values: &[String]) {
    render::redact(value, secret_values);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn ctx() -> Value {
        json!({
            "inputs": { "since": "2026-07-01T00:00:00Z", "count": 3 },
            "vars": { "server": "https://api.example.com" },
            "secrets": { "API_TOKEN": "tok-123" },
            "outputs": {
                "discover": {
                    "ids": [101, 102, 103],
                    "items": [ { "name": "first" }, { "name": "second" } ]
                }
            },
            "taskrun": { "value": { "id": 42 } },
            "grid": [[10, 20], [30, 40]]
        })
    }

    // ---- parse / serialize round-trips ----

    #[test]
    fn round_trip_canonicalizes_whitespace() {
        let segments = parse("{{inputs.x}}").unwrap();
        assert_eq!(serialize(&segments), "{{ inputs.x }}");

        let segments = parse("{{   inputs.x   }}").unwrap();
        assert_eq!(serialize(&segments), "{{ inputs.x }}");
    }

    #[test]
    fn round_trip_filter_canonicalization() {
        let segments = parse("{{now()|dateAdd(-7,'DAYS')}}").unwrap();
        assert_eq!(serialize(&segments), "{{ now() | dateAdd(-7, 'DAYS') }}");
        // Canonical form is a fixed point.
        let again = parse(&serialize(&segments)).unwrap();
        assert_eq!(again, segments);
    }

    #[test]
    fn round_trip_mixed_text_and_refs() {
        let template = "{{ vars.server }}/api/x?since={{ inputs.since }}";
        let segments = parse(template).unwrap();
        assert_eq!(serialize(&segments), template);
        assert_eq!(segments.len(), 3);
    }

    #[test]
    fn parse_text_only() {
        let segments = parse("no templates here { } }} still text").unwrap();
        assert_eq!(
            segments,
            vec![Segment::Text("no templates here { } }} still text".into())]
        );
    }

    #[test]
    fn parse_path_with_index_canonical() {
        let segments = parse("{{ outputs.discover.items[0].name }}").unwrap();
        match &segments[0] {
            Segment::Ref(re) => assert_eq!(re.path, "outputs.discover.items[0].name"),
            other => panic!("expected ref, got {other:?}"),
        }
    }

    #[test]
    fn parse_structure_exposes_filters() {
        let segments = parse("{{ inputs.since | dateAdd(2, 'HOURS') }}").unwrap();
        assert_eq!(
            segments,
            vec![Segment::Ref(RefExpr {
                path: "inputs.since".into(),
                filters: vec![Filter::DateAdd {
                    n: 2,
                    unit: DateUnit::Hours
                }],
            })]
        );
    }

    // ---- parse errors ----

    #[test]
    fn error_unclosed_brace_has_offset() {
        let err = parse("abc {{ inputs.x").unwrap_err();
        assert!(err.offset.is_some(), "expected offset, got {err:?}");
        assert!(err.message.contains("unclosed"), "message: {}", err.message);

        let err = parse("abc {{").unwrap_err();
        assert_eq!(err.offset, Some(4));
    }

    #[test]
    fn error_empty_expression() {
        let err = parse("{{ }}").unwrap_err();
        assert!(err.message.contains("empty"), "message: {}", err.message);
        assert_eq!(err.offset, Some(0));

        let err = parse("{{}}").unwrap_err();
        assert!(err.message.contains("empty"), "message: {}", err.message);
    }

    #[test]
    fn error_unknown_filter() {
        let err = parse("{{ inputs.x | upper }}").unwrap_err();
        assert!(
            err.message.contains("unknown filter") && err.message.contains("upper"),
            "message: {}",
            err.message
        );
        assert!(err.offset.is_some());
    }

    #[test]
    fn error_path_trailing_dot() {
        let err = parse("{{ inputs. }}").unwrap_err();
        assert!(
            err.message.contains("expected identifier"),
            "message: {}",
            err.message
        );
        assert_eq!(err.offset, Some(10), "offset of char after the dot");
    }

    #[test]
    fn error_path_leading_dot() {
        let err = parse("{{ .inputs }}").unwrap_err();
        assert!(
            err.message.contains("expected identifier"),
            "message: {}",
            err.message
        );
        assert_eq!(err.offset, Some(3), "offset of the leading dot");
    }

    #[test]
    fn error_index_not_numeric() {
        let err = parse("{{ inputs.x[abc] }}").unwrap_err();
        assert!(
            err.message.contains("expected array index"),
            "message: {}",
            err.message
        );
        assert_eq!(err.offset, Some(12), "offset of 'abc'");
    }

    #[test]
    fn error_index_unterminated() {
        let err = parse("{{ inputs.x[1 }}").unwrap_err();
        assert!(
            err.message.contains("']' after array index"),
            "message: {}",
            err.message
        );
        assert_eq!(err.offset, Some(13), "offset where ']' was expected");
    }

    #[test]
    fn error_ident_starting_with_digit() {
        let err = parse("{{ 9lives }}").unwrap_err();
        assert!(
            err.message.contains("expected identifier"),
            "message: {}",
            err.message
        );
        assert_eq!(err.offset, Some(3), "offset of the digit");
    }

    #[test]
    fn error_unknown_date_unit() {
        let err = parse("{{ inputs.x | dateAdd(1, 'WEEKS') }}").unwrap_err();
        assert!(
            err.message.contains("unknown date unit") && err.message.contains("WEEKS"),
            "message: {}",
            err.message
        );
        assert!(err.offset.is_some());
    }

    #[test]
    fn error_filter_arg_not_integer() {
        let err = parse("{{ inputs.x | dateAdd(x, 'DAYS') }}").unwrap_err();
        assert!(
            err.message.contains("expected integer"),
            "message: {}",
            err.message
        );
        assert!(err.offset.is_some());
    }

    #[test]
    fn error_unknown_function() {
        let err = parse("{{ upper() }}").unwrap_err();
        assert!(
            err.message.contains("unknown function") && err.message.contains("upper"),
            "message: {}",
            err.message
        );
        assert_eq!(err.offset, Some(3), "offset of the function name");
    }

    // ---- render ----

    #[test]
    fn render_text_only_passthrough() {
        let v = render("hello world", &ctx()).unwrap();
        assert_eq!(v, json!("hello world"));
    }

    #[test]
    fn render_single_ref_preserves_type() {
        assert_eq!(
            render("{{ outputs.discover.ids }}", &ctx()).unwrap(),
            json!([101, 102, 103])
        );
        assert_eq!(render("{{ inputs.count }}", &ctx()).unwrap(), json!(3));
        assert_eq!(render("{{ taskrun.value.id }}", &ctx()).unwrap(), json!(42));
    }

    #[test]
    fn render_mixed_stringifies_compactly() {
        assert_eq!(
            render("ids={{ outputs.discover.ids }}!", &ctx()).unwrap(),
            json!("ids=[101,102,103]!")
        );
        assert_eq!(
            render("{{ vars.server }}/api/x", &ctx()).unwrap(),
            json!("https://api.example.com/api/x")
        );
        assert_eq!(
            render("n={{ inputs.count }}", &ctx()).unwrap(),
            json!("n=3")
        );
    }

    #[test]
    fn render_two_refs_stringifies() {
        assert_eq!(
            render("{{ inputs.count }}{{ inputs.count }}", &ctx()).unwrap(),
            json!("33")
        );
    }

    #[test]
    fn render_filter_inside_mixed_template() {
        assert_eq!(
            render(
                "since={{ inputs.since | dateAdd(-7, 'DAYS') }}&limit=5",
                &ctx()
            )
            .unwrap(),
            json!("since=2026-06-24T00:00:00Z&limit=5")
        );
    }

    #[test]
    fn render_chained_indexing() {
        assert_eq!(render("{{ grid[0][1] }}", &ctx()).unwrap(), json!(20));
        assert_eq!(render("{{ grid[1][0] }}", &ctx()).unwrap(), json!(30));
        // Round-trips canonically too.
        assert_eq!(
            serialize(&parse("{{grid[0][1]}}").unwrap()),
            "{{ grid[0][1] }}"
        );
    }

    #[test]
    fn render_nested_path_and_index() {
        assert_eq!(
            render("{{ outputs.discover.items[0].name }}", &ctx()).unwrap(),
            json!("first")
        );
        assert_eq!(
            render("{{ outputs.discover.ids[2] }}", &ctx()).unwrap(),
            json!(103)
        );
    }

    #[test]
    fn render_now_shape() {
        let v = render("{{ now() }}", &ctx()).unwrap();
        let s = v.as_str().expect("now() must render a string");
        assert!(s.starts_with("20"), "got {s}");
        assert!(
            chrono::DateTime::parse_from_rfc3339(s).is_ok(),
            "not RFC3339: {s}"
        );
        assert!(s.ends_with('Z'), "not UTC Z form: {s}");
    }

    #[test]
    fn render_date_add_all_units_and_negative() {
        let c = ctx();
        assert_eq!(
            render("{{ inputs.since | dateAdd(-7, 'DAYS') }}", &c).unwrap(),
            json!("2026-06-24T00:00:00Z")
        );
        assert_eq!(
            render("{{ inputs.since | dateAdd(5, 'HOURS') }}", &c).unwrap(),
            json!("2026-07-01T05:00:00Z")
        );
        assert_eq!(
            render("{{ inputs.since | dateAdd(90, 'MINUTES') }}", &c).unwrap(),
            json!("2026-07-01T01:30:00Z")
        );
        // Chained filters.
        assert_eq!(
            render(
                "{{ inputs.since | dateAdd(1, 'DAYS') | dateAdd(-30, 'MINUTES') }}",
                &c
            )
            .unwrap(),
            json!("2026-07-01T23:30:00Z")
        );
    }

    #[test]
    fn render_date_add_normalizes_to_utc() {
        let c = json!({ "inputs": { "t": "2026-07-01T02:00:00+02:00" } });
        assert_eq!(
            render("{{ inputs.t | dateAdd(0, 'HOURS') }}", &c).unwrap(),
            json!("2026-07-01T00:00:00Z")
        );
    }

    #[test]
    fn render_now_with_date_add() {
        let v = render("{{ now() | dateAdd(-1, 'DAYS') }}", &ctx()).unwrap();
        let s = v.as_str().unwrap();
        let parsed = chrono::DateTime::parse_from_rfc3339(s).unwrap();
        let delta = chrono::Utc::now() - parsed.with_timezone(&chrono::Utc);
        // Roughly one day in the past.
        assert!(delta > chrono::Duration::hours(23) && delta < chrono::Duration::hours(25));
    }

    #[test]
    fn render_unknown_ref_error_names_full_path() {
        let err = render("{{ outputs.missing.ids }}", &ctx()).unwrap_err();
        assert!(
            err.message
                .contains("unknown reference: outputs.missing.ids"),
            "message: {}",
            err.message
        );

        let err = render("{{ outputs.discover.ids[9] }}", &ctx()).unwrap_err();
        assert!(
            err.message
                .contains("unknown reference: outputs.discover.ids[9]"),
            "message: {}",
            err.message
        );
    }

    #[test]
    fn render_date_add_on_non_datetime_errors() {
        assert!(render("{{ inputs.count | dateAdd(1, 'DAYS') }}", &ctx()).is_err());
        let c = json!({ "inputs": { "s": "not a date" } });
        assert!(render("{{ inputs.s | dateAdd(1, 'DAYS') }}", &c).is_err());
    }

    #[test]
    fn render_date_add_huge_offset_errors_not_panics() {
        let err = render(
            "{{ inputs.since | dateAdd(9223372036854775807, 'DAYS') }}",
            &ctx(),
        )
        .unwrap_err();
        assert!(
            err.message.contains("out of range"),
            "message: {}",
            err.message
        );
    }

    // ---- render_config ----

    #[test]
    fn render_config_deep_walk() {
        let config = json!({
            "url": "{{ vars.server }}/api/x",
            "ids": "{{ outputs.discover.ids }}",
            "nested": {
                "list": ["{{ inputs.count }}", "plain", { "k": "{{ taskrun.value.id }}" }]
            },
            "number": 7,
            "flag": true
        });
        let rendered = render_config(&config, &ctx()).unwrap();
        assert_eq!(
            rendered,
            json!({
                "url": "https://api.example.com/api/x",
                "ids": [101, 102, 103],
                "nested": { "list": [3, "plain", { "k": 42 }] },
                "number": 7,
                "flag": true
            })
        );
    }

    #[test]
    fn render_config_preserves_null_leaf() {
        let config = json!({ "a": null, "b": "{{ inputs.count }}", "c": [null] });
        assert_eq!(
            render_config(&config, &ctx()).unwrap(),
            json!({ "a": null, "b": 3, "c": [null] })
        );
    }

    // ---- referenced_paths ----

    #[test]
    fn referenced_paths_in_order_with_duplicates() {
        let paths = referenced_paths(
            "{{ inputs.a }} {{ outputs.discover.ids }} {{ inputs.a }} {{ now() }}",
        )
        .unwrap();
        assert_eq!(paths, vec!["inputs.a", "outputs.discover.ids", "inputs.a"]);
    }

    #[test]
    fn referenced_paths_strips_filters() {
        let paths = referenced_paths("{{ outputs.discover.ids[0] | dateAdd(1, 'DAYS') }}").unwrap();
        assert_eq!(paths, vec!["outputs.discover.ids[0]"]);
    }

    // ---- redact ----

    #[test]
    fn redact_multi_occurrence_and_substring() {
        let mut v = json!({
            "log": "token tok-123 used; again tok-123",
            "url": "https://x?auth=tok-123&other=1",
            "list": ["tok-123", { "deep": "prefix tok-123 suffix" }],
            "n": 5
        });
        redact(&mut v, &["tok-123".to_string()]);
        assert_eq!(
            v,
            json!({
                "log": "token *** used; again ***",
                "url": "https://x?auth=***&other=1",
                "list": ["***", { "deep": "prefix *** suffix" }],
                "n": 5
            })
        );
    }

    #[test]
    fn redact_multiple_secrets_and_empty_ignored() {
        let mut v = json!("a=alpha b=beta");
        redact(
            &mut v,
            &["alpha".to_string(), "beta".to_string(), String::new()],
        );
        assert_eq!(v, json!("a=*** b=***"));
    }

    #[test]
    fn redact_overlapping_secrets_order_independent() {
        // "b" is a substring of "ab": processing "b" first must not leave
        // the "a" of "ab" exposed. Both input orders must fully mask.
        let mut v = json!("xaby");
        redact(&mut v, &["b".to_string(), "ab".to_string()]);
        assert_eq!(v, json!("x***y"), "shorter secret listed first");

        let mut v = json!("xaby");
        redact(&mut v, &["ab".to_string(), "b".to_string()]);
        assert_eq!(v, json!("x***y"), "longer secret listed first");
    }

    #[test]
    fn redact_secret_containing_another_secret() {
        // One secret's value fully contains another secret.
        let mut v = json!("auth=tok-abc-999; short=abc");
        redact(&mut v, &["abc".to_string(), "tok-abc-999".to_string()]);
        assert_eq!(v, json!("auth=***; short=***"));

        let mut v = json!("auth=tok-abc-999; short=abc");
        redact(&mut v, &["tok-abc-999".to_string(), "abc".to_string()]);
        assert_eq!(v, json!("auth=***; short=***"));
    }
}
