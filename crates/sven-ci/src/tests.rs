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

    // ── resolve_model_cfg ─────────────────────────────────────────────────────

    use crate::runner::resolve_model_cfg;
    use sven_config::ModelConfig;

    fn openai_base() -> ModelConfig {
        // This is what the default Config produces: provider=openai with
        // api_key_env pointing at OPENAI_API_KEY.
        ModelConfig {
            provider: "openai".into(),
            name: "gpt-4o".into(),
            api_key_env: Some("OPENAI_API_KEY".into()),
            api_key: None,
            ..ModelConfig::default()
        }
    }

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

    #[test]
    fn provider_change_clears_inherited_api_key_env() {
        // Regression: switching provider=openai → anthropic must NOT carry over
        // OPENAI_API_KEY; doing so sends the wrong key and gets a 401.
        let cfg = resolve_model_cfg(&openai_base(), "anthropic/claude-sonnet-4-5");
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.name, "claude-sonnet-4-5");
        assert!(
            cfg.api_key_env.is_none(),
            "api_key_env must be cleared when provider changes (was OPENAI_API_KEY, now anthropic)"
        );
        assert!(cfg.api_key.is_none(), "api_key must be cleared when provider changes");
    }

    #[test]
    fn same_provider_model_override_keeps_api_key_env() {
        // When only the model name changes within the same provider, the key config
        // must be preserved (user may have set a custom key for their OpenAI account).
        let cfg = resolve_model_cfg(&openai_base(), "gpt-4o-mini");
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.name, "gpt-4o-mini");
        assert_eq!(
            cfg.api_key_env.as_deref(),
            Some("OPENAI_API_KEY"),
            "api_key_env must be kept when provider does not change"
        );
    }

    #[test]
    fn bare_provider_override_clears_api_key_env() {
        // "anthropic" alone (no model) should still clear the inherited key env.
        let cfg = resolve_model_cfg(&openai_base(), "anthropic");
        assert_eq!(cfg.provider, "anthropic");
        assert!(cfg.api_key_env.is_none(), "api_key_env must be cleared for bare provider change");
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
