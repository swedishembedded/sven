# Model Providers

Sven registers 36 provider ids. Five are native drivers (`openai`,
`anthropic`, `google`, `aws`, `cohere`); `dbus` is a native transport to a
local brain server; `mock` is an offline test provider; every other id speaks
the OpenAI chat-completions dialect through one shared driver and differs only
in its default URL and key variable. `sven list-providers` prints the
registry.

## Quick start

Set the provider and model in your config file (`~/.config/sven/config.yaml`):

```yaml
model:
  provider: groq
  name: llama-3.3-70b-versatile
  api_key_env: GROQ_API_KEY
```

Or override on the command line (`--model`/`-M`, also read from `SVEN_MODEL`):

```sh
sven -M groq/llama-3.3-70b-versatile "Write a hello world in Rust"
sven -M anthropic/claude-sonnet-4-6 "Explain async Rust"
sven -M ollama/llama3.2 "What is the capital of France?"
```

A key is looked up in this order: `api_key`, the variable named by
`api_key_env` (and nowhere else), the provider's default variable (listed
below), and, for `brain`, brain's keys file. A provider that needs a key and
finds none fails at startup with the variable to set.

To list all providers:

```sh
sven list-providers
sven list-providers --verbose   # adds API key env var and default URL
sven list-providers --json
```

To list models for a provider (the bundled catalog by default):

```sh
sven list-models --provider groq
sven list-models --provider openai --refresh   # query the provider's API
sven list-models --json
```

To check that the configured model can be used before starting a session:

```sh
sven doctor provider          # exits non-zero when it cannot
sven doctor provider --json
```

The check builds the provider, and for a server on this host or a private
network also confirms that the endpoint answers, serves the model, and
advertises tool calling. Hosted providers are never probed.

## Selecting a model

`--model` (also used for a sub-agent's model and mid-session model switches)
takes, in order of precedence:

1. `<name>[/model]` where `<name>` is a key of the `providers:` section: that
   entry's endpoint, key and per-model overrides. With no `/model` part the
   current model name is kept.
2. `provider/model` where `provider` is a registered id and the pair is in the
   bundled catalog. Credentials of the active model are kept only when the
   provider is the same.
3. A bare model name found in the catalog: the provider is inferred.
4. Otherwise `provider/model`, a bare provider id (keeps the model name) or a
   bare model name (keeps the provider) applied to the active configuration.
   Switching provider clears the old `base_url`, key, token limits,
   `driver_options` and Azure/AWS fields.

## Default provider auto-detection

When **no model is configured at all** (no `model:` block, no `--model`,
no `SVEN_MODEL`), sven picks a provider from the environment, in priority
order:

1. **`brain`**: a locally running [brain](#brain-local-edge-ai-server) model
   server, detected without network I/O by `BRAIN_API_KEY`, `BRAIN_API_KEYS_FILE`
   or brain's keys file (`$BRAIN_API_KEYS_FILE`, else
   `$XDG_RUNTIME_DIR/brain/api-keys.json`, else
   `<state dir>/brain/api-keys.json`). The model name is left empty and
   resolved by asking brain's `/v1/models` (the only model, else the first
   one that reports a context length). When brain does not answer or serves no
   model, and a key for 2-4 is set, that cloud provider is used instead; with
   no cloud key sven reports that brain is unreachable.
2. **OpenRouter** (`openrouter/auto`) when `OPENROUTER_API_KEY` is set.
3. **Anthropic** (`claude-sonnet-4-6`) when `ANTHROPIC_API_KEY` is set.
4. **OpenAI** (`gpt-5.2`) when `OPENAI_API_KEY` is set.
5. **A local server** when none of the above applies: `brain` (8788),
   `ollama` (11434), `vllm` (8000) and `lmstudio` (1234) are probed on
   localhost concurrently for one second, and the first in that order that
   serves a model is used. See
   [Local model discovery](configuration.md#local-model-discovery).

Setting `SVEN_DISABLE_BRAIN_AUTODETECT` (to any value) removes brain from
steps 1 and 5, for example on a CI host with a stale keys file. Any explicit
model configuration wins over auto-detection.

---

## Major Cloud Providers

### OpenAI

| Setting    | Value                        |
|------------|------------------------------|
| Provider id | `openai`                    |
| API key env | `OPENAI_API_KEY`            |
| Default URL | `https://api.openai.com/v1` |

```yaml
model:
  provider: openai
  name: gpt-4o
  api_key_env: OPENAI_API_KEY
```

Catalog models include `gpt-5.2`, `gpt-5.1`, `gpt-5-mini`, `gpt-4o`, `gpt-4.1`, `o3`, `o4-mini`.

---

### Anthropic

| Setting    | Value                         |
|------------|-------------------------------|
| Provider id | `anthropic`                  |
| API key env | `ANTHROPIC_API_KEY`          |
| Default URL | `https://api.anthropic.com`  |

```yaml
model:
  provider: anthropic
  name: claude-sonnet-4-6
  api_key_env: ANTHROPIC_API_KEY
```

Catalog models: `claude-opus-4-6`, `claude-sonnet-4-6`, `claude-sonnet-4-5`,
`claude-haiku-4-5`.

Prompt caching is on by default for Anthropic (`cache_system_prompt`,
`cache_tools`, `cache_conversation`, `cache_images`, `cache_tool_results`;
`extended_cache_time` opts in to the 1-hour TTL). See
[Configuration](configuration.md#model).

---

### Google Gemini

| Setting    | Value                                               |
|------------|-----------------------------------------------------|
| Provider id | `google`                                           |
| API key env | `GEMINI_API_KEY`                                  |
| Default URL | `https://generativelanguage.googleapis.com`       |

```yaml
model:
  provider: google
  name: gemini-2.5-flash
  api_key_env: GEMINI_API_KEY
```

Catalog models: `gemini-2.5-pro`, `gemini-2.5-flash`, `gemini-2.0-flash`,
`gemini-1.5-pro-002`, `gemini-1.5-flash-002`.

Get a free API key at [aistudio.google.com](https://aistudio.google.com).

---

### Azure OpenAI

| Setting    | Value                    |
|------------|--------------------------|
| Provider id | `azure`                 |
| API key env | `AZURE_OPENAI_API_KEY` (sent as the `api-key` header) |
| Default URL | none: set `azure_resource` or `base_url` |

```yaml
model:
  provider: azure
  name: gpt-4o
  api_key_env: AZURE_OPENAI_API_KEY
  azure_resource: myresource       # subdomain of .openai.azure.com
  azure_deployment: my-gpt4o-dep  # optional, defaults to model name
  azure_api_version: "2024-02-01" # optional, defaults to 2024-02-01
```

Or set `base_url` to the deployment URL (sven appends
`/chat/completions?api-version=...`):

```yaml
model:
  provider: azure
  name: gpt-4o
  base_url: https://myresource.openai.azure.com/openai/deployments/my-gpt4o-dep
  azure_api_version: "2024-02-01"
```

---

### AWS Bedrock

| Setting    | Value                                                    |
|------------|----------------------------------------------------------|
| Provider id | `aws`                                                   |
| Auth       | SigV4 from AWS credentials in the environment (no API key) |

```yaml
model:
  provider: aws
  name: us.anthropic.claude-sonnet-4-6
  aws_region: us-east-1  # optional
```

The region is `aws_region`, else `AWS_DEFAULT_REGION`, else `AWS_REGION`, else
`us-east-1`. The Converse API is used.

Set credentials in environment:
```sh
export AWS_ACCESS_KEY_ID=AKIA...
export AWS_SECRET_ACCESS_KEY=...
export AWS_SESSION_TOKEN=...      # only for temporary credentials
```

Catalog models:
- `us.anthropic.claude-sonnet-4-6`, `anthropic.claude-haiku-4-5-20251001-v1:0`
- `amazon.nova-pro-v1:0`, `amazon.nova-lite-v1:0`
- `us.meta.llama3-3-70b-instruct-v1:0`

---

### Cohere

| Setting    | Value                       |
|------------|-----------------------------|
| Provider id | `cohere`                   |
| API key env | `COHERE_API_KEY`           |
| Default URL | `https://api.cohere.com`  |

```yaml
model:
  provider: cohere
  name: command-r-plus-08-2024
  api_key_env: COHERE_API_KEY
```

Catalog models: `command-r-plus-08-2024`, `command-r`, `command-nightly`.

---

## Gateways

### OpenRouter

Access 200+ models from many providers through a single API key.

| Setting    | Value                              |
|------------|------------------------------------|
| Provider id | `openrouter`                      |
| API key env | `OPENROUTER_API_KEY`             |
| Default URL | `https://openrouter.ai/api/v1`   |

```yaml
model:
  provider: openrouter
  name: anthropic/claude-sonnet-4-6
  api_key_env: OPENROUTER_API_KEY
```

The default model, used when nothing else is configured, is
`openrouter/auto`. Sven sends `HTTP-Referer` and `X-Title: sven` headers, and
the session id as `prompt_cache_key`. The model list is cached on disk and
refreshed in the background.

`driver_options.auto_router_allowed_models` restricts the auto router:

```yaml
model:
  provider: openrouter
  name: openrouter/auto
  driver_options:
    auto_router_allowed_models: ["anthropic/*", "openai/gpt-5.1"]
```

---

### LiteLLM

Proxy gateway to 100+ providers using OpenAI-compatible format.

| Setting    | Value           |
|------------|-----------------|
| Provider id | `litellm`      |
| API key env | `LITELLM_API_KEY` (a key must be set, via the variable or `api_key`) |
| Requires   | `base_url` set  |

```yaml
model:
  provider: litellm
  name: gpt-4o
  base_url: http://localhost:4000  # your LiteLLM proxy URL
```

---

### Vercel AI Gateway

| Setting    | Value                              |
|------------|------------------------------------|
| Provider id | `vercel`                          |
| API key env | `VERCEL_API_KEY`                  |
| Default URL | `https://sdk.vercel.ai/openai`    |

---

### Cloudflare AI Gateway

| Setting    | Value                                            |
|------------|--------------------------------------------------|
| Provider id | `cloudflare`                                    |
| API key env | `CLOUDFLARE_API_TOKEN`                          |
| Requires   | `base_url` set (your account-specific gateway URL) |

---

### Any OpenAI-compatible hosted endpoint

A metered or self-run service that speaks the OpenAI chat-completions dialect
works through the `openai` provider with a `base_url` and a key read from the
environment. No model-specific configuration is needed:

```yaml
model:
  provider: openai
  name: <model id as listed by the endpoint's /v1/models>
  base_url: https://api.example.com/v1
  api_key_env: EXAMPLE_API_KEY
```

What sven relies on from the endpoint:

- `POST {base_url}/chat/completions` with `stream: true`: SSE `data:` frames
  ending in a finish reason or `data: [DONE]`. Tool calls may arrive as
  several calls in one `delta.tool_calls` array and interleaved across frames
  by `index`. A stream that ends without a finish reason or `[DONE]`, or whose
  `finish_reason` is `content_filter`/`error` (or null with a
  `native_finish_reason`), is a failed turn, never a short answer.
- `GET {base_url}/models` (optional): the entry matching `name` supplies the
  context window through `context_length`, `max_model_len` or
  `context_window`. Without one, the catalog or configured value stands.
- A `usage` object, in its own frame or on the last choice frame, feeds token
  accounting.

---

### Portkey

AI gateway with observability, caching, and routing.

| Setting    | Value                          |
|------------|--------------------------------|
| Provider id | `portkey`                     |
| API key env | `PORTKEY_API_KEY`            |
| Default URL | `https://api.portkey.ai/v1`  |

```yaml
model:
  provider: portkey
  name: gpt-4o
  api_key_env: PORTKEY_API_KEY
  driver_options:
    portkey_virtual_key: pk-live-xxx  # sent as the x-portkey-virtual-key header
```

---

## Fast Inference

### Groq

Near-instant inference via custom LPU hardware.

| Setting    | Value                                |
|------------|--------------------------------------|
| Provider id | `groq`                              |
| API key env | `GROQ_API_KEY`                     |
| Default URL | `https://api.groq.com/openai/v1`   |

```yaml
model:
  provider: groq
  name: llama-3.3-70b-versatile
  api_key_env: GROQ_API_KEY
```

Catalog models: `llama-3.3-70b-versatile`, `llama-3.1-8b-instant`,
`mixtral-8x7b-32768`, `deepseek-r1-distill-llama-70b`.

---

### Cerebras

Ultra-fast inference on Cerebras silicon.

| Setting    | Value                            |
|------------|----------------------------------|
| Provider id | `cerebras`                      |
| API key env | `CEREBRAS_API_KEY`             |
| Default URL | `https://api.cerebras.ai/v1`   |

```yaml
model:
  provider: cerebras
  name: llama3.1-70b
  api_key_env: CEREBRAS_API_KEY
```

---

## Open Model Platforms

### Together AI

| Setting    | Value                               |
|------------|-------------------------------------|
| Provider id | `together`                         |
| API key env | `TOGETHER_API_KEY`                |
| Default URL | `https://api.together.xyz/v1`      |

```yaml
model:
  provider: together
  name: meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo
  api_key_env: TOGETHER_API_KEY
```

---

### Fireworks AI

| Setting    | Value                                         |
|------------|-----------------------------------------------|
| Provider id | `fireworks`                                  |
| API key env | `FIREWORKS_API_KEY`                         |
| Default URL | `https://api.fireworks.ai/inference/v1`     |

```yaml
model:
  provider: fireworks
  name: accounts/fireworks/models/llama-v3p3-70b-instruct
  api_key_env: FIREWORKS_API_KEY
```

---

### DeepInfra

| Setting    | Value                                          |
|------------|------------------------------------------------|
| Provider id | `deepinfra`                                   |
| API key env | `DEEPINFRA_API_KEY`                          |
| Default URL | `https://api.deepinfra.com/v1/openai`        |

---

### Nebius AI

| Setting    | Value                                   |
|------------|-----------------------------------------|
| Provider id | `nebius`                               |
| API key env | `NEBIUS_API_KEY`                      |
| Default URL | `https://api.studio.nebius.ai/v1`     |

---

### SambaNova

| Setting    | Value                               |
|------------|-------------------------------------|
| Provider id | `sambanova`                        |
| API key env | `SAMBANOVA_API_KEY`               |
| Default URL | `https://api.sambanova.ai/v1`      |

---

### NVIDIA NIM

| Setting    | Value                                         |
|------------|-----------------------------------------------|
| Provider id | `nvidia`                                     |
| API key env | `NVIDIA_API_KEY`                            |
| Default URL | `https://integrate.api.nvidia.com/v1`        |

---

### Hugging Face

| Setting    | Value                                        |
|------------|----------------------------------------------|
| Provider id | `huggingface`                               |
| API key env | `HF_API_KEY`                               |
| Default URL | `https://router.huggingface.co/v1`          |

---

## Specialized

### Mistral AI

| Setting    | Value                            |
|------------|----------------------------------|
| Provider id | `mistral`                       |
| API key env | `MISTRAL_API_KEY`              |
| Default URL | `https://api.mistral.ai/v1`    |

Catalog models: `mistral-large-latest`, `codestral-latest`, `open-mistral-nemo`,
`magistral-medium-latest`.

---

### xAI (Grok)

| Setting    | Value                      |
|------------|----------------------------|
| Provider id | `xai`                     |
| API key env | `XAI_API_KEY`            |
| Default URL | `https://api.x.ai/v1`     |

Catalog models: `grok-3`, `grok-3-mini`, `grok-2`.

---

### Perplexity

AI with real-time web search.

| Setting    | Value                              |
|------------|------------------------------------|
| Provider id | `perplexity`                      |
| API key env | `PERPLEXITY_API_KEY`             |
| Default URL | `https://api.perplexity.ai`       |

Catalog models: `sonar-pro`, `sonar`, `sonar-reasoning-pro`.

---

## Regional Providers

### DeepSeek

| Setting    | Value                               |
|------------|-------------------------------------|
| Provider id | `deepseek`                         |
| API key env | `DEEPSEEK_API_KEY`                |
| Default URL | `https://api.deepseek.com/v1`      |

Catalog models: `deepseek-chat`, `deepseek-reasoner`.

---

### Moonshot AI (Kimi)

| Setting    | Value                              |
|------------|------------------------------------|
| Provider id | `moonshot`                        |
| API key env | `MOONSHOT_API_KEY`               |
| Default URL | `https://api.moonshot.cn/v1`      |

---

### Qwen / DashScope (Alibaba)

| Setting    | Value                                                        |
|------------|--------------------------------------------------------------|
| Provider id | `dashscope`                                                 |
| API key env | `DASHSCOPE_API_KEY`                                        |
| Default URL | `https://dashscope.aliyuncs.com/compatible-mode/v1`        |

Catalog models: `qwen-max`, `qwen-plus`, `qwen2.5-72b-instruct`, `qwq-32b`.

---

### GLM / Zhipu AI

| Setting    | Value                                      |
|------------|--------------------------------------------|
| Provider id | `glm`                                     |
| API key env | `GLM_API_KEY`                            |
| Default URL | `https://open.bigmodel.cn/api/paas/v4`   |

---

### MiniMax

| Setting    | Value                               |
|------------|-------------------------------------|
| Provider id | `minimax`                          |
| API key env | `MINIMAX_API_KEY`                 |
| Default URL | `https://api.minimax.chat/v1`      |

---

### Baidu Qianfan

| Setting    | Value                                       |
|------------|---------------------------------------------|
| Provider id | `qianfan`                                  |
| API key env | `QIANFAN_API_KEY`                         |
| Default URL | `https://qianfan.baidubce.com/v2`          |

---

## Local / OSS

Local providers are marked local in the registry: a request may legitimately
take minutes to start answering (prefill of a long prompt), so their stall
limits are far more generous than a hosted provider's, and sven asks them what
they serve (see [Local model discovery](configuration.md#local-model-discovery)).
`ollama`, `vllm` and `lmstudio` need no key; `brain` needs the key it mints.

### Brain (local edge-AI server)

[brain](https://swedishembedded.com/docs/brain) is a local model server that
exposes an OpenAI-compatible endpoint. Sven talks to it like any other local
OpenAI-compatible server.

| Setting    | Value                                 |
|------------|---------------------------------------|
| Provider id | `brain`                              |
| API key env | `BRAIN_API_KEY`                      |
| Default URL | `http://127.0.0.1:8788/v1`           |

```yaml
model:
  provider: brain
  name: ""          # empty = use the model brain is serving
```

```sh
sven -M brain/<model id as listed by /v1/models> "Hello"
```

What sven consumes:

- `GET /v1/models`: the served ids. An empty `model.name` resolves to the only
  model, or the first one that reports `context_length`. Optional per-model
  fields `max_output_tokens`, `capabilities` and `adapter` (`id`, `digest`)
  are used when present.
- `POST /v1/chat/completions` with `stream: true`, as described in
  [Any OpenAI-compatible hosted endpoint](#any-openai-compatible-hosted-endpoint).
- A bearer key. Sven uses `api_key`, `api_key_env`, `BRAIN_API_KEY`, or the
  `"openai"` entry of brain's keys file (a JSON object `{"<dialect>": "<key>"}`)
  at `$BRAIN_API_KEYS_FILE`, else `$XDG_RUNTIME_DIR/brain/api-keys.json`, else
  `<state dir>/brain/api-keys.json`. The file is read each time a provider is
  built, so a restarted server with a new key needs no re-export.

Thinking models (the Qwen family) reason by default: sven sends no
`enable_thinking`, so the served chat template decides. To turn reasoning off,
set it in the configuration file (a `--model` flag that changes the provider
clears `driver_options`, so name the model in the file as well):

```yaml
model:
  provider: brain
  name: <model id>
  driver_options:
    chat_template_kwargs:
      enable_thinking: false
```

An explicitly named `model.name` is sent as-is and bypasses resolution.
Auto-detection of brain is described in
[Default provider auto-detection](#default-provider-auto-detection).

### Brain over D-Bus (multimodal, Linux)

`provider: dbus` talks to brain on the D-Bus session or system bus instead of
HTTP. Images and audio travel as raw tensors on sealed memfd file descriptors
rather than as base64 in a JSON body.

```yaml
providers:
  brain_dbus:
    name: dbus
    input_modalities: [text, image, audio]
    driver_options:
      bus: session            # session | system | a raw D-Bus address
      service: com.swedishembedded.Brain1
      object_path: /com/swedishembedded/Brain1
      action: generate
      transport: memfd
      max_image_dim: 1024     # longest image side sent as raw pixels
    models:
      brain/omni:
        max_tokens: 32768
```

Every `driver_options` key is optional; the values above are the defaults
(`bus` defaults to `session`). Select it with `-M brain_dbus/brain/omni`.

What sven sends and expects, on interface
`com.swedishembedded.Brain1.Manager`:

- `Run(model, action, params, in_fds, in_meta, transport)` returns
  `(result, out_fds, out_meta)`. `params` is a JSON object holding `messages`
  (the conversation as a JSON string, with image and audio parts replaced by
  `[image]`/`[audio]` markers) and, when set, `max_new`, `temperature`,
  `top_p`, `top_k`, `seed` and `logprobs`. At most one image and one audio
  blob are sent per request; extra attachments are dropped with a warning.
  The generated text arrives in the `text` entry of `out_fds`.
- `ListModels()` lists model ids; failure is treated as "unknown".

Only the one-shot `Run` call is used, so the whole reply arrives at once: there
is no incremental token display. The connection is made lazily on the first
request.

The transport is compiled only on Linux and only with the `dbus` feature. The
default `sven` build has it, `--no-default-features --features minimal` does
not, and an SDK application gets it from `sven-sdk`'s default features. A build
without it still recognises `provider: dbus` and refuses it with that reason
rather than sending the request over HTTP.

---

### Ollama

Run models locally with [Ollama](https://ollama.ai).

| Setting    | Value                                 |
|------------|---------------------------------------|
| Provider id | `ollama`                             |
| Auth       | None                                  |
| Default URL | `http://localhost:11434/v1`          |

```sh
ollama pull llama3.2
sven -M ollama/llama3.2 "Hello"
```

```yaml
model:
  provider: ollama
  name: llama3.2
```

Catalog models: `llama3.2`, `qwen2.5-coder:7b`, `deepseek-r1:7b`, `mistral`.

---

### vLLM

High-throughput inference server.

| Setting    | Value                            |
|------------|----------------------------------|
| Provider id | `vllm`                          |
| Auth       | Optional bearer token (sent when a key resolves) |
| Default URL | `http://localhost:8000/v1`      |

```yaml
model:
  provider: vllm
  name: meta-llama/Llama-3.1-8B-Instruct
  base_url: http://my-vllm-server:8000/v1
```

---

### LM Studio

Desktop app for running local models.

| Setting    | Value                            |
|------------|----------------------------------|
| Provider id | `lmstudio`                      |
| Auth       | None                             |
| Default URL | `http://localhost:1234/v1`      |

```yaml
model:
  provider: lmstudio
  name: loaded-model-name
```

---

### Custom local server (`sven`)

`sven` is the id for a self-run OpenAI-compatible server on a host you name.
It behaves like `vllm` but has no default URL.

| Setting    | Value                                      |
|------------|--------------------------------------------|
| Provider id | `sven`                                    |
| Auth       | Optional bearer token                      |
| Requires   | `base_url` set                             |

```yaml
model:
  provider: sven
  name: ""                       # empty = the model the server serves
  base_url: http://gpu-box:8000/v1
```

---

## Mock (offline)

| Setting    | Value                  |
|------------|------------------------|
| Provider id | `mock`                |
| Auth       | None, no network       |

With no responses file the mock echoes the last user message as
`MOCK: <message>`. With `model.mock_responses_file` (or the
`SVEN_MOCK_RESPONSES` environment variable, which wins) it replays a YAML
script:

```yaml
responses:
  - match_type: contains     # contains | equals | starts_with | regex | default
    pattern: "ping"
    reply: "pong"
  - match_type: contains
    pattern: "write a file"
    tool_calls:
      - id: tc-1
        tool: fs
        args: {operation: write, path: test.txt, content: "hello"}
    after_tool_reply: "File written."
  - match_type: default
    reply: "I understand your request."
```

A rule may also set `thinking` (emitted before the reply) or `error` (the
stream fails with that message). A top-level `request_log` path appends every
request the mock receives to that file as one JSON line.

---

## Adding a custom provider

Any service that speaks the OpenAI chat-completions dialect works through the
`openai` provider with a different `base_url`:

```yaml
model:
  provider: openai
  name: my-custom-model
  base_url: https://my-custom-api.example.com/v1
  api_key_env: MY_API_KEY
```

To give an endpoint its own name, define it under `providers:` and select it
with `-M <name>/<model>`:

```yaml
providers:
  workbench:
    name: openai
    base_url: https://my-custom-api.example.com/v1
    api_key_env: MY_API_KEY
    models:
      my-custom-model:
        max_tokens: 65536
```

Each `providers.<name>` entry takes `name` (the driver id), `base_url`,
`api_key_env`, `api_key`, `max_tokens`, `max_output_tokens`, `max_input_tokens`,
`temperature`, `driver_options`, `azure_*`, `aws_region`,
`mock_responses_file`, `input_modalities` and `models`; a `models.<id>` entry
overrides the token limits, `temperature`, `driver_options`, the `cache_*`
flags, `mock_responses_file` and `input_modalities`.
`input_modalities` (any of `text`, `image`, `audio`) declares what a
self-hosted model accepts, since the bundled catalog only knows public models.

For OpenAI-compatible providers, `driver_options` is a JSON object merged into
every request body; its keys win over the ones sven computes (for example
`parse_tool_calls: false` for llama.cpp servers).

To contribute a driver with registry metadata, see
[DRIVERS.md](../../crates/model-drivers/DRIVERS.md).

---

## Full provider listing

Run `sven list-providers` for the canonical list of registered providers with
their API key variables and default URLs.
