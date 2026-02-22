// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
/// Tests for CI-mode step processing and option handling.
///
/// These are unit-level tests that exercise the logic that translates
/// CLI inputs into step queues and agent invocations without starting
/// a real network connection.
#[cfg(test)]
mod tests {
    use sven_input::{parse_workflow, StepQueue};

    // ── Input parsing (re-validates sven-input from the CI consumer's perspective) ──

    #[test]
    fn plain_text_input_becomes_one_fallback_step() {
        let mut w = parse_workflow("Do something useful.");
        assert_eq!(w.steps.len(), 1);
        assert!(w.steps.pop().unwrap().content.contains("Do something useful"));
    }

    #[test]
    fn markdown_with_three_sections_becomes_three_steps() {
        let md = "## A\nStep A.\n\n## B\nStep B.\n\n## C\nStep C.";
        let w = parse_workflow(md);
        assert_eq!(w.steps.len(), 3);
    }

    #[test]
    fn empty_input_gives_single_fallback_step() {
        let w = parse_workflow("");
        assert_eq!(w.steps.len(), 1);
    }

    #[test]
    fn h1_becomes_title_and_preamble_goes_to_system_prompt() {
        let md = "# My Workflow\n\nThis context goes to the system prompt.\n\n## Step one\nDo it.";
        let mut w = parse_workflow(md);
        assert_eq!(w.title.as_deref(), Some("My Workflow"));
        assert!(w.system_prompt_append.as_deref()
            .map(|s| s.contains("context goes to the system prompt"))
            .unwrap_or(false));
        assert_eq!(w.steps.len(), 1);
        assert_eq!(w.steps.pop().unwrap().label.as_deref(), Some("Step one"));
    }

    // ── Extra prompt prepend logic ────────────────────────────────────────────

    #[test]
    fn extra_prompt_can_be_prepended_to_queue() {
        // Simulate what CiRunner does when extra_prompt is Some
        let mut base = parse_workflow("## Step 1\nDo it.").steps;
        let extra_step = sven_input::Step {
            label: None,
            content: "Extra context.".into(),
            options: sven_input::StepOptions::default(),
        };
        let mut prepended = StepQueue::from(vec![extra_step]);
        while let Some(s) = base.pop() {
            prepended.push(s);
        }
        assert_eq!(prepended.len(), 2);
        let first = { let mut p = prepended; p.pop().unwrap() };
        assert_eq!(first.content, "Extra context.");
    }

    #[test]
    fn queue_without_extra_prompt_preserves_step_count() {
        let w = parse_workflow("## A\none\n\n## B\ntwo");
        assert_eq!(w.steps.len(), 2);
    }

    // ── Model name parsing (provider/model format) ────────────────────────────

    #[test]
    fn slash_separated_model_splits_provider_and_name() {
        let spec = "anthropic/claude-opus-4-5";
        let (provider, name) = spec.split_once('/').unwrap();
        assert_eq!(provider, "anthropic");
        assert_eq!(name, "claude-opus-4-5");
    }

    #[test]
    fn bare_model_name_has_no_slash() {
        let spec = "gpt-4o";
        assert!(spec.split_once('/').is_none(), "bare name has no slash");
    }

    // ── Step label extraction ─────────────────────────────────────────────────

    #[test]
    fn step_label_preserved_for_logging() {
        let md = "## Analyse codebase\nRead the files.";
        let mut w = parse_workflow(md);
        let step = w.steps.pop().unwrap();
        assert_eq!(step.label.as_deref(), Some("Analyse codebase"));
    }

    #[test]
    fn plain_text_fallback_step_has_none_label() {
        let mut w = parse_workflow("Just text, no heading.");
        let step = w.steps.pop().unwrap();
        assert!(step.label.is_none());
    }
}
