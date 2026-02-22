// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::collections::VecDeque;

/// Per-step configuration parsed from inline `<!-- sven: ... -->` HTML comments.
#[derive(Debug, Clone, Default)]
pub struct StepOptions {
    /// Override agent mode for this step (e.g. "research", "plan", "agent")
    pub mode: Option<String>,
    /// Override model for this step (e.g. "gpt-4o" or "anthropic/claude-opus-4-5")
    pub model: Option<String>,
    /// Step-level timeout in seconds (overrides the runner default)
    pub timeout_secs: Option<u64>,
    /// Optional cache key — if set, a matching cached result is reused
    pub cache_key: Option<String>,
}

/// A single step / message to be sent to the agent.
#[derive(Debug, Clone)]
pub struct Step {
    /// Optional heading label extracted from a `##` section
    pub label: Option<String>,
    /// The actual content of the step
    pub content: String,
    /// Per-step configuration parsed from `<!-- sven: ... -->` comments
    pub options: StepOptions,
}

/// A queue of steps that are delivered to the agent one at a time.
/// Preserves FIFO ordering.
#[derive(Debug, Default)]
pub struct StepQueue(VecDeque<Step>);

impl StepQueue {
    pub fn new() -> Self { Self(VecDeque::new()) }

    pub fn push(&mut self, step: Step) { self.0.push_back(step); }

    pub fn pop(&mut self) -> Option<Step> { self.0.pop_front() }

    pub fn peek(&self) -> Option<&Step> { self.0.front() }

    pub fn is_empty(&self) -> bool { self.0.is_empty() }

    pub fn len(&self) -> usize { self.0.len() }
}

impl From<Vec<Step>> for StepQueue {
    fn from(v: Vec<Step>) -> Self {
        Self(v.into_iter().collect())
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn step(content: &str) -> Step {
        Step { label: None, content: content.into(), options: StepOptions::default() }
    }

    fn labelled(label: &str, content: &str) -> Step {
        Step { label: Some(label.into()), content: content.into(), options: StepOptions::default() }
    }

    #[test]
    fn new_queue_is_empty() {
        let q = StepQueue::new();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn push_increases_len() {
        let mut q = StepQueue::new();
        q.push(step("a"));
        q.push(step("b"));
        assert_eq!(q.len(), 2);
        assert!(!q.is_empty());
    }

    #[test]
    fn pop_returns_fifo_order() {
        let mut q = StepQueue::new();
        q.push(step("first"));
        q.push(step("second"));
        q.push(step("third"));

        assert_eq!(q.pop().unwrap().content, "first");
        assert_eq!(q.pop().unwrap().content, "second");
        assert_eq!(q.pop().unwrap().content, "third");
        assert!(q.pop().is_none());
    }

    #[test]
    fn peek_does_not_consume() {
        let mut q = StepQueue::new();
        q.push(step("peek-me"));
        assert_eq!(q.peek().unwrap().content, "peek-me");
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn pop_empty_returns_none() {
        let mut q = StepQueue::new();
        assert!(q.pop().is_none());
    }

    #[test]
    fn from_vec_preserves_order() {
        let steps = vec![step("x"), labelled("L", "y"), step("z")];
        let mut q = StepQueue::from(steps);
        assert_eq!(q.pop().unwrap().content, "x");
        let l = q.pop().unwrap();
        assert_eq!(l.label.as_deref(), Some("L"));
        assert_eq!(q.pop().unwrap().content, "z");
    }

    #[test]
    fn labels_are_preserved() {
        let mut q = StepQueue::from(vec![labelled("My Step", "do it")]);
        let s = q.pop().unwrap();
        assert_eq!(s.label.as_deref(), Some("My Step"));
        assert_eq!(s.content, "do it");
    }
}
