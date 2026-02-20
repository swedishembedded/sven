use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use tracing::debug;

use crate::{Step, StepQueue};

/// Parse a markdown document into a [`StepQueue`].
///
/// Rules:
/// - Every `##` heading starts a new step; the heading text becomes the label.
/// - Content before the first `##` heading (or the entire document if there are
///   no `##` headings) is treated as a single unlabelled step.
/// - `#` and `###`+ headings are kept as body text inside the current step.
pub fn parse_markdown_steps(input: &str) -> StepQueue {
    let mut steps: Vec<Step> = Vec::new();
    let mut current_label: Option<String> = None;
    let mut current_body = String::new();
    let mut inside_h2 = false;
    let mut h2_text = String::new();

    let parser = Parser::new(input);
    for event in parser {
        match event {
            Event::Start(Tag::Heading { level: HeadingLevel::H2, .. }) => {
                // Flush previous step
                flush_step(&mut steps, current_label.take(), &mut current_body);
                inside_h2 = true;
                h2_text.clear();
            }
            Event::End(TagEnd::Heading(HeadingLevel::H2)) => {
                inside_h2 = false;
                current_label = Some(h2_text.trim().to_string());
            }
            Event::Text(t) | Event::Code(t) if inside_h2 => {
                h2_text.push_str(&t);
            }
            Event::Text(t) => {
                current_body.push_str(&t);
            }
            Event::SoftBreak | Event::HardBreak => {
                current_body.push('\n');
            }
            Event::Code(t) => {
                current_body.push('`');
                current_body.push_str(&t);
                current_body.push('`');
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                current_body.push_str("\n\n");
            }
            Event::Start(Tag::CodeBlock(_)) => {
                current_body.push_str("```\n");
            }
            Event::End(TagEnd::CodeBlock) => {
                current_body.push_str("```\n\n");
            }
            _ => {}
        }
    }

    // Final flush
    flush_step(&mut steps, current_label, &mut current_body);

    debug!(steps = steps.len(), "parsed markdown steps");

    // If nothing was found (empty input) create one empty step so the caller
    // always has something to process.
    if steps.is_empty() {
        steps.push(Step { label: None, content: input.trim().to_string() });
    }

    StepQueue::from(steps)
}

fn flush_step(out: &mut Vec<Step>, label: Option<String>, body: &mut String) {
    let content = body.trim().to_string();
    body.clear();

    // Skip empty steps with no label
    if content.is_empty() && label.is_none() {
        return;
    }

    // If the step only has a label but no body, use the label as content
    let content = if content.is_empty() {
        label.clone().unwrap_or_default()
    } else {
        content
    };

    out.push(Step { label, content });
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Basic parsing ─────────────────────────────────────────────────────────

    #[test]
    fn single_step_no_heading() {
        let q = parse_markdown_steps("hello world");
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn empty_input_gives_single_empty_step() {
        let q = parse_markdown_steps("");
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn whitespace_only_input_gives_one_step() {
        let q = parse_markdown_steps("   \n\n   ");
        assert_eq!(q.len(), 1);
    }

    // ── H2 section splitting ──────────────────────────────────────────────────

    #[test]
    fn multiple_h2_sections() {
        let md = "## Step one\nDo this.\n\n## Step two\nDo that.";
        let mut q = parse_markdown_steps(md);
        assert_eq!(q.len(), 2);
        let s1 = q.pop().unwrap();
        assert_eq!(s1.label.as_deref(), Some("Step one"));
        assert!(s1.content.contains("Do this"));
        let s2 = q.pop().unwrap();
        assert_eq!(s2.label.as_deref(), Some("Step two"));
    }

    #[test]
    fn preamble_before_h2() {
        let md = "Preamble text.\n\n## Section A\nContent.";
        let mut q = parse_markdown_steps(md);
        assert_eq!(q.len(), 2);
        let first = q.pop().unwrap();
        assert!(first.label.is_none());
        assert!(first.content.contains("Preamble"));
    }

    #[test]
    fn h1_heading_does_not_split_steps() {
        // H1 is document title, not a step boundary
        let md = "# Title\n\nSome content.\n\n## Real Step\nDo it.";
        let mut q = parse_markdown_steps(md);
        // Preamble (H1 + content) + 1 H2 step
        assert_eq!(q.len(), 2);
        let last = { let _ = q.pop(); q.pop().unwrap() };
        assert_eq!(last.label.as_deref(), Some("Real Step"));
    }

    #[test]
    fn h3_heading_does_not_split_steps() {
        let md = "## Parent\nIntro.\n\n### Sub-section\nDetails.";
        let q = parse_markdown_steps(md);
        // Only 1 H2 step; H3 is part of its body
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn step_label_strips_whitespace() {
        let md = "##   Trimmed Label   \nContent.";
        let mut q = parse_markdown_steps(md);
        assert_eq!(q.pop().unwrap().label.as_deref(), Some("Trimmed Label"));
    }

    #[test]
    fn five_steps_parsed_in_order() {
        let md = (1..=5)
            .map(|i| format!("## Step {i}\nDo step {i}."))
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut q = parse_markdown_steps(&md);
        assert_eq!(q.len(), 5);
        for i in 1..=5usize {
            let s = q.pop().unwrap();
            assert_eq!(s.label.as_deref(), Some(format!("Step {i}").as_str()));
        }
    }

    // ── Content preservation ──────────────────────────────────────────────────

    #[test]
    fn step_content_contains_body_text() {
        let md = "## My Step\nLine one.\nLine two.";
        let mut q = parse_markdown_steps(md);
        let s = q.pop().unwrap();
        assert!(s.content.contains("Line one"));
        assert!(s.content.contains("Line two"));
    }

    #[test]
    fn step_content_includes_code_block_markers() {
        let md = "## Step\n```rust\nfn main() {}\n```";
        let mut q = parse_markdown_steps(md);
        let s = q.pop().unwrap();
        assert!(s.content.contains("```"), "code block markers should be preserved");
    }

    #[test]
    fn step_without_body_uses_label_as_content() {
        // H2 heading with no following text
        let md = "## Only A Label";
        let mut q = parse_markdown_steps(md);
        let s = q.pop().unwrap();
        assert!(!s.content.is_empty(), "step must have non-empty content");
    }
}
