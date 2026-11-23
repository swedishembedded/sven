// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The typed contract of one model-driven method.

use std::marker::PhantomData;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;

/// How a method's result is obtained.
///
/// Independent of the contract: the same signature can be served either way,
/// and which one is used is a configuration decision rather than something a
/// caller passes at the call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Strategy {
    /// One schema-constrained turn with no tools.
    ///
    /// For work that needs interpretation but not investigation:
    /// classification, extraction, assessment, summarisation. The model cannot
    /// call anything, so the result depends only on what it was given.
    #[default]
    Predict,
    /// A full tool-using turn that ends by producing the result.
    ///
    /// For work that has to find things out first - reading files, running
    /// commands, searching - before it can answer. Uses the tools the engine
    /// was given ([`crate::Toolset`] and [`crate::EngineBuilder::tool`]).
    Investigate,
}

/// What a model-driven method promises: its instructions, its return type, and
/// the limits on obtaining it.
///
/// A method is a value rather than a function so that it can be declared once
/// next to the type it returns, reused across agents, and adjusted
/// (`max_repairs`, `postcondition`) without rewriting the call site.
pub struct Method<T> {
    pub(crate) name: String,
    pub(crate) role: Option<String>,
    pub(crate) task: String,
    pub(crate) strategy: Strategy,
    pub(crate) max_repairs: u32,
    #[allow(clippy::type_complexity)]
    pub(crate) postcondition: Option<Box<dyn Fn(&T) -> Result<(), String> + Send + Sync>>,
    /// A schema given at run time, for a method returning any JSON value
    /// ([`Method::with_schema`]); `None`: the return type's own.
    pub(crate) runtime_schema: Option<serde_json::Value>,
    pub(crate) marker: PhantomData<fn() -> T>,
}

impl<T> Method<T>
where
    T: DeserializeOwned + JsonSchema,
{
    /// Declares a method called `name`, which is also the schema name the
    /// provider is given.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            role: None,
            task: String::new(),
            strategy: Strategy::default(),
            max_repairs: 1,
            postcondition: None,
            runtime_schema: None,
            marker: PhantomData,
        }
    }

    /// The stable role the agent plays across every call - who it is, and the
    /// constraints that always apply.
    ///
    /// Belongs to the agent rather than the task, and is kept separate from
    /// [`Self::task`] so it can sit in the cacheable prefix of a prompt.
    #[must_use]
    pub fn role(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }

    /// The instructions for this one task.
    #[must_use]
    pub fn task(mut self, task: impl Into<String>) -> Self {
        self.task = task.into();
        self
    }

    /// How the result is obtained. Defaults to [`Strategy::Predict`].
    #[must_use]
    pub fn strategy(mut self, strategy: Strategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// How many further attempts a rejected answer is worth. Defaults to 1.
    ///
    /// Bounds the correction loop. Zero means the first answer is the only one:
    /// a rejected answer fails immediately.
    #[must_use]
    pub fn max_repairs(mut self, repairs: u32) -> Self {
        self.max_repairs = repairs;
        self
    }

    /// An invariant the result must satisfy beyond having the right shape.
    ///
    /// Return-type validation establishes only that a value has an acceptable
    /// structure. A postcondition is where "risk is a percentage" or "the cited
    /// file exists" belongs. Its error message is handed back to the model as
    /// feedback, so write it as an instruction rather than a complaint.
    #[must_use]
    pub fn postcondition(
        mut self,
        check: impl Fn(&T) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.postcondition = Some(Box::new(check));
        self
    }

    /// The method's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The JSON schema of the return type, derived from the type itself.
    ///
    /// Deriving rather than accepting a hand-written schema is what stops the
    /// description the model is given from drifting away from the type the
    /// caller actually gets.
    ///
    /// A method returning any JSON value may be given a schema at run time
    /// instead ([`Method::with_schema`]); its schema is then that one.
    #[must_use]
    pub fn schema(&self) -> serde_json::Value {
        if let Some(schema) = &self.runtime_schema {
            return schema.clone();
        }
        serde_json::to_value(schemars::schema_for!(T))
            .unwrap_or_else(|_| serde_json::json!({"type": "object"}))
    }

    /// The instruction text for one call, given a rendered input: the task,
    /// the input, and the JSON schema the answer must match. The schema is
    /// also the request's response format, but a provider that cannot
    /// constrain decoding ignores that field, so the prompt describes it too.
    pub(crate) fn instruction(&self, input: &str) -> String {
        let mut out = String::new();
        if !self.task.is_empty() {
            out.push_str(&self.task);
            out.push_str("\n\n");
        }
        out.push_str("Input:\n");
        out.push_str(input);
        out.push_str(
            "\n\nAnswer with nothing but one JSON object that matches this JSON schema:\n",
        );
        out.push_str(&self.schema().to_string());
        out
    }
}

impl Method<serde_json::Value> {
    /// The schema the answer must match, known only at run time: a caller's
    /// own document of what it wants (a JSON Schema a user wrote, say). It is
    /// the response format the provider is asked to constrain decoding to and
    /// the shape the prompt describes, in place of [`serde_json::Value`]'s,
    /// which allows anything.
    ///
    /// Only a method returning any JSON value takes one: a typed return type
    /// is its own schema, and a second one could contradict it. The answer is
    /// parsed as JSON, not validated against `schema`; a caller that needs it
    /// held to the schema checks it in a [`Self::postcondition`].
    #[must_use]
    pub fn with_schema(mut self, schema: serde_json::Value) -> Self {
        self.runtime_schema = Some(schema);
        self
    }
}
