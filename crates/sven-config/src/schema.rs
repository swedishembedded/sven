use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub tui: TuiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Provider identifier: "openai" | "anthropic" | "mock"
    pub provider: String,
    /// Model name forwarded to the provider
    pub name: String,
    /// Environment variable that holds the API key (read at runtime)
    pub api_key_env: Option<String>,
    /// Explicit API key; prefer api_key_env in config files
    pub api_key: Option<String>,
    /// Base URL override (useful for local proxies / LiteLLM)
    pub base_url: Option<String>,
    /// Maximum tokens to request in a single completion
    pub max_tokens: Option<u32>,
    /// Temperature (0.0–2.0)
    pub temperature: Option<f32>,
    /// Path to YAML mock-responses file (used when provider = "mock").
    /// Can also be set via the SVEN_MOCK_RESPONSES environment variable.
    pub mock_responses_file: Option<String>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            name: "gpt-4o".into(),
            api_key_env: Some("OPENAI_API_KEY".into()),
            api_key: None,
            base_url: None,
            max_tokens: Some(4096),
            temperature: Some(0.2),
            mock_responses_file: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Default mode when none is specified on the CLI
    pub default_mode: AgentMode,
    /// Maximum number of autonomous tool-call rounds before stopping
    pub max_tool_rounds: u32,
    /// Token fraction at which proactive compaction triggers (0.0–1.0)
    pub compaction_threshold: f32,
    /// System prompt override; leave None to use the built-in prompt
    pub system_prompt: Option<String>,

    /// Per-step wall-clock timeout in seconds (0 = no limit).
    /// Can be set in config, overridden by frontmatter or CLI flag.
    #[serde(default)]
    pub max_step_timeout_secs: u64,

    /// Total run wall-clock timeout in seconds (0 = no limit).
    #[serde(default)]
    pub max_run_timeout_secs: u64,

    // ── Runtime-only fields (never persisted to TOML) ─────────────────────────

    /// Absolute path to the project root (found by walking up to .git).
    /// Set at runtime in headless mode; not read from / written to config.
    #[serde(skip)]
    pub project_root: Option<PathBuf>,

    /// Pre-formatted CI environment context block appended to the system
    /// prompt.  Set at runtime when a known CI environment is detected.
    #[serde(skip)]
    pub ci_context_note: Option<String>,

    /// Pre-formatted git context block (branch, commit, dirty status).
    /// Set at runtime by collecting live git metadata.
    #[serde(skip)]
    pub git_context_note: Option<String>,

    /// Contents of the project context file (e.g. `.sven/context.md`,
    /// `AGENTS.md`, or `CLAUDE.md`) injected into the system prompt.
    /// Set at runtime; not persisted to TOML.
    #[serde(skip)]
    pub project_context_file: Option<String>,

    /// Text appended to the default system prompt.
    /// Set at runtime from `--append-system-prompt`.  Ignored when
    /// `system_prompt` is set (the custom prompt is used verbatim).
    #[serde(skip)]
    pub append_system_prompt: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            default_mode: AgentMode::Agent,
            max_tool_rounds: 50,
            compaction_threshold: 0.85,
            system_prompt: None,
            max_step_timeout_secs: 0,
            max_run_timeout_secs: 0,
            project_root: None,
            ci_context_note: None,
            git_context_note: None,
            project_context_file: None,
            append_system_prompt: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    /// Pure research – read-only tools, no writes
    Research,
    /// Generate a structured plan, no code changes
    Plan,
    /// Full agent with read/write tools
    Agent,
}

impl std::fmt::Display for AgentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentMode::Research => write!(f, "research"),
            AgentMode::Plan => write!(f, "plan"),
            AgentMode::Agent => write!(f, "agent"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsConfig {
    /// Automatically approve shell commands matching these glob patterns
    pub auto_approve_patterns: Vec<String>,
    /// Block shell commands matching these glob patterns
    pub deny_patterns: Vec<String>,
    /// Timeout in seconds for a single tool call
    pub timeout_secs: u64,
    /// Use Docker sandbox for shell execution
    pub use_docker: bool,
    /// Docker image to use when use_docker is true
    pub docker_image: Option<String>,
    /// Web fetch and search configuration
    #[serde(default)]
    pub web: WebConfig,
    /// Persistent memory configuration
    #[serde(default)]
    pub memory: MemoryConfig,
    /// Linter configuration
    #[serde(default)]
    pub lints: LintsConfig,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            auto_approve_patterns: vec![
                "cat *".into(),
                "ls *".into(),
                "find *".into(),
                "rg *".into(),
                "grep *".into(),
            ],
            deny_patterns: vec![
                "rm -rf /*".into(),
                "dd if=*".into(),
            ],
            timeout_secs: 30,
            use_docker: false,
            docker_image: None,
            web: WebConfig::default(),
            memory: MemoryConfig::default(),
            lints: LintsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebSearchConfig {
    /// Brave Search API key (also checked via BRAVE_API_KEY env var)
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    /// Search backend configuration
    #[serde(default)]
    pub search: WebSearchConfig,
    /// Default maximum characters for web_fetch (default 50000)
    pub fetch_max_chars: usize,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            search: WebSearchConfig::default(),
            fetch_max_chars: 50_000,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Path to the memory JSON file (default: ~/.config/sven/memory.json)
    pub memory_file: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LintsConfig {
    /// Override the lint command for Rust projects
    pub rust_command: Option<String>,
    /// Override the lint command for TypeScript/JS projects
    pub typescript_command: Option<String>,
    /// Override the lint command for Python projects
    pub python_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuiConfig {
    /// Colour theme: "dark" | "light" | "solarized"
    pub theme: String,
    /// Show line numbers in code blocks
    pub code_line_numbers: bool,
    /// Width used for markdown wrapping (0 = auto)
    pub wrap_width: u16,
    /// Use plain ASCII borders/indicators instead of Unicode box-drawing and
    /// Braille characters.  Enable this when the terminal font lacks wide
    /// Unicode support (the font renders replacement glyphs / "gibberish").
    /// Can also be forced with the SVEN_ASCII_BORDERS=1 environment variable.
    #[serde(default)]
    pub ascii_borders: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            code_line_numbers: false,
            wrap_width: 0,
            ascii_borders: false,
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Defaults ─────────────────────────────────────────────────────────────

    #[test]
    fn config_default_model_provider_is_openai() {
        let c = Config::default();
        assert_eq!(c.model.provider, "openai");
    }

    #[test]
    fn config_default_model_name_is_gpt4o() {
        let c = Config::default();
        assert_eq!(c.model.name, "gpt-4o");
    }

    #[test]
    fn config_default_api_key_env_is_set() {
        let c = Config::default();
        assert_eq!(c.model.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
    }

    #[test]
    fn config_default_no_explicit_api_key() {
        let c = Config::default();
        assert!(c.model.api_key.is_none());
    }

    #[test]
    fn config_default_agent_mode_is_agent() {
        let c = Config::default();
        assert_eq!(c.agent.default_mode, AgentMode::Agent);
    }

    #[test]
    fn config_default_max_tool_rounds_positive() {
        let c = Config::default();
        assert!(c.agent.max_tool_rounds > 0);
    }

    #[test]
    fn config_default_compaction_threshold_in_range() {
        let c = Config::default();
        assert!(c.agent.compaction_threshold > 0.0);
        assert!(c.agent.compaction_threshold < 1.0);
    }

    #[test]
    fn config_default_no_system_prompt_override() {
        let c = Config::default();
        assert!(c.agent.system_prompt.is_none());
    }

    #[test]
    fn config_default_tui_theme_is_dark() {
        let c = Config::default();
        assert_eq!(c.tui.theme, "dark");
    }

    #[test]
    fn config_default_tools_has_auto_approve_patterns() {
        let c = Config::default();
        assert!(!c.tools.auto_approve_patterns.is_empty());
    }

    #[test]
    fn config_default_docker_disabled() {
        let c = Config::default();
        assert!(!c.tools.use_docker);
    }

    // ── AgentMode ─────────────────────────────────────────────────────────────

    #[test]
    fn agent_mode_display_research() {
        assert_eq!(AgentMode::Research.to_string(), "research");
    }

    #[test]
    fn agent_mode_display_plan() {
        assert_eq!(AgentMode::Plan.to_string(), "plan");
    }

    #[test]
    fn agent_mode_display_agent() {
        assert_eq!(AgentMode::Agent.to_string(), "agent");
    }

    #[test]
    fn agent_mode_equality() {
        assert_eq!(AgentMode::Agent, AgentMode::Agent);
        assert_ne!(AgentMode::Research, AgentMode::Plan);
    }

    // ── TOML round-trip ───────────────────────────────────────────────────────

    #[test]
    fn config_serialises_to_valid_toml() {
        let c = Config::default();
        let toml_str = toml::to_string(&c).unwrap();
        assert!(toml_str.contains("provider"));
        assert!(toml_str.contains("openai"));
    }

    #[test]
    fn config_deserialises_from_toml() {
        let toml_str = r#"
[model]
provider = "anthropic"
name = "claude-opus-4-5"
max_tokens = 8192
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(c.model.provider, "anthropic");
        assert_eq!(c.model.name, "claude-opus-4-5");
        assert_eq!(c.model.max_tokens, Some(8192));
    }

    #[test]
    fn config_partial_toml_fills_in_defaults() {
        // Only override one field; all others should get defaults
        let toml_str = r#"
[model]
name = "gpt-4o-mini"
provider = "openai"
"#;
        let c: Config = toml::from_str(toml_str).unwrap();
        // Explicitly set field is preserved
        assert_eq!(c.model.name, "gpt-4o-mini");
        // Unset fields fall back to defaults via serde
        assert_eq!(c.agent.max_tool_rounds, AgentConfig::default().max_tool_rounds);
    }

    #[test]
    fn agent_mode_toml_serde_roundtrip() {
        #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
        struct Wrap { mode: AgentMode }
        let w = Wrap { mode: AgentMode::Plan };
        let s = toml::to_string(&w).unwrap();
        let back: Wrap = toml::from_str(&s).unwrap();
        assert_eq!(back.mode, AgentMode::Plan);
    }
}
