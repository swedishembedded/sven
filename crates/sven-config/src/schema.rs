// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use serde::{Deserialize, Serialize};

/// Serde default helper — returns `true`.
///
/// Used for config fields that should be enabled unless the user explicitly
/// sets them to `false`.  `#[serde(default)]` on a `bool` always falls back
/// to `bool::default()` (i.e. `false`), so a named function is required.
fn default_true() -> bool {
    true
}

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
    /// Named provider configurations.
    ///
    /// Define custom endpoints, local models, or additional accounts here and
    /// reference them by name with `--model <key>` or `--model <key>/<model>`.
    ///
    /// ```yaml
    /// providers:
    ///   my_ollama:
    ///     provider: openai        # uses the OpenAI-compatible wire format
    ///     base_url: http://localhost:11434/v1
    ///     name: llama3.2          # default model for this provider
    ///   work_anthropic:
    ///     provider: anthropic
    ///     api_key_env: WORK_ANTHROPIC_KEY
    ///     name: claude-opus-4-5
    /// ```
    #[serde(default)]
    pub providers: std::collections::HashMap<String, ModelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Provider identifier.  Run `sven list-providers` for the full list.
    /// Common values: "openai" | "anthropic" | "google" | "azure" | "aws" |
    /// "groq" | "openrouter" | "ollama" | "mistral" | "deepseek" | "mock"
    pub provider: String,
    /// Model name forwarded to the provider API
    pub name: String,
    /// Environment variable that holds the API key (read at runtime)
    pub api_key_env: Option<String>,
    /// Explicit API key; prefer api_key_env in config files to avoid secrets
    /// in version-controlled files
    pub api_key: Option<String>,
    /// Base URL override.  Useful for local proxies, LiteLLM, or Cloudflare.
    /// For most hosted providers the correct default is auto-selected.
    pub base_url: Option<String>,
    /// Maximum tokens to request in a single completion
    pub max_tokens: Option<u32>,
    /// Sampling temperature (0.0–2.0)
    pub temperature: Option<f32>,

    // ── Azure OpenAI ─────────────────────────────────────────────────────────
    /// Azure resource name (the subdomain of `.openai.azure.com`).
    /// Required when provider = "azure" and base_url is not set.
    pub azure_resource: Option<String>,
    /// Azure deployment name.  Defaults to `model.name` when not set.
    pub azure_deployment: Option<String>,
    /// Azure REST API version string, e.g. `"2024-02-01"`.
    pub azure_api_version: Option<String>,

    // ── AWS Bedrock ───────────────────────────────────────────────────────────
    /// AWS region override (also honoured via AWS_DEFAULT_REGION env var).
    pub aws_region: Option<String>,

    // ── Prompt caching ────────────────────────────────────────────────────────
    /// Attach an explicit cache-control marker to the system message.
    ///
    /// **Anthropic**: adds `"cache_control": {"type": "ephemeral"}` to the
    /// system block, which tells the API to cache the prefix up to and
    /// including that block.  Anthropic charges a one-time write fee and
    /// subsequent calls save ~90% on cached input tokens.
    ///
    /// **Other providers**: OpenAI and Google cache automatically; this flag
    /// has no effect for those providers.
    #[serde(default = "default_true")]
    pub cache_system_prompt: bool,

    /// Use the extended (1-hour) cache TTL instead of the default 5-minute
    /// window.  Applies to the system prompt (when `cache_system_prompt = true`)
    /// and to tool definitions (when `cache_tools = true`).  Only meaningful
    /// for the Anthropic provider.  Sends the
    /// `anthropic-beta: extended-cache-ttl-2025-04-11` header automatically.
    ///
    /// Conversation caching (`cache_conversation`) always uses the 5-minute
    /// TTL regardless of this setting, because conversation turns are
    /// typically frequent enough to keep the cache refreshed within 5 minutes.
    #[serde(default)]
    pub extended_cache_time: bool,

    /// Cache tool definitions using Anthropic prompt caching.
    ///
    /// Tool definitions are stable across requests within a session, making
    /// them ideal for caching.  The last tool in the list receives a
    /// `cache_control` marker so Anthropic caches all tool definitions as a
    /// prefix.  Uses the same TTL as `extended_cache_time` controls (1-hour
    /// when true, 5-minute otherwise).
    ///
    /// With many tools (each ~200-500 tokens), this can save thousands of
    /// tokens per request.
    #[serde(default = "default_true")]
    pub cache_tools: bool,

    /// Enable automatic conversation caching (Anthropic only).
    ///
    /// Adds a top-level `cache_control` marker that instructs Anthropic to
    /// automatically cache conversation history up to the last message.
    /// Subsequent turns read prior context from cache at 10% of the base
    /// token cost, dramatically reducing cost for multi-turn agent sessions.
    ///
    /// The cache breakpoint automatically advances with each new turn so no
    /// manual management is needed.
    #[serde(default = "default_true")]
    pub cache_conversation: bool,

    /// Cache image content blocks in conversation history (Anthropic only).
    ///
    /// Images are token-expensive: even a modest screenshot costs hundreds of
    /// input tokens every turn it remains in context.  Marking the oldest image
    /// blocks with `cache_control` preserves them across turns, saving ~90% on
    /// those tokens for the rest of the session.
    ///
    /// Uses the same TTL tier as `extended_cache_time` controls.  The number
    /// of cached images is bounded by the remaining Anthropic breakpoint budget
    /// (maximum 4 breakpoints total across system, tools, conversation, and
    /// images/tool-results).
    #[serde(default = "default_true")]
    pub cache_images: bool,

    /// Cache large tool results in conversation history (Anthropic only).
    ///
    /// When an agent reads files, runs commands, or fetches documents, those
    /// tool results can consume thousands of tokens on every subsequent turn.
    /// Marking them with `cache_control` once saves ~90% on those tokens for
    /// all following turns.
    ///
    /// A result is eligible when its serialised content exceeds 4 096
    /// characters (~1 024 tokens, the Anthropic minimum cacheable length for
    /// Sonnet-class models).  The oldest eligible results are cached first;
    /// the count is bounded by the remaining breakpoint budget.
    ///
    /// Uses the same TTL tier as `extended_cache_time` controls.
    #[serde(default = "default_true")]
    pub cache_tool_results: bool,

    // ── Provider-specific extras ──────────────────────────────────────────────
    /// Free-form provider-specific options forwarded as-is to the driver.
    /// Useful for headers or parameters not covered by the standard fields.
    #[serde(default)]
    pub driver_options: serde_json::Value,

    // ── Mock provider ─────────────────────────────────────────────────────────
    /// Path to YAML mock-responses file (used when provider = "mock").
    /// Can also be set via the SVEN_MOCK_RESPONSES environment variable.
    pub mock_responses_file: Option<String>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            name: "gpt-4o".into(),
            // api_key_env is intentionally None here.  resolve_api_key() falls
            // through to the driver registry, which already knows the canonical
            // env-var name for each provider (OPENAI_API_KEY, ANTHROPIC_API_KEY,
            // etc.).  Hard-coding it here would shadow the registry lookup and
            // cause the wrong key to be sent whenever the provider is overridden
            // at the step level (e.g. <!-- sven: provider=anthropic -->).
            api_key_env: None,
            api_key: None,
            base_url: None,
            max_tokens: Some(4096),
            temperature: Some(0.2),
            azure_resource: None,
            azure_deployment: None,
            azure_api_version: None,
            aws_region: None,
            // Comprehensive caching is on by default for every provider that
            // supports it (currently Anthropic).  The flags are no-ops for
            // providers such as OpenAI that cache automatically.  Only the
            // extended (1-hour) TTL remains opt-in because it carries a 2×
            // write cost that is only worthwhile when turns are >5 min apart.
            cache_system_prompt: true,
            extended_cache_time: false,
            cache_tools: true,
            cache_conversation: true,
            cache_images: true,
            cache_tool_results: true,
            driver_options: serde_json::Value::Null,
            mock_responses_file: None,
        }
    }
}

fn default_agent_mode() -> AgentMode {
    AgentMode::Agent
}
fn default_max_tool_rounds() -> u32 {
    200
}
fn default_compaction_threshold() -> f32 {
    0.85
}

/// Strategy used when compacting the session context.
///
/// `Structured` (default) instructs the model to produce a typed Markdown
/// checkpoint with fixed sections (Active Task, Key Decisions, Files &
/// Artifacts, Constraints, Pending Items, Session Narrative).  This produces
/// checkpoints that are easier for the model to navigate on future turns.
///
/// `Narrative` uses the original free-form summarisation prompt and is
/// available for backward-compatibility or when a simpler output is preferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CompactionStrategy {
    #[default]
    Structured,
    Narrative,
}

impl std::fmt::Display for CompactionStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompactionStrategy::Structured => write!(f, "structured"),
            CompactionStrategy::Narrative => write!(f, "narrative"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Default mode when none is specified on the CLI
    #[serde(default = "default_agent_mode")]
    pub default_mode: AgentMode,
    /// Maximum number of autonomous tool-call rounds before stopping
    #[serde(default = "default_max_tool_rounds")]
    pub max_tool_rounds: u32,
    /// Token fraction at which proactive compaction triggers (0.0–1.0).
    /// The budget gate compares effective tokens (calibrated estimate + schema
    /// overhead) against the model's usable input budget, which is
    /// context_window minus max_output_tokens.
    #[serde(default = "default_compaction_threshold")]
    pub compaction_threshold: f32,
    /// Number of recent non-system messages preserved verbatim during
    /// compaction.  The oldest messages beyond this tail are summarised by
    /// the LLM.  Higher values retain more recent context but reduce the
    /// compression benefit.
    ///
    /// A value of 6 corresponds to roughly 3 back-and-forth turns
    /// (user + assistant per turn, tool results excluded from the count).
    /// Set to 0 to summarise the full history (original behaviour).
    #[serde(default = "default_compaction_keep_recent")]
    pub compaction_keep_recent: usize,
    /// Compaction checkpoint format.
    ///
    /// `structured` (default): produces a typed Markdown checkpoint with
    /// fixed sections preserving tasks, decisions, files, and constraints.
    /// `narrative`: uses the original free-form summarisation prompt.
    #[serde(default)]
    pub compaction_strategy: CompactionStrategy,
    /// Maximum tokens allowed for a single tool result before it is
    /// deterministically truncated before entering the session.
    ///
    /// Truncation is content-aware: shell output keeps head+tail lines, grep
    /// keeps leading matches, read_file keeps head+tail lines.  A value of
    /// 0 disables per-result truncation entirely.
    #[serde(default = "default_tool_result_token_cap")]
    pub tool_result_token_cap: usize,
    /// Fraction of the context window reserved for tool schemas, the dynamic
    /// context block (git/CI info), and measurement error in the token
    /// approximation.  Reduces the effective compaction trigger threshold.
    ///
    /// Example: threshold=0.85, reserve=0.10 → compaction fires when
    /// calibrated session tokens reach 75% of the input budget.
    #[serde(default = "default_compaction_overhead_reserve")]
    pub compaction_overhead_reserve: f32,
    /// System prompt override; leave None to use the built-in prompt
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// Per-step wall-clock timeout in seconds (0 = no limit).
    /// Can be set in config, overridden by frontmatter or CLI flag.
    #[serde(default)]
    pub max_step_timeout_secs: u64,

    /// Total run wall-clock timeout in seconds (0 = no limit).
    #[serde(default)]
    pub max_run_timeout_secs: u64,
}

fn default_compaction_keep_recent() -> usize {
    6
}
fn default_tool_result_token_cap() -> usize {
    4000
}
fn default_compaction_overhead_reserve() -> f32 {
    0.10
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            default_mode: AgentMode::Agent,
            max_tool_rounds: 200,
            compaction_threshold: 0.85,
            compaction_keep_recent: default_compaction_keep_recent(),
            compaction_strategy: CompactionStrategy::Structured,
            tool_result_token_cap: default_tool_result_token_cap(),
            compaction_overhead_reserve: default_compaction_overhead_reserve(),
            system_prompt: None,
            max_step_timeout_secs: 0,
            max_run_timeout_secs: 0,
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
    /// GDB debugging configuration
    #[serde(default)]
    pub gdb: GdbConfig,
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
            deny_patterns: vec!["rm -rf /*".into(), "dd if=*".into()],
            timeout_secs: 30,
            use_docker: false,
            docker_image: None,
            web: WebConfig::default(),
            memory: MemoryConfig::default(),
            lints: LintsConfig::default(),
            gdb: GdbConfig::default(),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdbConfig {
    /// Path to gdb-multiarch (or gdb) executable
    #[serde(default = "GdbConfig::default_gdb_path")]
    pub gdb_path: String,
    /// Default timeout for GDB commands in seconds
    #[serde(default = "GdbConfig::default_command_timeout_secs")]
    pub command_timeout_secs: u64,
    /// Timeout for the initial gdb_connect handshake in seconds.
    /// This covers symbol loading (which can take 15-30s for large ELFs)
    /// plus the TCP connection + GDB/MI startup.  Must be >= command_timeout_secs.
    #[serde(default = "GdbConfig::default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    /// Milliseconds to wait after spawning the GDB server before connecting
    #[serde(default = "GdbConfig::default_server_startup_wait_ms")]
    pub server_startup_wait_ms: u64,
}

impl GdbConfig {
    fn default_gdb_path() -> String {
        "gdb-multiarch".into()
    }
    fn default_command_timeout_secs() -> u64 {
        10
    }
    fn default_connect_timeout_secs() -> u64 {
        30
    }
    fn default_server_startup_wait_ms() -> u64 {
        500
    }
}

impl Default for GdbConfig {
    fn default() -> Self {
        Self {
            gdb_path: Self::default_gdb_path(),
            command_timeout_secs: Self::default_command_timeout_secs(),
            connect_timeout_secs: Self::default_connect_timeout_secs(),
            server_startup_wait_ms: Self::default_server_startup_wait_ms(),
        }
    }
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
    fn config_default_api_key_env_is_none() {
        // api_key_env must be None in the default config so that resolve_api_key()
        // falls through to the driver registry.  A hard-coded value here would
        // shadow the registry and send the wrong key on per-step provider overrides.
        let c = Config::default();
        assert!(c.model.api_key_env.is_none());
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
    fn config_default_compaction_keep_recent_is_six() {
        let c = Config::default();
        assert_eq!(c.agent.compaction_keep_recent, 6);
    }

    #[test]
    fn config_compaction_keep_recent_yaml_round_trip() {
        let yaml_str = "agent:\n  compaction_keep_recent: 10\n";
        let c: Config = serde_yaml::from_str(yaml_str).unwrap();
        assert_eq!(c.agent.compaction_keep_recent, 10);
        // Round-trip
        let back_yaml = serde_yaml::to_string(&c).unwrap();
        let back: Config = serde_yaml::from_str(&back_yaml).unwrap();
        assert_eq!(back.agent.compaction_keep_recent, 10);
    }

    #[test]
    fn config_compaction_keep_recent_defaults_when_absent_from_yaml() {
        // A YAML with an agent section but no compaction_keep_recent uses serde default.
        let yaml_str =
            "agent:\n  max_tool_rounds: 30\n  default_mode: agent\n  compaction_threshold: 0.9\n";
        let c: Config = serde_yaml::from_str(yaml_str).unwrap();
        assert_eq!(
            c.agent.compaction_keep_recent, 6,
            "serde default must fill in missing field"
        );
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

    // ── Prompt caching defaults ───────────────────────────────────────────────

    #[test]
    fn config_default_caching_enabled_except_extended_ttl() {
        // All caching flags default to true — sven caches comprehensively
        // out-of-the-box for every provider that supports explicit caching.
        // extended_cache_time stays false: the 1-hour TTL has a 2× write cost
        // and is only worthwhile when turns are more than 5 minutes apart.
        let c = Config::default();
        assert!(
            c.model.cache_system_prompt,
            "cache_system_prompt must default to true"
        );
        assert!(c.model.cache_tools, "cache_tools must default to true");
        assert!(
            c.model.cache_conversation,
            "cache_conversation must default to true"
        );
        assert!(c.model.cache_images, "cache_images must default to true");
        assert!(
            c.model.cache_tool_results,
            "cache_tool_results must default to true"
        );
        assert!(
            !c.model.extended_cache_time,
            "extended_cache_time must remain false by default"
        );
    }

    #[test]
    fn config_cache_flags_can_be_disabled_via_yaml() {
        // Users may opt out of individual cache layers.
        let yaml_str = "model:\n  provider: anthropic\n  name: claude-sonnet-4-5\n  \
                        cache_system_prompt: false\n  cache_tools: false\n  \
                        cache_conversation: false\n  cache_images: false\n  \
                        cache_tool_results: false\n";
        let c: Config = serde_yaml::from_str(yaml_str).unwrap();
        assert!(!c.model.cache_system_prompt);
        assert!(!c.model.cache_tools);
        assert!(!c.model.cache_conversation);
        assert!(!c.model.cache_images);
        assert!(!c.model.cache_tool_results);
    }

    #[test]
    fn config_extended_cache_time_can_be_enabled_via_yaml() {
        let yaml_str = "model:\n  provider: anthropic\n  name: claude-sonnet-4-5\n  \
                        extended_cache_time: true\n";
        let c: Config = serde_yaml::from_str(yaml_str).unwrap();
        assert!(c.model.extended_cache_time);
    }

    #[test]
    fn config_cache_flags_omitted_yaml_uses_defaults() {
        // When not specified in YAML the flags must use the struct defaults
        // (true for caching flags, false for extended TTL).
        let yaml_str = "model:\n  provider: anthropic\n  name: claude-sonnet-4-5\n";
        let c: Config = serde_yaml::from_str(yaml_str).unwrap();
        assert!(
            c.model.cache_system_prompt,
            "cache_system_prompt must default to true"
        );
        assert!(c.model.cache_tools, "cache_tools must default to true");
        assert!(
            c.model.cache_conversation,
            "cache_conversation must default to true"
        );
        assert!(
            !c.model.extended_cache_time,
            "extended_cache_time must default to false"
        );
        assert!(c.model.cache_images, "cache_images must default to true");
        assert!(
            c.model.cache_tool_results,
            "cache_tool_results must default to true"
        );
    }

    #[test]
    fn config_cache_flags_round_trip_yaml() {
        let mut c = Config::default();
        c.model.provider = "anthropic".into();
        // Flip all flags to the non-default values to verify round-trip fidelity.
        c.model.cache_tools = false;
        c.model.cache_conversation = false;
        c.model.cache_images = false;
        c.model.cache_tool_results = false;
        c.model.extended_cache_time = true;
        let yaml = serde_yaml::to_string(&c).unwrap();
        let back: Config = serde_yaml::from_str(&yaml).unwrap();
        assert!(!back.model.cache_tools);
        assert!(!back.model.cache_conversation);
        assert!(!back.model.cache_images);
        assert!(!back.model.cache_tool_results);
        assert!(back.model.extended_cache_time);
    }

    // ── YAML round-trip ───────────────────────────────────────────────────────

    #[test]
    fn config_serialises_to_valid_yaml() {
        let c = Config::default();
        let yaml_str = serde_yaml::to_string(&c).unwrap();
        assert!(yaml_str.contains("provider"));
        assert!(yaml_str.contains("openai"));
    }

    #[test]
    fn config_deserialises_from_yaml() {
        let yaml_str =
            "model:\n  provider: anthropic\n  name: claude-opus-4-5\n  max_tokens: 8192\n";
        let c: Config = serde_yaml::from_str(yaml_str).unwrap();
        assert_eq!(c.model.provider, "anthropic");
        assert_eq!(c.model.name, "claude-opus-4-5");
        assert_eq!(c.model.max_tokens, Some(8192));
    }

    #[test]
    fn config_partial_yaml_fills_in_defaults() {
        let yaml_str = "model:\n  name: gpt-4o-mini\n  provider: openai\n";
        let c: Config = serde_yaml::from_str(yaml_str).unwrap();
        assert_eq!(c.model.name, "gpt-4o-mini");
        assert_eq!(
            c.agent.max_tool_rounds,
            AgentConfig::default().max_tool_rounds
        );
    }

    #[test]
    fn agent_mode_yaml_serde_roundtrip() {
        #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
        struct Wrap {
            mode: AgentMode,
        }
        let w = Wrap {
            mode: AgentMode::Plan,
        };
        let s = serde_yaml::to_string(&w).unwrap();
        let back: Wrap = serde_yaml::from_str(&s).unwrap();
        assert_eq!(back.mode, AgentMode::Plan);
    }

    // ── providers map ─────────────────────────────────────────────────────────

    #[test]
    fn config_default_providers_is_empty() {
        let c = Config::default();
        assert!(c.providers.is_empty(), "providers must be empty by default");
    }

    #[test]
    fn config_providers_deserialised_from_yaml() {
        let yaml = r#"
providers:
  my_ollama:
    provider: openai
    base_url: http://localhost:11434/v1
    name: llama3.2
"#;
        let c: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(c.providers.len(), 1);
        let p = c.providers.get("my_ollama").unwrap();
        assert_eq!(p.provider, "openai");
        assert_eq!(p.base_url.as_deref(), Some("http://localhost:11434/v1"));
        assert_eq!(p.name, "llama3.2");
    }

    #[test]
    fn config_providers_round_trip_yaml() {
        let yaml = r#"
providers:
  local:
    provider: openai
    base_url: http://127.0.0.1:8080/v1
    name: phi-3
"#;
        let c: Config = serde_yaml::from_str(yaml).unwrap();
        let serialised = serde_yaml::to_string(&c).unwrap();
        let back: Config = serde_yaml::from_str(&serialised).unwrap();
        let p = back.providers.get("local").unwrap();
        assert_eq!(p.name, "phi-3");
        assert_eq!(p.base_url.as_deref(), Some("http://127.0.0.1:8080/v1"));
    }

    #[test]
    fn config_providers_absent_in_yaml_uses_empty_default() {
        let yaml = "model:\n  provider: openai\n  name: gpt-4o\n";
        let c: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(c.providers.is_empty());
    }
}
