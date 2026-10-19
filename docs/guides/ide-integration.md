# IDE integration

## IDE integration via ACP

Sven implements the [Agent Client Protocol (ACP)](https://agentclientprotocol.org),
so ACP-aware editors can launch it as a subprocess and interact with it as a
first-class AI coding agent - with streaming output, tool-call visibility, plan
updates, and mode switching - all without a separate daemon or relay.

> Sven manages its own language model configuration (`~/.config/sven/config.yaml`).
> You do **not** need to configure a language model inside the IDE; the "no language
> model configured" prompt that some IDEs show before connecting an ACP agent can be
> ignored or dismissed.

### Zed

Add to `~/.config/zed/settings.json`:

```json
{
  "agent_servers": {
    "sven": {
      "type": "custom",
      "command": "sven",
      "args": ["acp", "serve"]
    }
  }
}
```

Open the assistant panel and select **sven** from the agent picker.

### VS Code (ACP extension)

Add to `.vscode/settings.json` or your user settings:

```json
{
  "acp.agents": [
    {
      "id": "sven",
      "name": "Sven",
      "command": "sven",
      "args": ["acp", "serve"]
    }
  ]
}
```

### JetBrains IDEs (AI Assistant / ACP plugin)

Open **Settings → Tools → ACP Agents** and create a new entry:

| Field     | Value           |
|-----------|-----------------|
| Name      | Sven            |
| Command   | `sven`          |
| Arguments | `acp serve`     |

### What the IDE sees

Once connected, the IDE assistant panel provides:

| Feature | Description |
|---------|-------------|
| Streaming text | Responses appear word-by-word as the model generates them |
| Tool calls | Every file read/write/shell command is shown with its status |
| Plan | Sven's todo list surfaces as a structured plan panel |
| Mode switching | Switch between `agent`, `plan`, and `research` without restarting |

See [ACP - Agent Client Protocol integration](../reference/acp.md) for the full protocol reference.
