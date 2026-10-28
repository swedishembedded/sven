# Installation

## Requirements

- Linux (x86_64 or aarch64) or macOS; the install script supports both
- A terminal emulator that supports 256 colours (almost all modern terminals do)
- A model to talk to: an API key for a hosted provider (OpenRouter, Anthropic,
  OpenAI and others, see [Providers](../reference/providers.md)) or a local model server

---

## Option 0 - Install script (quickest)

Downloads the newest sven 4.x release for your platform from GitHub, checks it
against the release's published SHA-256 and that it runs, and installs it. On
Linux (x86_64 or aarch64) it installs the static build, which runs on any
distribution, glibc or musl; on macOS the universal build. Any other platform
is told plainly that it is not supported.

The script follows patch and minor releases by itself but never moves to a new
major version, which may change what you built on: a new major version comes
with a new script.

```sh
curl --proto '=https' --tlsv1.2 -sSf https://agentsven.com/install.sh | sh
```

Environment variables:

| Variable | Default | Effect |
|----------|---------|--------|
| `SVEN_VERSION` | newest 4.x | Install exactly this version, e.g. `4.0.0` |
| `SVEN_INSTALL_DIR` | `/usr/local/bin` | Where to put the binary |
| `SVEN_NO_SUDO` | unset | Set to `1` to never use sudo (fails if the directory is not writable) |

If the install directory is not writable and `sudo` is not available, the binary
goes to `~/.local/bin` instead. The script tells you when the install directory
is not on your `PATH`.

## Option 1 - Debian/Ubuntu package

Each release attaches a `.deb` for x86_64 (`sven-linux-x86_64.deb`) and aarch64
(`sven-linux-aarch64.deb`) on the
[releases page](https://github.com/swedishembedded/sven/releases).

```sh
sudo dpkg -i sven-linux-x86_64.deb
```

The package places the `sven` binary at `/usr/bin/sven`, installs shell
completion scripts for bash, zsh, and fish, and registers the `sven://` URL
handler used by [MCP OAuth](../guides/troubleshooting.md#mcp-oauth-authentication) sign-in.

---

## Option 2 - Build from source

### 1. Install Rust

If you do not have Rust installed, use the official installer:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

A recent stable toolchain is required.

### 2. Clone and build

```sh
git clone https://github.com/swedishembedded/sven.git
cd sven
make build/release
```

The optimised binary is produced at `target/release/sven`.

### 3. Install the binary

Copy it to a directory on your `PATH`:

```sh
sudo cp target/release/sven /usr/local/bin/
```

Or add `target/release` to your `PATH` temporarily to try it out:

```sh
export PATH="$PWD/target/release:$PATH"
```

---

## Shell completions

`sven completions <shell>` prints a completion script for bash, zsh, fish, elvish
or powershell. The common ones:

**bash**

```sh
sven completions bash > ~/.local/share/bash-completion/completions/sven
```

Or, if you prefer a system-wide install:

```sh
sven completions bash | sudo tee /usr/share/bash-completion/completions/sven
```

**zsh**

```sh
sven completions zsh > "${fpath[1]}/_sven"
```

**fish**

```sh
sven completions fish > ~/.config/fish/completions/sven.fish
```

After adding the completion file, restart your shell or source the relevant
file for the change to take effect.

---

## Verify your installation

```sh
sven --version
```

It prints `sven` followed by the version number.

---

## Set your API key

sven needs a model to talk to. With no model configured, it picks one from
what it finds: a brain server running here, then OpenRouter, Anthropic, OpenAI
(whichever key is stored),
and finally a local model server found on localhost. See
[Default provider auto-detection](../reference/providers.md#default-provider-auto-detection).

With brain, there is nothing to set: run `brain serve` and sven finds the
server, its address and its key.

For a hosted provider, store its key once. It is read from stdin, typed
without echo, and kept in `~/.config/sven/credentials.yaml` (mode 0600):

```sh
sven auth set openrouter     # or anthropic, openai, groq, ... (`sven list-providers`)
```

Keys are not read from the environment. A key can also live in a file named by
`api_key_file` in the config - see
[Configuration](../reference/configuration.md) for details.

---

## Quick smoke test

With a model available, check that sven can reach it:

```sh
sven doctor provider
```

then run a task:

```sh
echo "Say hello in one sentence." | sven --headless
```

You should see a response printed to standard output and the process should
exit cleanly. This actually calls the provider, so it is what tells you the key
works.

To check the binary itself without a key — and without any network call — use
the mock model:

```sh
echo "Say hello in one sentence." | sven --headless --model mock
```
