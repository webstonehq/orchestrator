//! Template rendering: reference resolution against a JSON context, the
//! `now()` function, `dateAdd` filters, and secret redaction.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::Value;

use super::{DateUnit, ExprError, Filter, RefExpr, Segment};

pub(crate) fn render(template: &str, ctx: &Value) -> Result<Value, ExprError> {
    let segments = super::parse(template)?;

    // Exactly one Ref segment: preserve the referenced value's JSON type.
    if let [Segment::Ref(re)] = segments.as_slice() {
        return eval_ref(re, ctx);
    }

    let mut out = String::new();
    for segment in &segments {
        match segment {
            Segment::Text(text) => out.push_str(text),
            Segment::Ref(re) => match eval_ref(re, ctx)? {
                Value::String(s) => out.push_str(&s),
                // `Value` displays as compact JSON (serde_json::to_string).
                other => out.push_str(&other.to_string()),
            },
        }
    }
    Ok(Value::String(out))
}

pub(crate) fn render_config(config: &Value, ctx: &Value) -> Result<Value, ExprError> {
    match config {
        Value::String(template) => render(template, ctx),
        Value::Array(items) => items
            .iter()
            .map(|item| render_config(item, ctx))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(map) => map
            .iter()
            .map(|(key, item)| Ok((key.clone(), render_config(item, ctx)?)))
            .collect::<Result<serde_json::Map<_, _>, ExprError>>()
            .map(Value::Object),
        other => Ok(other.clone()),
    }
}

pub(crate) fn redact(value: &mut Value, secret_values: &[String]) {
    // Replace longest secrets first: if one secret is a substring of another
    // (or contained in it), masking the shorter one first would split the
    // longer secret into fragments that partially leak. Callers pass values
    // in nondeterministic (HashMap) order, so we must not depend on it.
    let mut secrets: Vec<&str> = secret_values
        .iter()
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .collect();
    secrets.sort_unstable_by_key(|s| std::cmp::Reverse(s.len()));
    redact_inner(value, &secrets);
}

fn redact_inner(value: &mut Value, secrets: &[&str]) {
    match value {
        Value::String(s) => {
            for secret in secrets {
                if s.contains(secret) {
                    *s = s.replace(secret, "***");
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_inner(item, secrets);
            }
        }
        Value::Object(map) => {
            for (_key, item) in map.iter_mut() {
                redact_inner(item, secrets);
            }
        }
        _ => {}
    }
}

fn eval_ref(re: &RefExpr, ctx: &Value) -> Result<Value, ExprError> {
    let mut value = resolve_path(&re.path, ctx)?;
    for filter in &re.filters {
        value = apply_filter(filter, value)?;
    }
    Ok(value)
}

fn resolve_path(path: &str, ctx: &Value) -> Result<Value, ExprError> {
    if path == "now()" {
        return Ok(Value::String(now_rfc3339()));
    }

    let unknown = || ExprError::render(format!("unknown reference: {path}"));
    let mut current = ctx;
    for part in PathParts::new(path) {
        current = match part {
            PathPart::Key(key) => current.get(key).ok_or_else(unknown)?,
            PathPart::Index(index) => current.get(index).ok_or_else(unknown)?,
        };
    }
    Ok(current.clone())
}

enum PathPart<'a> {
    Key(&'a str),
    Index(usize),
}

/// Iterator over the parts of a canonical path string (as produced by the
/// parser), e.g. `outputs.discover.ids[0].name`.
///
/// This intentionally re-lexes the canonical string emitted by `parse.rs`
/// (documented duplication): `RefExpr` exposes only the canonical `path`
/// string in the public API, so both consumers (Rust here, TypeScript in the
/// UI) re-split it the same trivial way.
struct PathParts<'a> {
    rest: &'a str,
}

impl<'a> PathParts<'a> {
    fn new(path: &'a str) -> Self {
        PathParts { rest: path }
    }
}

impl<'a> Iterator for PathParts<'a> {
    type Item = PathPart<'a>;

    fn next(&mut self) -> Option<PathPart<'a>> {
        if self.rest.is_empty() {
            return None;
        }
        if let Some(after) = self.rest.strip_prefix('[') {
            let end = after.find(']').expect("canonical path has closing ']'");
            let index = after[..end].parse::<usize>().expect("canonical index");
            self.rest = &after[end + 1..];
            return Some(PathPart::Index(index));
        }
        let rest = self.rest.strip_prefix('.').unwrap_or(self.rest);
        let end = rest.find(['.', '[']).unwrap_or(rest.len());
        self.rest = &rest[end..];
        Some(PathPart::Key(&rest[..end]))
    }
}

fn apply_filter(filter: &Filter, value: Value) -> Result<Value, ExprError> {
    match filter {
        Filter::DateAdd { n, unit } => date_add(value, *n, *unit),
    }
}

fn date_add(value: Value, n: i64, unit: DateUnit) -> Result<Value, ExprError> {
    let Value::String(input) = &value else {
        return Err(ExprError::render(format!(
            "dateAdd: input must be an RFC3339 datetime string, got {value}"
        )));
    };
    let parsed = DateTime::parse_from_rfc3339(input).map_err(|e| {
        ExprError::render(format!(
            "dateAdd: '{input}' is not an RFC3339 datetime: {e}"
        ))
    })?;

    let duration = match unit {
        DateUnit::Days => Duration::try_days(n),
        DateUnit::Hours => Duration::try_hours(n),
        DateUnit::Minutes => Duration::try_minutes(n),
    }
    .ok_or_else(|| ExprError::render(format!("dateAdd: offset out of range: {n}")))?;

    let result = parsed
        .with_timezone(&Utc)
        .checked_add_signed(duration)
        .ok_or_else(|| {
            ExprError::render(format!("dateAdd: resulting datetime out of range: {input}"))
        })?;
    Ok(Value::String(
        result.to_rfc3339_opts(SecondsFormat::Secs, true),
    ))
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}
