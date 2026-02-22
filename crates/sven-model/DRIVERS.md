# Model Driver API

This document describes the driver architecture in `sven-model` and explains
how to add support for a new model provider.

## Architecture overview

```
sven_config::ModelConfig
        │
        ▼
sven_model::from_config()          ← single dispatch function
        │
        ├─ "openai"    → OpenAiProvider      (wraps OpenAICompatProvider)
        ├─ "anthropic" → AnthropicProvider   (native Anthropic Messages API)
        ├─ "google"    → GoogleProvider      (native Gemini API)
        ├─ "aws"       → BedrockProvider     (native Bedrock Converse + SigV4)
        ├─ "cohere"    → CohereProvider      (native Cohere Chat v2 API)
        ├─ "azure"     → AzureCompatProvider (Azure OpenAI: different URL/auth)
        ├─ "groq"      → OpenAICompatProvider("groq", ...)
        ├─ "ollama"    → OpenAICompatProvider("ollama", ...)
        │  …30+ more providers…
        │
        ▼
    Box<dyn ModelProvider>
```

## The `ModelProvider` trait

Every driver must implement `sven_model::ModelProvider`:

```rust
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Provider id (must match the registry entry id).
    fn name(&self) -> &str;

    /// Model identifier forwarded to the API.
    fn model_name(&self) -> &str;

    /// Stream a chat completion.
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream>;

    /// List available models (default: return static catalog entries).
    async fn list_models(&self) -> anyhow::Result<Vec<ModelCatalogEntry>> {
        // default implementation — return catalog entries filtered by provider
    }
}
```

`ResponseStream` is `Pin<Box<dyn Stream<Item = anyhow::Result<ResponseEvent>> + Send>>`.

## `CompletionRequest` and `ResponseEvent`

### CompletionRequest

```rust
pub struct CompletionRequest {
    pub messages: Vec<Message>,   // full conversation history
    pub tools:    Vec<ToolSchema>, // tool definitions
    pub stream:   bool,           // true for SSE streaming
}
```

### ResponseEvent

```rust
pub enum ResponseEvent {
    TextDelta(String),          // incremental text chunk
    ThinkingDelta(String),      // thinking/reasoning chunk (e.g. Gemini 2.5)
    ToolCall { id, name, arguments }, // tool invocation
    Usage { input_tokens, output_tokens },
    Error(String),
    Done,
}
```

## Adding a new OpenAI-compatible driver

Most providers speak the standard `/v1/chat/completions` wire format.  The
easiest path:

1. **Add registry metadata** in `src/registry.rs`:

```rust
DriverMeta {
    id: "myprovider",
    name: "My Provider",
    description: "Short one-line description",
    default_api_key_env: Some("MYPROVIDER_API_KEY"),
    default_base_url: Some("https://api.myprovider.com/v1"),
    requires_api_key: true,
},
```

2. **Add a match arm** in `src/lib.rs` `from_config`:

```rust
"myprovider" => Ok(Box::new(OpenAICompatProvider::new(
    "myprovider",
    cfg.name.clone(),
    key(),
    &base_url("https://api.myprovider.com/v1"),
    resolved_max_tokens,
    cfg.temperature,
    vec![], // extra headers if needed
    AuthStyle::Bearer,
))),
```

3. **Add model catalog entries** in `models.yaml`:

```yaml
- id: my-provider-model-v1
  name: My Provider Model v1
  provider: myprovider
  context_window: 131072
  max_output_tokens: 8192
  description: My Provider flagship model
```

That's it!  The `OpenAICompatProvider` handles request serialisation, SSE
parsing, tool calls, streaming, and live model listing automatically.

## Adding a native (non-OpenAI) driver

For providers with a distinct API format (e.g. different message structure,
binary framing, or special auth):

1. Create `src/myprovider.rs` implementing `ModelProvider` directly.
2. Register in `registry.rs` and `lib.rs` as above.

See `src/google.rs` (Gemini streaming SSE), `src/aws.rs` (Bedrock Converse +
SigV4), and `src/cohere.rs` (Cohere Chat v2) for reference implementations.

## Auth styles

`OpenAICompatProvider` supports three auth modes:

| Style           | Header                         | Used by                    |
|-----------------|-------------------------------|----------------------------|
| `Bearer`        | `Authorization: Bearer <key>` | Most hosted providers      |
| `ApiKeyHeader`  | `api-key: <key>`              | Azure OpenAI               |
| `None`          | (no header)                   | Ollama, LM Studio, vLLM    |

## Extra headers

Some providers require custom headers (e.g. OpenRouter's `HTTP-Referer`).
Pass them as the `extra_headers` parameter:

```rust
vec![
    ("HTTP-Referer".into(), "https://github.com/yourorg/sven".into()),
    ("X-Title".into(), "sven".into()),
]
```

## AWS Bedrock specifics

The `BedrockProvider` reads AWS credentials from environment variables:

- `AWS_ACCESS_KEY_ID`
- `AWS_SECRET_ACCESS_KEY`
- `AWS_SESSION_TOKEN` (optional, for temporary credentials)
- `AWS_DEFAULT_REGION` or `AWS_REGION` (default: `us-east-1`)

SigV4 signing is implemented inline in `src/aws.rs` using `sha2` + `hex`
(already workspace dependencies) — no AWS SDK crate required.

The driver uses the synchronous Bedrock `POST /model/{id}/converse` endpoint
and wraps the response in a fake stream for API compatibility.

## Configuration schema

All model settings live in `sven_config::ModelConfig`:

```yaml
model:
  provider: groq                # driver id from registry
  name: llama-3.3-70b-versatile
  api_key_env: GROQ_API_KEY     # env var holding the key
  max_tokens: 8192              # optional (catalog default used otherwise)
  temperature: 0.2

  # Azure-specific
  azure_resource: myresource    # subdomain of .openai.azure.com
  azure_deployment: my-gpt4o   # deployment name
  azure_api_version: "2024-02-01"

  # AWS-specific
  aws_region: us-west-2

  # Provider-specific extras (forwarded to driver)
  driver_options:
    portkey_virtual_key: pk-live-xxx
```

## Running integration tests

Integration tests are `#[ignore]`d by default.  To run them:

```sh
# All integration tests (requires respective API keys):
cargo test -p sven-model -- --include-ignored

# Specific provider:
GROQ_API_KEY=gsk_... cargo test -p sven-model test_groq -- --include-ignored
```

Test files:
- `tests/all_drivers_mock.rs` — unit tests for every registered driver (no network)
- `tests/driver_tests.rs` — live API integration tests (ignored by default)

## Registry CLI

```sh
# List all providers with id, name, description
sven list-providers

# Verbose: also show API key env var and default URL
sven list-providers --verbose

# JSON output
sven list-providers --json

# List catalog models for a specific provider
sven list-models --provider groq

# Live query (requires API key in env)
sven list-models --provider openai --refresh
```
