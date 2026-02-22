# Model Providers

Sven supports **35+ model providers** out of the box, all implemented natively
in Rust.  This document describes each provider, its configuration, and how to
get started.

## Quick start

Set the provider and model in your config file (`~/.config/sven/config.yaml`):

```yaml
model:
  provider: groq
  name: llama-3.3-70b-versatile
  api_key_env: GROQ_API_KEY
```

Or override on the command line:

```sh
sven -M groq/llama-3.3-70b-versatile "Write a hello world in Rust"
sven -M anthropic/claude-3-5-sonnet-20241022 "Explain async Rust"
sven -M ollama/llama3.2 "What is the capital of France?"
```

To list all available providers:

```sh
sven list-providers
sven list-providers --verbose   # includes API key env var and URL
sven list-providers --json
```

To list models for a specific provider:

```sh
sven list-models --provider groq
sven list-models --provider openai --refresh   # live API query
```

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

Featured models: `gpt-4o`, `gpt-4.1`, `o1`, `o3`, `o4-mini`

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
  name: claude-sonnet-4-5
  api_key_env: ANTHROPIC_API_KEY
```

Featured models: `claude-opus-4-5`, `claude-sonnet-4-5`, `claude-haiku-4-5`,
`claude-3-5-sonnet-20241022`

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
  name: gemini-2.0-flash
  api_key_env: GEMINI_API_KEY
```

Featured models: `gemini-2.5-pro-preview-05-06`, `gemini-2.0-flash`,
`gemini-1.5-pro-002`, `gemini-1.5-flash-002`

Get a free API key at [aistudio.google.com](https://aistudio.google.com).

---

### Azure OpenAI

| Setting    | Value                    |
|------------|--------------------------|
| Provider id | `azure`                 |
| API key env | `AZURE_OPENAI_API_KEY`  |

```yaml
model:
  provider: azure
  name: gpt-4o
  api_key_env: AZURE_OPENAI_API_KEY
  azure_resource: myresource       # subdomain of .openai.azure.com
  azure_deployment: my-gpt4o-dep  # deployment name in Azure portal
  azure_api_version: "2024-02-01" # optional, defaults to 2024-02-01
```

Or set `base_url` directly:

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
| Auth       | AWS credentials via env vars (no API key needed)        |

```yaml
model:
  provider: aws
  name: us.anthropic.claude-3-5-sonnet-20241022-v2:0
  aws_region: us-east-1  # optional, defaults to AWS_DEFAULT_REGION
```

Set credentials in environment:
```sh
export AWS_ACCESS_KEY_ID=AKIA...
export AWS_SECRET_ACCESS_KEY=...
export AWS_DEFAULT_REGION=us-east-1
```

Featured models:
- `us.anthropic.claude-3-5-sonnet-20241022-v2:0`
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
  name: command-r-plus
  api_key_env: COHERE_API_KEY
```

Featured models: `command-r-plus-08-2024`, `command-r`, `command-nightly`

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
  name: anthropic/claude-opus-4-5
  api_key_env: OPENROUTER_API_KEY
```

OpenRouter passes `HTTP-Referer: https://github.com/svenai/sven` automatically.

---

### LiteLLM

Proxy gateway to 100+ providers using OpenAI-compatible format.

| Setting    | Value           |
|------------|-----------------|
| Provider id | `litellm`      |
| Requires   | `base_url` set  |

```yaml
model:
  provider: litellm
  name: gpt-4o
  base_url: http://localhost:4000  # your LiteLLM proxy URL
```

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
    portkey_virtual_key: pk-live-xxx  # optional virtual key for routing
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

Featured models: `llama-3.3-70b-versatile`, `llama-3.1-8b-instant`,
`mixtral-8x7b-32768`, `deepseek-r1-distill-llama-70b`

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

Featured models: `mistral-large-latest`, `codestral-latest`, `mistral-nemo`,
`magistral-medium-latest`

---

### xAI (Grok)

| Setting    | Value                      |
|------------|----------------------------|
| Provider id | `xai`                     |
| API key env | `XAI_API_KEY`            |
| Default URL | `https://api.x.ai/v1`     |

Featured models: `grok-3`, `grok-3-mini`, `grok-2`

---

### Perplexity

AI with real-time web search.

| Setting    | Value                              |
|------------|------------------------------------|
| Provider id | `perplexity`                      |
| API key env | `PERPLEXITY_API_KEY`             |
| Default URL | `https://api.perplexity.ai`       |

Featured models: `sonar-pro`, `sonar`, `sonar-reasoning-pro`

---

## Regional Providers

### DeepSeek

| Setting    | Value                               |
|------------|-------------------------------------|
| Provider id | `deepseek`                         |
| API key env | `DEEPSEEK_API_KEY`                |
| Default URL | `https://api.deepseek.com/v1`      |

Featured models: `deepseek-chat` (V3), `deepseek-reasoner` (R1)

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

Featured models: `qwen-max`, `qwen-plus`, `qwen2.5-72b-instruct`, `qwq-32b`

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

No API key required for local providers.

### Ollama

Run models locally with [Ollama](https://ollama.ai).

| Setting    | Value                                 |
|------------|---------------------------------------|
| Provider id | `ollama`                             |
| Auth       | None                                  |
| Default URL | `http://localhost:11434/v1`          |

```sh
# Pull a model first:
ollama pull llama3.2

# Then configure sven:
sven -M ollama/llama3.2 "Hello"
```

```yaml
model:
  provider: ollama
  name: llama3.2
```

Popular models: `llama3.2`, `qwen2.5-coder:7b`, `deepseek-r1:7b`, `mistral`

---

### vLLM

High-throughput inference server.

| Setting    | Value                            |
|------------|----------------------------------|
| Provider id | `vllm`                          |
| Auth       | Optional bearer token            |
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

## Adding a custom provider

1. Set `provider: openai` (or any OpenAI-compatible provider)
2. Override `base_url` to your custom endpoint:

```yaml
model:
  provider: openai
  name: my-custom-model
  base_url: https://my-custom-api.example.com/v1
  api_key_env: MY_API_KEY
```

For providers not yet in the registry, this allows immediate use via the OpenAI
compatibility layer.  See [DRIVERS.md](../crates/sven-model/DRIVERS.md) for how
to contribute a proper driver with registry metadata.

---

## Full provider listing

Run `sven list-providers` for the canonical list of all 35+ registered
providers including their API key environment variables and default URLs.
