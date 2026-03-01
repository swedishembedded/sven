// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use tracing::debug;

use crate::{Step, StepOptions, StepQueue};

/// The result of parsing a workflow markdown document.
///
/// ```markdown
/// # Title                          ← title field
///
/// Preamble text before first ##    ← system_prompt_append field
///
/// ## Step label                    ← first step
/// <!-- sven: mode=research -->
/// Step body…
/// ```
pub struct ParsedWorkflow {
    /// Text of the first `#` H1 heading, used as the conversation title.
    pub title: Option<String>,
    /// Body text between the H1 and the first `##` heading.
    /// Appended to the agent system prompt, not sent as a user message.
    pub system_prompt_append: Option<String>,
    /// The ordered queue of steps derived from `##` H2 sections.
    pub steps: StepQueue,
}

/// Parse a markdown workflow document into a [`ParsedWorkflow`].
///
/// Rules:
/// - The first `#` H1 heading becomes the conversation title and is not
///   included in any step or in the system prompt append.
/// - Content between the H1 (or document start) and the first `##` H2
///   heading is collected as `system_prompt_append`.
/// - Every `##` H2 heading starts a new step; the heading text is the label.
/// - `#` and `###`+ headings are kept as body text inside the current step.
/// - `<!-- sven: key=value ... -->` comments set per-step options and are
///   removed from the step body.
/// - All other HTML comments are stripped from output.
/// - **Fallback**: if the document contains no `##` H2 headings, the entire
///   body (after the H1, if any) is returned as a single unlabelled step so
///   that plain-text piped input continues to work.
pub fn parse_workflow(input: &str) -> ParsedWorkflow {
    let mut title: Option<String> = None;
    let mut preamble = String::new();
    let mut steps: Vec<Step> = Vec::new();

    // Phase tracking
    let mut inside_h1 = false;
    let mut h1_text = String::new();
    let mut h1_done = false; // once H1 is consumed, preamble collection begins
    let mut in_step = false; // true once the first H2 has been seen

    let mut current_label: Option<String> = None;
    let mut current_body = String::new();
    let mut current_opts = StepOptions::default();
    let mut inside_h2 = false;
    let mut h2_text = String::new();

    let parser = Parser::new(input);

    for event in parser {
        match event {
            // ── H1: title ────────────────────────────────────────────────────
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) => {
                inside_h1 = true;
                h1_text.clear();
            }
            Event::End(TagEnd::Heading(HeadingLevel::H1)) => {
                inside_h1 = false;
                if !h1_done {
                    title = Some(h1_text.trim().to_string());
                    h1_done = true;
                }
                // Either way, do not add H1 text to preamble or step body.
            }
            Event::Text(t) | Event::Code(t) if inside_h1 => {
                h1_text.push_str(&t);
            }

            // ── H2: step boundary ────────────────────────────────────────────
            Event::Start(Tag::Heading {
                level: HeadingLevel::H2,
                ..
            }) => {
                if in_step {
                    flush_step(
                        &mut steps,
                        current_label.take(),
                        &mut current_body,
                        std::mem::take(&mut current_opts),
                    );
                }
                inside_h2 = true;
                h2_text.clear();
                in_step = true;
            }
            Event::End(TagEnd::Heading(HeadingLevel::H2)) => {
                inside_h2 = false;
                current_label = Some(h2_text.trim().to_string());
            }
            Event::Text(t) | Event::Code(t) if inside_h2 => {
                h2_text.push_str(&t);
            }

            // ── Body text ────────────────────────────────────────────────────
            Event::Text(t) => {
                if in_step {
                    current_body.push_str(&t);
                } else {
                    h1_done = true; // any text before H1 or after H1 counts
                    preamble.push_str(&t);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if in_step {
                    current_body.push('\n');
                } else {
                    preamble.push('\n');
                }
            }
            Event::Code(t) => {
                let s = format!("`{t}`");
                if in_step {
                    current_body.push_str(&s);
                } else {
                    preamble.push_str(&s);
                }
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                if in_step {
                    current_body.push_str("\n\n");
                } else {
                    preamble.push_str("\n\n");
                }
            }
            Event::Start(Tag::CodeBlock(_)) => {
                let s = "```\n";
                if in_step {
                    current_body.push_str(s);
                } else {
                    preamble.push_str(s);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                let s = "```\n\n";
                if in_step {
                    current_body.push_str(s);
                } else {
                    preamble.push_str(s);
                }
            }

            // ── Sven directives and HTML comments ────────────────────────────
            Event::Html(html) => {
                let trimmed = html.trim();
                // Pass every HTML comment to the parser; it handles both the
                // explicit `<!-- sven: key=val -->` form and the implicit
                // `<!-- key=val -->` form (when known keys are present).
                // Non-option comments (no '=' or no known key) are ignored
                // inside parse_sven_comment_into.
                if in_step && trimmed.starts_with("<!--") && trimmed.contains("-->") {
                    parse_sven_comment_into(trimmed, &mut current_opts);
                }
                // All HTML comments are stripped from the step body output.
            }

            _ => {}
        }
    }

    // Final flush of the last step (if any)
    if in_step {
        flush_step(
            &mut steps,
            current_label,
            &mut current_body,
            std::mem::take(&mut current_opts),
        );
    }

    debug!(steps = steps.len(), "parsed workflow steps");

    // Fallback: no H2 sections found → treat entire body as one unlabelled
    // step so plain-text stdin continues to work.
    if steps.is_empty() {
        let content = if preamble.trim().is_empty() {
            input.trim().to_string()
        } else {
            preamble.trim().to_string()
        };
        steps.push(Step {
            label: None,
            content,
            options: StepOptions::default(),
        });
        return ParsedWorkflow {
            title,
            system_prompt_append: None,
            steps: StepQueue::from(steps),
        };
    }

    let system_prompt_append = {
        let t = preamble.trim().to_string();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    };

    ParsedWorkflow {
        title,
        system_prompt_append,
        steps: StepQueue::from(steps),
    }
}

/// Parse a `<!-- sven: key=value key2=value2 -->` or `<!-- key=value key2=value2 -->` comment into `opts`.
///
/// Supports both formats:
/// - `<!-- sven: mode=agent model=gpt-4o -->` (explicit sven: prefix)
/// - `<!-- mode=agent model=gpt-4o -->` (implicit, auto-detected by presence of known keys)
fn parse_sven_comment_into(comment: &str, opts: &mut StepOptions) {
    // Try explicit <!-- sven: ... --> format first
    let (start, end) = if let Some(i) = comment.find("<!-- sven:") {
        let start = i + "<!-- sven:".len();
        match comment[start..].find("-->") {
            Some(e) => (start, start + e),
            None => return,
        }
    } else {
        // Try implicit <!-- key=value --> format (must contain at least one known key)
        if let Some(start_idx) = comment.find("<!--") {
            let start = start_idx + "<!--".len();
            match comment[start..].find("-->") {
                Some(e) => {
                    let potential_content = comment[start..start + e].trim();
                    // Only parse if the content looks like pure key=value pairs:
                    // every whitespace-split token must contain '=' (no bare words
                    // like "step:"), and at least one must be a known sven key.
                    // This keeps "<!-- model=gpt-4o -->" working while correctly
                    // ignoring old formats like "<!-- step: mode=research -->".
                    let all_kv = potential_content
                        .split_whitespace()
                        .all(|t| t.contains('='));
                    let has_known_key = potential_content.split_whitespace().any(|t| {
                        matches!(
                            t.split_once('=').map(|(k, _)| k),
                            Some("mode" | "model" | "provider" | "timeout" | "cache_key")
                        )
                    });
                    if potential_content.contains('=') && all_kv && has_known_key {
                        (start, start + e)
                    } else {
                        return; // Not a step options comment
                    }
                }
                None => return,
            }
        } else {
            return;
        }
    };

    let inner = comment[start..end].trim();

    for token in inner.split_whitespace() {
        if let Some((key, val)) = token.split_once('=') {
            let val = val.trim_matches('"').trim_matches('\'');
            match key {
                "mode" => opts.mode = Some(val.to_string()),
                "provider" => opts.provider = Some(val.to_string()),
                "model" => opts.model = Some(val.to_string()),
                "timeout" => opts.timeout_secs = val.parse().ok(),
                "cache_key" => opts.cache_key = Some(val.to_string()),
                _ => {}
            }
        }
    }
}

fn flush_step(out: &mut Vec<Step>, label: Option<String>, body: &mut String, options: StepOptions) {
    let content = body.trim().to_string();
    body.clear();

    // Skip completely empty steps with no label
    if content.is_empty() && label.is_none() {
        return;
    }

    // If a step only has a label, use the label as content
    let content = if content.is_empty() {
        label.clone().unwrap_or_default()
    } else {
        content
    };

    out.push(Step {
        label,
        content,
        options,
    });
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Fallback / plain-text ─────────────────────────────────────────────────

    #[test]
    fn plain_text_no_headings_gives_single_fallback_step() {
        let w = parse_workflow("hello world");
        assert_eq!(w.steps.len(), 1);
        assert!(w.title.is_none());
        assert!(w.system_prompt_append.is_none());
    }

    #[test]
    fn empty_input_gives_single_fallback_step() {
        let w = parse_workflow("");
        assert_eq!(w.steps.len(), 1);
    }

    #[test]
    fn whitespace_only_gives_single_fallback_step() {
        let w = parse_workflow("   \n\n   ");
        assert_eq!(w.steps.len(), 1);
    }

    // ── H1 title extraction ───────────────────────────────────────────────────

    #[test]
    fn h1_becomes_title_not_a_step() {
        let md = "# My Workflow\n\n## Step one\nDo this.";
        let mut w = parse_workflow(md);
        assert_eq!(w.title.as_deref(), Some("My Workflow"));
        assert_eq!(w.steps.len(), 1);
        assert_eq!(w.steps.pop().unwrap().label.as_deref(), Some("Step one"));
    }

    #[test]
    fn no_h1_gives_no_title() {
        let md = "## Only a step\nContent.";
        let w = parse_workflow(md);
        assert!(w.title.is_none());
        assert_eq!(w.steps.len(), 1);
    }

    #[test]
    fn h1_text_not_in_step_content() {
        let md = "# The Title\n\n## Step\nBody text.";
        let mut w = parse_workflow(md);
        let step = w.steps.pop().unwrap();
        assert!(
            !step.content.contains("The Title"),
            "H1 text must not appear in step content"
        );
    }

    // ── Preamble → system_prompt_append ──────────────────────────────────────

    #[test]
    fn preamble_goes_to_system_prompt_not_step() {
        let md = "# Title\n\nIntroductory context.\n\n## Do work\nThe task.";
        let mut w = parse_workflow(md);
        assert_eq!(w.steps.len(), 1, "preamble must NOT become a step");
        assert!(w
            .system_prompt_append
            .as_deref()
            .map(|s| s.contains("Introductory context"))
            .unwrap_or(false));
        assert_eq!(w.steps.pop().unwrap().label.as_deref(), Some("Do work"));
    }

    #[test]
    fn no_preamble_gives_none_system_prompt_append() {
        let md = "# Title\n\n## Step\nDo it.";
        let w = parse_workflow(md);
        assert!(w.system_prompt_append.is_none());
    }

    #[test]
    fn preamble_without_h1_still_goes_to_system_prompt() {
        let md = "Some context before any step.\n\n## Step\nContent.";
        let w = parse_workflow(md);
        assert!(w
            .system_prompt_append
            .as_deref()
            .map(|s| s.contains("Some context"))
            .unwrap_or(false));
        assert_eq!(w.steps.len(), 1);
    }

    // ── H2 sections → steps ───────────────────────────────────────────────────

    #[test]
    fn multiple_h2_sections_each_become_a_step() {
        let md = "## Step one\nDo this.\n\n## Step two\nDo that.";
        let mut w = parse_workflow(md);
        assert_eq!(w.steps.len(), 2);
        let s1 = w.steps.pop().unwrap();
        assert_eq!(s1.label.as_deref(), Some("Step one"));
        assert!(s1.content.contains("Do this"));
        let s2 = w.steps.pop().unwrap();
        assert_eq!(s2.label.as_deref(), Some("Step two"));
    }

    #[test]
    fn step_label_strips_whitespace() {
        let md = "##   Trimmed Label   \nContent.";
        let mut w = parse_workflow(md);
        assert_eq!(
            w.steps.pop().unwrap().label.as_deref(),
            Some("Trimmed Label")
        );
    }

    #[test]
    fn five_steps_parsed_in_order() {
        let md = (1..=5)
            .map(|i| format!("## Step {i}\nDo step {i}."))
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut w = parse_workflow(&md);
        assert_eq!(w.steps.len(), 5);
        for i in 1..=5usize {
            let s = w.steps.pop().unwrap();
            assert_eq!(s.label.as_deref(), Some(format!("Step {i}").as_str()));
        }
    }

    #[test]
    fn h3_heading_does_not_split_steps() {
        let md = "## Parent\nIntro.\n\n### Sub-section\nDetails.";
        let w = parse_workflow(md);
        assert_eq!(w.steps.len(), 1);
    }

    #[test]
    fn step_without_body_uses_label_as_content() {
        let md = "## Only A Label";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert!(!s.content.is_empty());
    }

    // ── Content preservation ──────────────────────────────────────────────────

    #[test]
    fn step_content_contains_body_text() {
        let md = "## My Step\nLine one.\nLine two.";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert!(s.content.contains("Line one"));
        assert!(s.content.contains("Line two"));
    }

    #[test]
    fn step_content_includes_code_block_markers() {
        let md = "## Step\n```rust\nfn main() {}\n```";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert!(
            s.content.contains("```"),
            "code block markers should be preserved"
        );
    }

    // ── <!-- sven: ... --> directives ─────────────────────────────────────────

    #[test]
    fn sven_comment_sets_mode() {
        let md = "## My Step\n<!-- sven: mode=research -->\nDo some reading.";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert_eq!(s.options.mode.as_deref(), Some("research"));
        assert!(!s.content.contains("<!-- sven:"));
    }

    #[test]
    fn sven_comment_sets_timeout() {
        let md = "## Heavy Step\n<!-- sven: timeout=600 -->\nExpensive work.";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert_eq!(s.options.timeout_secs, Some(600));
    }

    #[test]
    fn sven_comment_sets_multiple_options() {
        let md = "## Step\n<!-- sven: mode=agent timeout=120 cache_key=abc -->\nWork.";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert_eq!(s.options.mode.as_deref(), Some("agent"));
        assert_eq!(s.options.timeout_secs, Some(120));
        assert_eq!(s.options.cache_key.as_deref(), Some("abc"));
    }

    #[test]
    fn sven_comment_sets_model() {
        let md = "## Step\n<!-- sven: model=gpt-4o -->\nDo the work.";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert_eq!(s.options.model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn sven_comment_sets_model_with_provider_prefix() {
        let md = "## Step\n<!-- sven: mode=research model=anthropic/claude-opus-4-5 -->\nResearch.";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert_eq!(s.options.mode.as_deref(), Some("research"));
        assert_eq!(
            s.options.model.as_deref(),
            Some("anthropic/claude-opus-4-5")
        );
    }

    #[test]
    fn step_without_sven_comment_has_default_options() {
        let md = "## My Step\nJust do it.";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert!(s.options.mode.is_none());
        assert!(s.options.timeout_secs.is_none());
        assert!(s.options.cache_key.is_none());
    }

    #[test]
    fn old_step_comment_syntax_not_parsed_as_options() {
        // <!-- step: ... --> is no longer recognized; options stay None
        let md = "## Step\n<!-- step: mode=research -->\nContent.";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert!(
            s.options.mode.is_none(),
            "old <!-- step: --> syntax must not be parsed"
        );
        // The comment itself should still be stripped from body
        assert!(
            !s.content.contains("<!-- step:"),
            "old comment should be stripped from body"
        );
    }

    #[test]
    fn implicit_comment_format_works_without_sven_prefix() {
        // <!-- model=gpt-4o mode=agent --> should work (no sven: prefix needed)
        let md = "## Step\n<!-- model=gpt-4o mode=agent -->\nDo work.";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert_eq!(s.options.model.as_deref(), Some("gpt-4o"));
        assert_eq!(s.options.mode.as_deref(), Some("agent"));
        assert!(!s.content.contains("<!--"));
    }

    #[test]
    fn implicit_comment_with_only_model_works() {
        let md = "## Step\n<!-- model=gpt-5.2 -->\nContent.";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert_eq!(s.options.model.as_deref(), Some("gpt-5.2"));
    }

    // ── provider= key (regression: was silently discarded) ──────────────────

    #[test]
    fn sven_comment_sets_provider_and_model_separately() {
        // This is the exact pattern that triggered the bug:
        // provider= was parsed but silently dropped (missing match arm).
        let md = "## Say Hi\n<!-- sven: provider=anthropic model=claude-sonnet-4-5 -->\nHi Claude!";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert_eq!(
            s.options.provider.as_deref(),
            Some("anthropic"),
            "provider should be parsed from sven comment"
        );
        assert_eq!(
            s.options.model.as_deref(),
            Some("claude-sonnet-4-5"),
            "model should be parsed from sven comment"
        );
    }

    #[test]
    fn sven_comment_provider_only_works() {
        let md = "## Step\n<!-- sven: provider=anthropic -->\nHello.";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert_eq!(s.options.provider.as_deref(), Some("anthropic"));
        assert!(s.options.model.is_none());
    }

    #[test]
    fn implicit_comment_provider_and_model() {
        // <!-- provider=anthropic model=claude-sonnet-4-5 --> without sven: prefix
        let md = "## Step\n<!-- provider=anthropic model=claude-sonnet-4-5 -->\nHello.";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert_eq!(s.options.provider.as_deref(), Some("anthropic"));
        assert_eq!(s.options.model.as_deref(), Some("claude-sonnet-4-5"));
    }

    #[test]
    fn non_option_html_comments_are_ignored() {
        // Regular HTML comments without known keys should not be parsed as options
        let md = "## Step\n<!-- This is just a regular comment -->\nContent.";
        let mut w = parse_workflow(md);
        let s = w.steps.pop().unwrap();
        assert!(s.options.model.is_none());
        assert!(s.options.mode.is_none());
    }

    // ── Full document: H1 + preamble + steps ─────────────────────────────────

    #[test]
    fn full_workflow_document() {
        let md = concat!(
            "# Token Usage Support\n\n",
            "Investigate how token usage is tracked.\n\n",
            "## Research\n",
            "<!-- sven: mode=research model=gpt-4o -->\n",
            "Investigate codex, openclaw, and claude-code.\n\n",
            "## Implement\n",
            "<!-- sven: mode=agent -->\n",
            "Apply what you found.\n",
        );
        let mut w = parse_workflow(md);
        assert_eq!(w.title.as_deref(), Some("Token Usage Support"));
        assert!(w
            .system_prompt_append
            .as_deref()
            .map(|s| s.contains("Investigate how token usage"))
            .unwrap_or(false));
        assert_eq!(w.steps.len(), 2);
        let s1 = w.steps.pop().unwrap();
        assert_eq!(s1.label.as_deref(), Some("Research"));
        assert_eq!(s1.options.mode.as_deref(), Some("research"));
        let s2 = w.steps.pop().unwrap();
        assert_eq!(s2.label.as_deref(), Some("Implement"));
        assert_eq!(s2.options.mode.as_deref(), Some("agent"));
    }
}
