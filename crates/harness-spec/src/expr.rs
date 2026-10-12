// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Expressions and templates.
//!
//! A harness addresses run data through a small, explicitly bounded grammar:
//! dotted paths rooted at a fixed namespace (`inputs`, `facts`, `steps`,
//! `env`), string interpolation with `{{ path }}`, and — for `branch`
//! conditions — comparisons and boolean combinators. There are no
//! user-defined functions here; anything richer belongs to the sandboxed
//! `transform` primitive (ADR 0004).
//!
//! The grammar is intentionally narrower than the deleted graph DSL's: a
//! path is validated at compile time against the harness's declared
//! namespaces, and interpolation is Unicode-correct by construction (the
//! old `bytes[i] as char` failure mode is structurally impossible — paths
//! are parsed, never byte-cast).

use serde::{Deserialize, Serialize};
use std::fmt;

/// A dotted path into run data, e.g. `inputs.topic` or `steps.pick.value.ok`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Path {
    segments: Vec<String>,
}

impl Path {
    /// Parses a dotted path. Empty segments and whitespace-only segments are
    /// rejected.
    ///
    /// # Errors
    ///
    /// [`crate::HarnessError::Invalid`] when the path is empty or has an empty
    /// segment.
    pub fn parse(text: &str) -> Result<Self, crate::HarnessError> {
        let text = text.trim();
        let segments: Vec<String> = text.split('.').map(str::to_string).collect();
        if text.is_empty() || segments.iter().any(String::is_empty) {
            return Err(crate::HarnessError::Invalid(format!(
                "invalid path `{text}`: paths are non-empty dotted segments"
            )));
        }
        Ok(Self { segments })
    }

    /// The path's segments, root first.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// The path's root namespace (`inputs`, `facts`, `steps`, `env`).
    #[must_use]
    pub fn root(&self) -> &str {
        &self.segments[0]
    }

    /// Renders the path back to its dotted form.
    #[must_use]
    pub fn as_text(&self) -> String {
        self.segments.join(".")
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_text())
    }
}

/// A string with `{{ path }}` interpolations.
///
/// Parsing splits the literal once, at compile time; rendering joins literal
/// text with resolved values and never re-parses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct Template {
    parts: Vec<TemplatePart>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TemplatePart {
    Text(String),
    Hole(Path),
}

impl Template {
    /// Parses `text` into literal parts and holes.
    ///
    /// # Errors
    ///
    /// [`crate::HarnessError::Invalid`] when a `{{ ... }}` hole is unclosed or its
    /// path does not parse.
    pub fn parse(text: &str) -> Result<Self, crate::HarnessError> {
        let mut parts = Vec::new();
        let mut rest = text;
        loop {
            match rest.find("{{") {
                None => {
                    if !rest.is_empty() {
                        parts.push(TemplatePart::Text(rest.to_string()));
                    }
                    break;
                }
                Some(start) => {
                    if start > 0 {
                        parts.push(TemplatePart::Text(rest[..start].to_string()));
                    }
                    let after = &rest[start + 2..];
                    let end = after.find("}}").ok_or_else(|| {
                        crate::HarnessError::Invalid(format!(
                            "unclosed `{{{{` in template `{text}`"
                        ))
                    })?;
                    let hole = Path::parse(after[..end].trim())?;
                    parts.push(TemplatePart::Hole(hole));
                    rest = &after[end + 2..];
                }
            }
        }
        Ok(Self { parts })
    }

    /// The paths this template reads, in order of first appearance.
    #[must_use]
    pub fn paths(&self) -> Vec<&Path> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                TemplatePart::Text(_) => None,
                TemplatePart::Hole(path) => Some(path),
            })
            .collect()
    }

    /// `true` when the template is a single hole that covers the whole
    /// string — such a template carries the referenced value's type through,
    /// not its string form.
    #[must_use]
    pub fn is_single_hole(&self) -> bool {
        matches!(self.parts.as_slice(), [TemplatePart::Hole(_)])
    }

    /// The single hole's path when [`Template::is_single_hole`].
    #[must_use]
    pub fn single_hole(&self) -> Option<&Path> {
        match self.parts.as_slice() {
            [TemplatePart::Hole(path)] => Some(path),
            _ => None,
        }
    }

    /// The empty template: renders to nothing. A compile-time fallback for
    /// a prompt that failed to parse — validation rejects it before any
    /// plan is built, so this never reaches the runtime.
    #[must_use]
    pub fn empty() -> Self {
        Self { parts: Vec::new() }
    }

    /// Renders the template with `resolve` answering hole paths.
    #[must_use]
    pub fn render(&self, resolve: &dyn Fn(&Path) -> Option<serde_json::Value>) -> String {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                TemplatePart::Text(text) => out.push_str(text),
                TemplatePart::Hole(path) => match resolve(path) {
                    Some(serde_json::Value::String(s)) => out.push_str(&s),
                    Some(v) => out.push_str(&v.to_string()),
                    None => out.push_str(&format!("{{{{{path}}}}}")),
                },
            }
        }
        out
    }
}

/// A branch condition: a comparison over paths and literals.
///
/// Kept as an enum rather than an operator-precedence grammar: the language
/// has no user-defined functions and deliberately no arithmetic, so a flat
/// sum-of-products is the whole grammar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "serde_json::Value", into = "serde_json::Value")]
pub enum Expr {
    /// A path's value is truthy (non-null, non-false, non-empty string).
    Truthy(Path),
    /// A path's value equals a literal.
    Eq {
        path: Path,
        value: serde_json::Value,
    },
    /// A path's value is not equal to a literal.
    Ne {
        path: Path,
        value: serde_json::Value,
    },
    /// Both operands hold.
    All(Vec<Expr>),
    /// At least one operand holds.
    Any(Vec<Expr>),
    /// The operand does not hold.
    Not(Box<Expr>),
}

impl Expr {
    /// Parses an expression from a YAML-given string or map form.
    ///
    /// A plain string is a path, a single-hole template (`{{ path }}`),
    /// or their negations (`!path`, `!{{ path }}`); a map is
    /// `{all|any|not|eq|ne: ...}`. A single-hole template means "that
    /// value is truthy", which is the natural authoring form next to the
    /// interpolation syntax used everywhere else. Kept to the forms the
    /// language documents; anything else is an error.
    ///
    /// # Errors
    ///
    /// [`crate::HarnessError::Invalid`] for an unparseable form.
    pub fn parse_value(value: &serde_json::Value) -> Result<Self, crate::HarnessError> {
        match value {
            serde_json::Value::String(text) => {
                let (negated, body) = match text.strip_prefix('!') {
                    Some(rest) => (true, rest.trim_start()),
                    None => (false, text.as_str()),
                };
                let path = if body.starts_with("{{") {
                    let template = Template::parse(body)?;
                    if !template.is_single_hole() {
                        return Err(crate::HarnessError::Invalid(format!(
                            "a condition template must be one `{{{{ path }}}}`, got `{text}`"
                        )));
                    }
                    template
                        .single_hole()
                        .ok_or_else(|| {
                            crate::HarnessError::Invalid(format!(
                                "a condition template must be one `{{{{ path }}}}`, got `{text}`"
                            ))
                        })?
                        .clone()
                } else {
                    Path::parse(body)?
                };
                if negated {
                    Ok(Self::Not(Box::new(Self::Truthy(path))))
                } else {
                    Ok(Self::Truthy(path))
                }
            }
            serde_json::Value::Object(map) => {
                if let Some(v) = map.get("all") {
                    let items = expr_list(v)?;
                    if items.is_empty() {
                        return Err(crate::HarnessError::Invalid(
                            "`all` needs at least one operand".into(),
                        ));
                    }
                    return Ok(Self::All(items));
                }
                if let Some(v) = map.get("any") {
                    let items = expr_list(v)?;
                    if items.is_empty() {
                        return Err(crate::HarnessError::Invalid(
                            "`any` needs at least one operand".into(),
                        ));
                    }
                    return Ok(Self::Any(items));
                }
                if let Some(v) = map.get("not") {
                    return Ok(Self::Not(Box::new(Self::parse_value(v)?)));
                }
                if let Some(path) = map.get("eq") {
                    let value = map.get("value").ok_or_else(|| {
                        crate::HarnessError::Invalid("`eq` needs a `value`".into())
                    })?;
                    return Ok(Self::Eq {
                        path: Path::parse(path.as_str().ok_or_else(|| {
                            crate::HarnessError::Invalid("`eq.path` must be a string".into())
                        })?)?,
                        value: value.clone(),
                    });
                }
                if let Some(path) = map.get("ne") {
                    let value = map.get("value").ok_or_else(|| {
                        crate::HarnessError::Invalid("`ne` needs a `value`".into())
                    })?;
                    return Ok(Self::Ne {
                        path: Path::parse(path.as_str().ok_or_else(|| {
                            crate::HarnessError::Invalid("`ne.path` must be a string".into())
                        })?)?,
                        value: value.clone(),
                    });
                }
                Err(crate::HarnessError::Invalid(format!(
                    "unsupported expression object: {value}"
                )))
            }
            other => Err(crate::HarnessError::Invalid(format!(
                "an expression must be a path string or an object, got {other}"
            ))),
        }
    }

    /// Every path the expression reads.
    #[must_use]
    pub fn paths(&self) -> Vec<&Path> {
        match self {
            Self::Truthy(path) => vec![path],
            Self::Eq { path, .. } | Self::Ne { path, .. } => vec![path],
            Self::All(items) | Self::Any(items) => items.iter().flat_map(Self::paths).collect(),
            Self::Not(inner) => inner.paths(),
        }
    }

    /// The always-false expression. A compile-time fallback for a condition
    /// that failed to parse — validation rejects it before any plan is
    /// built, so this never reaches the runtime.
    #[must_use]
    pub fn truthy_false() -> Self {
        Self::All(Vec::new())
    }

    /// Evaluates the expression against `resolve`.
    #[must_use]
    pub fn eval(&self, resolve: &dyn Fn(&Path) -> Option<serde_json::Value>) -> bool {
        match self {
            Self::Truthy(path) => resolve(path).is_some_and(|v| is_truthy(&v)),
            Self::Eq { path, value } => resolve(path).is_some_and(|v| &v == value),
            Self::Ne { path, value } => resolve(path).is_none_or(|v| v != *value),
            Self::All(items) => items.iter().all(|e| e.eval(resolve)),
            Self::Any(items) => items.iter().any(|e| e.eval(resolve)),
            Self::Not(inner) => !inner.eval(resolve),
        }
    }
}

impl From<String> for Template {
    fn from(text: String) -> Self {
        Self::parse(&text).unwrap_or_else(|_| Self {
            parts: vec![TemplatePart::Text(text)],
        })
    }
}

impl From<Template> for String {
    fn from(t: Template) -> String {
        let mut out = String::new();
        for part in &t.parts {
            match part {
                TemplatePart::Text(text) => out.push_str(text),
                TemplatePart::Hole(path) => {
                    out.push_str("{{");
                    out.push_str(&path.as_text());
                    out.push_str("}}");
                }
            }
        }
        out
    }
}

impl From<serde_json::Value> for Expr {
    fn from(value: serde_json::Value) -> Self {
        Self::parse_value(&value).unwrap_or(Self::All(Vec::new()))
    }
}

impl From<Expr> for serde_json::Value {
    fn from(e: Expr) -> Self {
        match e {
            Expr::Truthy(p) => serde_json::Value::String(p.as_text()),
            Expr::Not(inner) => serde_json::json!({ "not": serde_json::Value::from(*inner) }),
            Expr::All(items) => serde_json::json!({ "all": items }),
            Expr::Any(items) => serde_json::json!({ "any": items }),
            Expr::Eq { path, value } => {
                serde_json::json!({ "eq": path.as_text(), "value": value })
            }
            Expr::Ne { path, value } => {
                serde_json::json!({ "ne": path.as_text(), "value": value })
            }
        }
    }
}

fn expr_list(value: &serde_json::Value) -> Result<Vec<Expr>, crate::HarnessError> {
    let items = value.as_array().ok_or_else(|| {
        crate::HarnessError::Invalid("`all`/`any` need an array of expressions".into())
    })?;
    items.iter().map(Expr::parse_value).collect()
}

/// The truthiness rule: null and false are false, an empty string is false,
/// an empty array/object is false, zero is *not* false (it is data).
#[must_use]
pub fn is_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
        serde_json::Value::Number(_) => true,
    }
}
