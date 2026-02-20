/// Tests for CI-mode step processing and option handling.
///
/// These are unit-level tests that exercise the logic that translates
/// CLI inputs into step queues and agent invocations without starting
/// a real network connection.
#[cfg(test)]
mod tests {
    use sven_input::{parse_markdown_steps, StepQueue};

    // ── Input parsing (re-validates sven-input from the CI consumer's perspective) ──

    #[test]
    fn plain_text_input_becomes_one_step() {
        let mut q = parse_markdown_steps("Do something useful.");
        assert_eq!(q.len(), 1);
        assert!(q.pop().unwrap().content.contains("Do something useful"));
    }

    #[test]
    fn markdown_with_three_sections_becomes_three_steps() {
        let md = "## A\nStep A.\n\n## B\nStep B.\n\n## C\nStep C.";
        let q = parse_markdown_steps(md);
        assert_eq!(q.len(), 3);
    }

    #[test]
    fn empty_input_gives_single_step() {
        let q = parse_markdown_steps("");
        assert_eq!(q.len(), 1);
    }

    // ── Extra prompt prepend logic ────────────────────────────────────────────

    #[test]
    fn extra_prompt_can_be_prepended_to_queue() {
        // Simulate what CiRunner does when extra_prompt is Some
        let base = parse_markdown_steps("## Step 1\nDo it.");
        let extra_step = sven_input::Step { label: None, content: "Extra context.".into() };
        let mut prepended = StepQueue::from(vec![extra_step]);
        let mut base = base;
        while let Some(s) = base.pop() {
            prepended.push(s);
        }
        assert_eq!(prepended.len(), 2);
        let first = { let mut p = prepended; p.pop().unwrap() };
        assert_eq!(first.content, "Extra context.");
    }

    #[test]
    fn queue_without_extra_prompt_preserves_step_count() {
        let q = parse_markdown_steps("## A\none\n\n## B\ntwo");
        assert_eq!(q.len(), 2);
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
        let mut q = parse_markdown_steps(md);
        let step = q.pop().unwrap();
        assert_eq!(step.label.as_deref(), Some("Analyse codebase"));
    }

    #[test]
    fn unlabelled_step_has_none_label() {
        let q = parse_markdown_steps("Just text, no heading.");
        let mut q = q;
        let step = q.pop().unwrap();
        assert!(step.label.is_none());
    }
}
