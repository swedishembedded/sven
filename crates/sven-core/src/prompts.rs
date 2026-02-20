use sven_config::AgentMode;

/// Build the system prompt for the given agent mode.
pub fn system_prompt(mode: AgentMode, custom: Option<&str>) -> String {
    if let Some(custom) = custom {
        return custom.to_string();
    }

    let mode_instructions = match mode {
        AgentMode::Research => {
            "You are a research assistant.  You may read files, search the codebase, and look up \
             information.  You MUST NOT write, modify, or delete any files.  Focus on \
             gathering and summarising information accurately."
        }
        AgentMode::Plan => {
            "You are a planning assistant.  Analyse the request and produce a clear, structured \
             plan with numbered steps.  You may read files to inform the plan, but MUST NOT \
             modify them.  Output the plan in Markdown."
        }
        AgentMode::Agent => {
            "You are a capable coding agent.  You can read and write files, run shell commands, \
             and search the codebase.  Work systematically, verify your changes, and report \
             your progress clearly."
        }
    };

    format!(
        "You are Sven, an efficient AI coding agent operating in `{mode}` mode.\n\n\
         {mode_instructions}\n\n\
         ## Guidelines\n\
         - Be concise and precise in your responses.\n\
         - Use tools instead of guessing file contents.\n\
         - When writing code, follow existing project conventions.\n\
         - If a task is ambiguous, ask for clarification before acting.\n\
         - Summarise what you did at the end of each turn.",
    )
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sven_config::AgentMode;

    // ── Custom prompt override ────────────────────────────────────────────────

    #[test]
    fn custom_prompt_is_returned_verbatim() {
        let prompt = system_prompt(AgentMode::Agent, Some("Custom instructions here."));
        assert_eq!(prompt, "Custom instructions here.");
    }

    // ── Mode-specific content ─────────────────────────────────────────────────

    #[test]
    fn research_mode_mentions_read_only() {
        let p = system_prompt(AgentMode::Research, None);
        assert!(p.contains("read-only") || p.contains("MUST NOT write"),
            "Research mode should forbid writes");
    }

    #[test]
    fn plan_mode_mentions_structured_plan() {
        let p = system_prompt(AgentMode::Plan, None);
        assert!(p.to_lowercase().contains("plan"),
            "Plan mode prompt should mention 'plan'");
    }

    #[test]
    fn agent_mode_mentions_write_capability() {
        let p = system_prompt(AgentMode::Agent, None);
        assert!(p.contains("write files") || p.contains("read and write"),
            "Agent mode should mention write capability");
    }

    // ── Common structure ──────────────────────────────────────────────────────

    #[test]
    fn all_modes_name_sven() {
        for mode in [AgentMode::Research, AgentMode::Plan, AgentMode::Agent] {
            let p = system_prompt(mode, None);
            assert!(p.contains("Sven"), "prompt should identify the agent as Sven");
        }
    }

    #[test]
    fn all_modes_include_mode_name_in_prompt() {
        for (mode, expected) in [
            (AgentMode::Research, "research"),
            (AgentMode::Plan, "plan"),
            (AgentMode::Agent, "agent"),
        ] {
            let p = system_prompt(mode, None);
            assert!(p.contains(expected),
                "prompt for {mode} should contain the mode name");
        }
    }

    #[test]
    fn all_modes_include_guidelines_section() {
        for mode in [AgentMode::Research, AgentMode::Plan, AgentMode::Agent] {
            let p = system_prompt(mode, None);
            assert!(p.contains("Guidelines"), "prompt should contain a Guidelines section");
        }
    }
}
