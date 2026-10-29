# Model Providers

Sven registers 36 provider ids. Five are native drivers (`openai`,
`anthropic`, `google`, `aws`, `cohere`); `dbus` is a native transport to a
local brain server; `mock` is an offline test provider; every other id speaks
the OpenAI chat-completions dialect through one shared driver and differs only
in its default URL and whether it needs a key. `sven list-providers` prints the
registry.

## Quick start

Set the provider and model in your config file (`~/.config/sven/config.yaml`):

```yaml
model:
  provider: groq
  name: llama-3.3-70b-versatile
```

and store the provider's key once (it is read from stdin, typed without echo
on a terminal):

```sh
sven auth set groq
```

Or override on the command line (`--model`/`-M`, also read from `SVEN_MODEL`):

```sh
sven -M groq/llama-3.3-70b-versatile "Write a hello world in Rust"
sven -M anthropic/claude-sonnet-4-6 "Explain async Rust"
sven -M ollama/llama3.2 "What is the capital of France?"
```

A key is looked up in this order: `api_key`, the file `api_key_file` names
(`~/` is the home directory), the key stored for the provider with
`sven auth set <provider>`, and, for `brain`, the connection record of the
running server. A stored key serves every model of its provider, and every
`providers:` entry of that driver at the provider's own endpoint, so
`openrouter/<any model>` needs it once. It is never sent to another
`base_url`: configuration is also read from the working directory, so an
endpoint a cloned repository names gets only the key it names itself
(`api_key_file`). Keys are never read from the environment. A provider that needs a key and
finds none fails at startup saying how to store one.

```sh
sven auth set openrouter            # store a key (stdin)
sven auth set anthropic --key-file ~/keys/anthropic
sven auth list                      # the providers with a stored key
sven auth remove openrouter
```

Keys live in `~/.config/sven/credentials.yaml`, mode 0600. On a CI runner,
pipe the secret in: `printf '%s' "$SECRET" | sven auth set anthropic`.

To list all providers:

```sh
sven list-providers
sven list-providers --verbose   # adds whether a key is stored, and the default URL
sven list-providers --json
```

To list every model that can be used now, with the name `--model` takes for
it: each local server that answers (brain, ollama, vLLM, LM Studio), each
`providers:` entry and each hosted provider with a stored key, asked live. A
source that does not answer, or refuses its key, is reported on stderr.

```sh
sven list-models                    # e.g. brain/Qwen/Qwen3-0.6B, openrouter/z-ai/glm-5.3-flash
sven list-models --provider openrouter
sven list-models --json
```

To check that the configured model can be used before starting a session:

```sh
sven doctor provider          # exits non-zero when it cannot
sven doctor provider --json
```

The check builds the provider, confirms that its endpoint answers and accepts
the key, that it serves the model, and that the model advertises tool calling.
A hosted provider is asked for its model list only, which costs nothing; no
completion is requested. Google, AWS, Azure, Cohere and D-Bus stop after the
provider check.

## Selecting a model

`--model` (also used for a sub-agent's model and mid-session model switches)
takes, in order of precedence:

1. `<name>[/model]` where `<name>` is a key of the `providers:` section: that
   entry's endpoint, key and per-model overrides. With no `/model` part the
   current model name is kept.
2. `provider/model` where `provider` is a registered id and the pair has been
   served by that provider's endpoint (see [Served models](#served-models)).
   Credentials of the active model are kept only when the
   provider is the same.
3. A bare model name that some provider's endpoint has served: the provider is
   inferred.
4. Otherwise `provider/model` (also for a model no endpoint is known to serve,
   which is kept as written), a bare provider id (keeps the model name) or a
   bare model name (keeps the provider) applied to the active configuration.
   Switching provider clears the old `base_url`, key, token limits,
   `driver_options` and Azure/AWS fields.

## Served models

sven bundles no list of models, limits or prices. What it knows about a model is
what the model's endpoint served, kept in a per-provider cache
(`~/.config/sven/model-cache/<provider>.json`) that is refreshed after a
successful model list and, for a hosted provider with a stored key, in the
background when the file is older than 24 hours, so a start-up without network
still knows the last served list. A list that fails is an error; sven never
substitutes entries of its own.

| figure | read from the endpoint's `GET /models` entry |
|--------|-----------------------------------------------|
| context window | `context_length`, else `max_model_len`, else `context_window`; Google `inputTokenLimit`; Cohere `context_length` |
| output cap | `top_provider.max_completion_tokens`, else `max_output_tokens`; Google `outputTokenLimit` |
| input modalities | `architecture.input_modalities`, else `capabilities`; otherwise text only |
| price | `pricing` (below) |

A figure the endpoint does not serve is unknown, and the configuration is
consulted instead (`model.max_tokens`, `max_output_tokens`, `input_modalities`,
`cost.prices`). With none of them there is no context-window gate, the driver's
default output cap applies and the price is unknown, never zero.

`pricing` is the OpenRouter shape: US dollars **per token** as decimal strings,
`prompt` and `completion` required, `input_cache_read` and `input_cache_write`
optional.

```json
{"id": "deepseek/deepseek-chat", "context_length": 163840,
 "top_provider": {"max_completion_tokens": 32768},
 "pricing": {"prompt": "0.00000014", "completion": "0.00000042",
             "input_cache_read": "0.000000014"}}
```

sven multiplies each figure by one million to get the per-million-token prices
of `cost.prices`. A cache write that is not stated is charged at the `prompt`
price, and no cache time-to-live is served. A price that is negative (OpenRouter
states `-1` for per-request pricing) or unreadable is unknown. A price declared
in `cost.prices` wins over a served one.

The anthropic, google, cohere and OpenAI-compatible drivers record their model
lists this way; Bedrock lists nothing, so its models are known by configuration
alone.

## Default provider auto-detection

When **no model is configured at all** (no `model:` block, no `--model`,
no `SVEN_MODEL`), sven picks a provider, in priority order:

1. **`brain`**: a brain server running on this machine, found without network
   I/O from the connection record it publishes (see
   [Brain](#brain-local-edge-ai-server)). The model name is left empty and
   resolved by asking brain's `/v1/models`. When brain does not answer or
   serves no model, and a key for 2-4 is stored, that hosted provider is used
   instead.
2. **OpenRouter** (`openrouter/auto`) when its key is stored.
3. **Anthropic** (`claude-sonnet-4-6`) when its key is stored.
4. **OpenAI** (`gpt-5.2`) when its key is stored.
5. **A local server** when none of the above applies: `brain` (8788),
   `ollama` (11434), `vllm` (8000) and `lmstudio` (1234) are probed on
   localhost concurrently for one second, and the first in that order that
   serves a model is used. See
   [Local model discovery](configuration.md#local-model-discovery).

Any explicit model configuration wins over auto-detection.

---

## Major Cloud Providers

### OpenAI

| Setting    | Value                        |
|------------|------------------------------|
| Provider id | `openai`                    |
| Key         | `sven auth set openai` |
| Default URL | `https://api.openai.com/v1` |

```yaml
model:
  provider: openai
  name: gpt-4o
```

Catalog models include `gpt-5.2`, `gpt-5.1`, `gpt-5-mini`, `gpt-4o`, `gpt-4.1`, `o3`, `o4-mini`.

---

### Anthropic

| Setting    | Value                         |
|------------|-------------------------------|
| Provider id | `anthropic`                  |
| Key         | `sven auth set anthropic` |
| Default URL | `https://api.anthropic.com`  |

```yaml
model:
  provider: anthropic
  name: claude-sonnet-4-6
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
| Key         | `sven auth set google` |
| Default URL | `https://generativelanguage.googleapis.com`       |

```yaml
model:
  provider: google
  name: gemini-2.5-flash
```

Catalog models: `gemini-2.5-pro`, `gemini-2.5-flash`, `gemini-2.0-flash`,
`gemini-1.5-pro-002`, `gemini-1.5-flash-002`.

Get a free API key at [aistudio.google.com](https://aistudio.google.com).

---

### Azure OpenAI

| Setting    | Value                    |
|------------|--------------------------|
| Provider id | `azure`                 |
| Key         | `api_key_file` beside `azure_resource` (sent as the `api-key` header) |
| Default URL | none: set `azure_resource` or `base_url` |

```yaml
model:
  provider: azure
  name: gpt-4o
  azure_resource: myresource       # subdomain of .openai.azure.com
  azure_deployment: my-gpt4o-dep  # optional, defaults to model name
  azure_api_version: "2024-02-01" # optional, defaults to 2024-02-01
  api_key_file: ~/.config/keys/azure
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
| Key         | `sven auth set cohere` |
| Default URL | `https://api.cohere.com`  |

```yaml
model:
  provider: cohere
  name: command-r-plus-08-2024
```

Catalog models: `command-r-plus-08-2024`, `command-r`, `command-nightly`.

---

## Gateways

### OpenRouter

Access 200+ models from many providers through a single API key.

| Setting    | Value                              |
|------------|------------------------------------|
| Provider id | `openrouter`                      |
| Key         | `sven auth set openrouter` |
| Default URL | `https://openrouter.ai/api/v1`   |

```yaml
model:
  provider: openrouter
  name: anthropic/claude-sonnet-4-6
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
| Key         | `api_key_file` beside the `base_url` |
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
| Key         | `sven auth set vercel` |
| Default URL | `https://sdk.vercel.ai/openai`    |

---

### Cloudflare AI Gateway

| Setting    | Value                                            |
|------------|--------------------------------------------------|
| Provider id | `cloudflare`                                    |
| Key         | `api_key_file` beside the `base_url` |
| Requires   | `base_url` set (your account-specific gateway URL) |

---

### Any OpenAI-compatible hosted endpoint

A metered or self-run service that speaks the OpenAI chat-completions dialect
works through the `openai` provider with a `base_url` and its own key. No
model-specific configuration is needed:

```yaml
model:
  provider: openai
  name: <model id as listed by the endpoint's /v1/models>
  base_url: https://api.example.com/v1
  api_key_file: ~/.config/keys/example
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
| Key         | `sven auth set portkey` |
| Default URL | `https://api.portkey.ai/v1`  |

```yaml
model:
  provider: portkey
  name: gpt-4o
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
| Key         | `sven auth set groq` |
| Default URL | `https://api.groq.com/openai/v1`   |

```yaml
model:
  provider: groq
  name: llama-3.3-70b-versatile
```

Catalog models: `llama-3.3-70b-versatile`, `llama-3.1-8b-instant`,
`mixtral-8x7b-32768`, `deepseek-r1-distill-llama-70b`.

---

### Cerebras

Ultra-fast inference on Cerebras silicon.

| Setting    | Value                            |
|------------|----------------------------------|
| Provider id | `cerebras`                      |
| Key         | `sven auth set cerebras` |
| Default URL | `https://api.cerebras.ai/v1`   |

```yaml
model:
  provider: cerebras
  name: llama3.1-70b
```

---

## Open Model Platforms

### Together AI

| Setting    | Value                               |
|------------|-------------------------------------|
| Provider id | `together`                         |
| Key         | `sven auth set together` |
| Default URL | `https://api.together.xyz/v1`      |

```yaml
model:
  provider: together
  name: meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo
```

---

### Fireworks AI

| Setting    | Value                                         |
|------------|-----------------------------------------------|
| Provider id | `fireworks`                                  |
| Key         | `sven auth set fireworks` |
| Default URL | `https://api.fireworks.ai/inference/v1`     |

```yaml
model:
  provider: fireworks
  name: accounts/fireworks/models/llama-v3p3-70b-instruct
```

---

### DeepInfra

| Setting    | Value                                          |
|------------|------------------------------------------------|
| Provider id | `deepinfra`                                   |
| Key         | `sven auth set deepinfra` |
| Default URL | `https://api.deepinfra.com/v1/openai`        |

---

### Nebius AI

| Setting    | Value                                   |
|------------|-----------------------------------------|
| Provider id | `nebius`                               |
| Key         | `sven auth set nebius` |
| Default URL | `https://api.studio.nebius.ai/v1`     |

---

### SambaNova

| Setting    | Value                               |
|------------|-------------------------------------|
| Provider id | `sambanova`                        |
| Key         | `sven auth set sambanova` |
| Default URL | `https://api.sambanova.ai/v1`      |

---

### NVIDIA NIM

| Setting    | Value                                         |
|------------|-----------------------------------------------|
| Provider id | `nvidia`                                     |
| Key         | `sven auth set nvidia` |
| Default URL | `https://integrate.api.nvidia.com/v1`        |

---

### Hugging Face

| Setting    | Value                                        |
|------------|----------------------------------------------|
| Provider id | `huggingface`                               |
| Key         | `sven auth set huggingface` |
| Default URL | `https://router.huggingface.co/v1`          |

---

## Specialized

### Mistral AI

| Setting    | Value                            |
|------------|----------------------------------|
| Provider id | `mistral`                       |
| Key         | `sven auth set mistral` |
| Default URL | `https://api.mistral.ai/v1`    |

Catalog models: `mistral-large-latest`, `codestral-latest`, `open-mistral-nemo`,
`magistral-medium-latest`.

---

### xAI (Grok)

| Setting    | Value                      |
|------------|----------------------------|
| Provider id | `xai`                     |
| Key         | `sven auth set xai` |
| Default URL | `https://api.x.ai/v1`     |

Catalog models: `grok-3`, `grok-3-mini`, `grok-2`.

---

### Perplexity

AI with real-time web search.

| Setting    | Value                              |
|------------|------------------------------------|
| Provider id | `perplexity`                      |
| Key         | `sven auth set perplexity` |
| Default URL | `https://api.perplexity.ai`       |

Catalog models: `sonar-pro`, `sonar`, `sonar-reasoning-pro`.

---

## Regional Providers

### DeepSeek

| Setting    | Value                               |
|------------|-------------------------------------|
| Provider id | `deepseek`                         |
| Key         | `sven auth set deepseek` |
| Default URL | `https://api.deepseek.com/v1`      |

Catalog models: `deepseek-chat`, `deepseek-reasoner`.

---

### Moonshot AI (Kimi)

| Setting    | Value                              |
|------------|------------------------------------|
| Provider id | `moonshot`                        |
| Key         | `sven auth set moonshot` |
| Default URL | `https://api.moonshot.cn/v1`      |

---

### Qwen / DashScope (Alibaba)

| Setting    | Value                                                        |
|------------|--------------------------------------------------------------|
| Provider id | `dashscope`                                                 |
| Key         | `sven auth set dashscope` |
| Default URL | `https://dashscope.aliyuncs.com/compatible-mode/v1`        |

Catalog models: `qwen-max`, `qwen-plus`, `qwen2.5-72b-instruct`, `qwq-32b`.

---

### GLM / Zhipu AI

| Setting    | Value                                      |
|------------|--------------------------------------------|
| Provider id | `glm`                                     |
| Key         | `sven auth set glm` |
| Default URL | `https://open.bigmodel.cn/api/paas/v4`   |

---

### MiniMax

| Setting    | Value                               |
|------------|-------------------------------------|
| Provider id | `minimax`                          |
| Key         | `sven auth set minimax` |
| Default URL | `https://api.minimax.chat/v1`      |

---

### Baidu Qianfan

| Setting    | Value                                       |
|------------|---------------------------------------------|
| Provider id | `qianfan`                                  |
| Key         | `sven auth set qianfan` |
| Default URL | `https://qianfan.baidubce.com/v2`          |

---

## Local / OSS

Local providers are marked local in the registry: a request may legitimately
take minutes to start answering (prefill of a long prompt), so their stall
limits are far more generous than a hosted provider's, and sven asks them what
they serve (see [Local model discovery](configuration.md#local-model-discovery)).
`ollama`, `vllm` and `lmstudio` need no key; `brain` needs the key it mints,
which sven reads from the server's connection record.

### Brain (local edge-AI server)

[brain](https://swedishembedded.com/docs/brain) is a local model server that
exposes an OpenAI-compatible endpoint. Sven talks to it like any other local
OpenAI-compatible server, and finds it, its address and its key with no
configuration: run `brain serve`, then `sven`.

| Setting    | Value                                 |
|------------|---------------------------------------|
| Provider id | `brain`                              |
| Key         | from the server's connection record  |
| Default URL | the running server's, else `http://127.0.0.1:8788/v1` |

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
- The connection record every `brain serve` with an HTTP surface publishes
  while it runs: `$XDG_RUNTIME_DIR/brain/serve-<pid>.json` (else
  `<tmpdir>/brain-<uid>/`), a JSON object whose `surfaces` list holds an
  `openai` entry with the `url` it listens on and its `key`. Sven uses the
  server on brain's default port when several run, else the most recently
  started; a configured `base_url` selects the record of that server. A record
  is trusted only when this user owns it and its directory and nobody else
  can write either (or read the record), and only while its process lives.
  It is read each time a provider is built, so a restarted server with a new
  key needs nothing. `api_key` or `api_key_file` still name a key explicitly.

Thinking models (the Qwen family) reason by default: unless told otherwise,
sven sends no `enable_thinking` and the served chat template decides. Set
`model.enable_thinking` to say it explicitly; the `brain` provider then sends it
as `chat_template_kwargs.enable_thinking`. A value that
`driver_options.chat_template_kwargs.enable_thinking` already holds wins, and a
`--model` flag that changes the provider clears both, so name the model in the
file:

```yaml
model:
  provider: brain
  name: <model id>
  enable_thinking: false     # unset sends nothing
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
      max_image_dim: 1024     # longest image side sent as raw pixels
    models:
      brain/omni:
        max_tokens: 32768
```

Every `driver_options` key is optional; the values above are the defaults
(`bus` defaults to `session`). An unknown key, the obsolete `transport` key,
or a value of the wrong type is logged as a warning, never silently ignored. Select it with `-M brain_dbus/brain/omni`.

What sven sends and expects, on interface
`com.swedishembedded.Brain1.Manager`:

- `Subscribe(model, action, params, in_fds, in_meta)` returns `(job, socket)`
  at once. `params` is a JSON object holding `messages`
  (the conversation as a JSON string, with image and audio parts replaced by
  `[image]`/`[audio]` markers), the sampling temperature as `temp` (always:
  the request's, the model's `temperature`, else 0.2, since brain decodes
  greedily without one) and, when set, `max_new`, `top_p`, `top_k`, `seed`,
  `logprobs` with `return_token_ids`, and the request's tools as `tools`
  (a JSON string of OpenAI function definitions, with `tool_choice` `"none"`
  when tools are offered but must not be called). At most one image and one
  audio blob are sent per request; extra attachments are dropped with a
  warning. Brain then writes one JSON frame per `SOCK_SEQPACKET` datagram:
  `progress` frames carry content `delta`s and `event`s (reasoning, tool-call
  start and argument pieces), `blob` frames carry a memfd, and the last frame
  is `done` with the result object or `error`. Whatever the stream did not
  carry (text, `reasoning_content`, `tool_calls` as a JSON string of
  `{id, name, arguments}`) is read from the `done` result, so an agent works
  over D-Bus as it does over HTTP. So are the decode's identity (`seed`,
  `adapter`, `base_digest`, `backend`, `engine`) and, when asked for, the
  token facts (`prompt_token_ids`, `completion_token_ids`,
  `completion_logprobs`).
- `ListModels()` lists model ids; failure is treated as "unknown".

- `Cancel(job)` is called when the response stream is dropped before `done`
  (an interrupted turn), so brain stops generating for nobody.

Because the stream starts when the job is admitted, a generation queued behind
others under parallel load does not trip the first-byte timeout. The
connection is made lazily on the first request.

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
  api_key_file: ~/.config/keys/my-endpoint
```

To give an endpoint its own name, define it under `providers:` and select it
with `-M <name>/<model>`:

```yaml
providers:
  workbench:
    name: openai
    base_url: https://my-custom-api.example.com/v1
    api_key_file: ~/.config/keys/my-endpoint
    models:
      my-custom-model:
        max_tokens: 65536
```

Each `providers.<name>` entry takes `name` (the driver id), `base_url`,
`api_key_file`, `api_key`, `max_tokens`, `max_output_tokens`, `max_input_tokens`,
`temperature`, `driver_options`, `azure_*`, `aws_region`,
`mock_responses_file`, `input_modalities` and `models`; a `models.<id>` entry
overrides the token limits, `temperature`, `driver_options`, the `cache_*`
flags, `mock_responses_file` and `input_modalities`.
`input_modalities` (any of `text`, `image`, `audio`) declares what a
model accepts when its server does not say.

For OpenAI-compatible providers, `driver_options` is a JSON object merged into
every request body; its keys win over the ones sven computes (for example
`parse_tool_calls: false` for llama.cpp servers).

To contribute a driver with registry metadata, see
[DRIVERS.md](../../crates/model-drivers/DRIVERS.md).

---

## Full provider listing

Run `sven list-providers` for the canonical list of registered providers with
their API key variables and default URLs.
