# Model Driver API

This document describes the driver architecture in `sven-model-drivers` and
explains how to add support for a new model provider. (`sven-model` itself
is just the `ModelProvider` trait + request/response types + the pure
`ModelResolver`/`resolve_model_cfg` config-resolution logic - no reqwest, no
per-provider dependency closure. See that crate's `lib.rs` for how the two
fit together, and `sven-model-catalog`/`sven-model-mock` for the cache of
served models and the mock providers.)

## Architecture overview

```
sven_model_drivers::ModelConfig
        │
        ▼
sven_model_drivers::from_config()  ← single dispatch function
        │
        ├─ "anthropic" → AnthropicProvider   (native Anthropic Messages API)
        ├─ "google"    → GoogleProvider      (native Gemini API)
        ├─ "aws"       → BedrockProvider     (native Bedrock Converse + SigV4)
        ├─ "cohere"    → CohereProvider      (native Cohere Chat v2 API)
        ├─ "azure"     → AzureCompatProvider (Azure OpenAI: different URL/auth)
        ├─ every other registered driver (openai, groq, ollama, vllm, litellm,
        │  openrouter, portkey, cloudflare, sven, brain, ...)
        │                → OpenAICompatProvider, configured by its registry entry
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

    /// List the models the endpoint serves (default: what it last served,
    /// from the catalog cache). A driver that can ask overrides this, records
    /// the answer with `listing::record` and returns an error when it fails.
    async fn list_models(&self) -> anyhow::Result<Vec<ModelCatalogEntry>> {
        // default implementation - cached entries filtered by provider
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
    default_base_url: Some("https://api.myprovider.com/v1"),
    requires_api_key: true,
},
```

   No code is needed for a plain OpenAI-protocol endpoint. Leave
   `default_base_url` as `None` when the user must supply the URL, set
   `local: true` for a server on the user's own hardware, and fill `requests`
   (`RequestExtras`) when the gateway needs fixed headers, a header taken from
   `driver_options`, or `driver_options` rewritten into body fields. The
   catch-all arm in `from_config` builds `OpenAICompatProvider::for_driver`
   from that entry, so discovery, modalities, context window, tool support,
   reasoning resend and request recording behave the same for every driver.

3. **Models need no entries.** An OpenAI-compatible `/models` list is read as
   it stands, including `context_length`, `top_provider.max_completion_tokens`,
   `architecture.input_modalities` and `pricing`; see
   `docs/reference/providers.md#served-models`.

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

`OpenAICompatProvider` supports two auth styles; a request carries no
credential when no key resolved (Ollama, LM Studio, vLLM):

| Style           | Header                         | Used by                    |
|-----------------|-------------------------------|----------------------------|
| `Bearer`        | `Authorization: Bearer <key>` | Most hosted providers      |
| `ApiKeyHeader`  | `api-key: <key>`              | Azure OpenAI               |

## Extra headers

Some gateways require custom headers (OpenRouter's `HTTP-Referer`, Portkey's
virtual key). They are data in the driver's `requests` entry in
`crates/model/src/registry.rs`, never code in the client.

## AWS Bedrock specifics

The `BedrockProvider` reads AWS credentials from environment variables:

- `AWS_ACCESS_KEY_ID`
- `AWS_SECRET_ACCESS_KEY`
- `AWS_SESSION_TOKEN` (optional, for temporary credentials)
- `AWS_DEFAULT_REGION` or `AWS_REGION` (default: `us-east-1`)

SigV4 signing is implemented inline in `src/aws.rs` using `sha2` + `hex`
(already workspace dependencies) - no AWS SDK crate required.

The driver uses the synchronous Bedrock `POST /model/{id}/converse` endpoint
and wraps the response in a fake stream for API compatibility.

## Configuration schema

All model settings live in `sven_model_drivers::ModelConfig`:

```yaml
model:
  provider: groq                # driver id from registry
  name: llama-3.3-70b-versatile
  # key: `sven auth set groq` (or api_key_file: FILE beside a custom base_url)
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
# All integration tests (each needs its provider's key, stored once with
# `sven auth set <provider>`):
cargo test -p sven-model-drivers -- --include-ignored

# Specific provider:
sven auth set groq
cargo test -p sven-model-drivers test_groq -- --include-ignored
```

Test files:
- `tests/all_drivers_mock.rs` - unit tests for every registered driver (no network)
- `tests/driver_tests.rs` - live API integration tests (ignored by default)

## Registry CLI

```sh
# List all providers with id, name, description
sven list-providers

# Verbose: also show whether a key is stored, and the default URL
sven list-providers --verbose

# JSON output
sven list-providers --json

# The models a provider serves now (live; needs its stored key)
sven list-models --provider groq
```
