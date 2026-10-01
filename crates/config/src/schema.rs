// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The keys a configuration section recognises, as its owner declares them,
//! and the report of every key in a document that no section recognises.
//!
//! A section's owner - the crate that reads it - describes its keys with a
//! [`Schema`] next to the type that holds them. The program that loads a
//! document composes its sections' schemas into one and asks the document
//! which of its keys none of them knows ([`crate::ConfigDocument::unknown_keys`]).
//! This crate knows no section itself.

use serde_yaml::Value;

/// The keys of a configuration value: which a section recognises, and how
/// far into each the report looks.
#[derive(Clone, Debug)]
pub struct Schema(Kind);

#[derive(Clone, Debug)]
enum Kind {
    /// Any value, not looked into.
    Value,
    /// A mapping with exactly these keys.
    Fields(Vec<(&'static str, Schema)>),
    /// A mapping whose keys the user chooses (a provider name, a server
    /// name); each value is described by the inner schema.
    Entries(Box<Schema>),
    /// Accepted but acted on by nothing; reported once, with the reason.
    Ignored(&'static str),
}

impl Schema {
    /// Any value, not looked into: a scalar, a list, a free-form mapping.
    #[must_use]
    pub fn value() -> Self {
        Self(Kind::Value)
    }

    /// A mapping with exactly the keys `fields` names, each described by its
    /// own schema.
    #[must_use]
    pub fn fields(fields: impl IntoIterator<Item = (&'static str, Schema)>) -> Self {
        Self(Kind::Fields(fields.into_iter().collect()))
    }

    /// A mapping with exactly the keys `names`, none of them looked into.
    #[must_use]
    pub fn keys(names: &[&'static str]) -> Self {
        Self::fields(names.iter().map(|&name| (name, Self::value())))
    }

    /// A mapping whose keys the user chooses, every value described by
    /// `each`.
    #[must_use]
    pub fn entries(each: Schema) -> Self {
        Self(Kind::Entries(Box::new(each)))
    }

    /// A section accepted but acted on by nothing: reported once, by name,
    /// with `reason`, however many keys it holds.
    #[must_use]
    pub fn ignored(reason: &'static str) -> Self {
        Self(Kind::Ignored(reason))
    }

    /// This mapping with `key` described by `schema`, replacing an existing
    /// description of it. How a section composes keys whose owners differ.
    ///
    /// # Panics
    ///
    /// If this schema is not a mapping of [`Self::fields`] - a composition
    /// error in the program, not in the user's file.
    #[must_use]
    pub fn with(mut self, key: &'static str, schema: Schema) -> Self {
        let Kind::Fields(fields) = &mut self.0 else {
            panic!("`{key}` added to a schema that is not a mapping of fields");
        };
        match fields.iter_mut().find(|(name, _)| *name == key) {
            Some(field) => field.1 = schema,
            None => fields.push((key, schema)),
        }
        self
    }

    /// Records a warning for every key under `value` this schema does not
    /// recognise, and one for every ignored section present. `path` is the
    /// dotted location of `value` in the document (empty at the root).
    pub(crate) fn report(&self, value: &Value, path: &str, warnings: &mut Vec<String>) {
        let Value::Mapping(map) = value else {
            return;
        };
        let keys = map.iter().filter_map(|(key, value)| match key {
            Value::String(key) => Some((key.as_str(), value)),
            _ => None,
        });
        match &self.0 {
            Kind::Value | Kind::Ignored(_) => {}
            Kind::Entries(each) => {
                for (key, value) in keys {
                    each.report(value, &format!("{path}.{key}"), warnings);
                }
            }
            Kind::Fields(fields) => {
                for (key, value) in keys {
                    let full_path = if path.is_empty() {
                        key.to_string()
                    } else {
                        format!("{path}.{key}")
                    };
                    match fields.iter().find(|(name, _)| *name == key) {
                        None => warnings.push(format!(
                            "Unrecognised config field `{path}.{key}` - check spelling or update sven"
                        )),
                        Some((_, Schema(Kind::Ignored(reason)))) => warnings.push(format!(
                            "Config section `{full_path}` is ignored: {reason}"
                        )),
                        Some((_, schema)) => schema.report(value, &full_path, warnings),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(schema: &Schema, yaml: &str) -> Vec<String> {
        let value: Value = serde_yaml::from_str(yaml).unwrap();
        let mut warnings = Vec::new();
        schema.report(&value, "", &mut warnings);
        warnings
    }

    fn sample() -> Schema {
        Schema::fields([
            ("model", Schema::keys(&["provider", "name"])),
            (
                "servers",
                Schema::entries(Schema::fields([
                    ("transport", Schema::keys(&["type", "url"])),
                    ("env", Schema::value()),
                ])),
            ),
            (
                "memory",
                Schema::fields([
                    ("file", Schema::value()),
                    ("learning", Schema::ignored("nothing reads it")),
                ]),
            ),
        ])
    }

    #[test]
    fn known_keys_earn_no_warning() {
        let yaml = "model: {provider: openai, name: gpt}\nservers:\n  a:\n    transport: {type: http, url: u}\n    env: {ANY: thing}\n";
        assert_eq!(report(&sample(), yaml), Vec::<String>::new());
    }

    #[test]
    fn an_unknown_key_is_named_by_its_path() {
        let warnings = report(
            &sample(),
            "modle: x\nmodel: {nme: y}\nservers: {a: {transport: {typ: http}}}\n",
        );
        assert_eq!(
            warnings,
            [
                "Unrecognised config field `.modle` - check spelling or update sven",
                "Unrecognised config field `model.nme` - check spelling or update sven",
                "Unrecognised config field `servers.a.transport.typ` - check spelling or update sven",
            ]
        );
    }

    #[test]
    fn an_ignored_section_earns_one_warning_whatever_it_holds() {
        let warnings = report(&sample(), "memory:\n  file: f\n  learning: {a: 1, b: 2}\n");
        assert_eq!(
            warnings,
            ["Config section `memory.learning` is ignored: nothing reads it"]
        );
    }

    #[test]
    fn a_free_form_value_is_not_looked_into() {
        assert!(report(&sample(), "servers: {a: {env: {whatever: 1}}}\n").is_empty());
    }

    #[test]
    fn with_adds_and_replaces_a_key() {
        let schema = Schema::keys(&["a"])
            .with("b", Schema::keys(&["c"]))
            .with("a", Schema::ignored("gone"));
        assert_eq!(
            report(&schema, "a: 1\nb: {c: 1, d: 2}\n"),
            [
                "Config section `a` is ignored: gone",
                "Unrecognised config field `b.d` - check spelling or update sven",
            ]
        );
    }
}
