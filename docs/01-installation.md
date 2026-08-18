# Installation

## Requirements

- A 64-bit Linux system (x86_64 or arm64)
- A terminal emulator that supports 256 colours (almost all modern terminals do)
- An API key for at least one supported model provider (OpenAI or Anthropic)

---

## Option 0 - Install script (quickest)

Downloads the latest release binary for your platform and installs it:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://agentsven.com/install | sh
```

Environment variables:

| Variable | Default | Effect |
|----------|---------|--------|
| `SVEN_VERSION` | latest | Install a specific version, e.g. `0.2.1` |
| `SVEN_INSTALL_DIR` | `/usr/local/bin` | Where to put the binary |
| `SVEN_NO_SUDO` | unset | Set to `1` to never use sudo (fails if the directory is not writable) |

## Option 1 - Debian/Ubuntu package

If a `.deb` package is available for your version, this is the simplest route.

```sh
sudo dpkg -i sven_0.1.0_amd64.deb
```

The package places the `sven` binary at `/usr/bin/sven` and installs shell
completion scripts for bash, zsh, and fish automatically.

---

## Option 2 - Build from source

### 1. Install Rust

If you do not have Rust installed, use the official installer:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

A recent stable toolchain (1.75 or later) is recommended.

### 2. Clone and build

```sh
git clone https://github.com/swedishembedded/sven.git
cd sven
make release
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

sven can generate completion scripts for bash, zsh, and fish.

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

You should see output like:

```
sven 0.1.0
```

---

## Set your API key

sven needs an API key to talk to a language model. The simplest way is to set
an environment variable. Add one of these lines to your shell profile
(`~/.bashrc`, `~/.zshrc`, or similar):

Sven defaults to OpenRouter (`openrouter/auto`), and auto-detects in the
order brain → OpenRouter → Anthropic → OpenAI when a key is present.
See [providers.md](providers.md#default-provider-auto-detection).

```sh
# OpenRouter (the default)
export OPENROUTER_API_KEY="sk-or-..."

# OpenAI
export OPENAI_API_KEY="sk-..."

# Anthropic
export ANTHROPIC_API_KEY="sk-ant-..."
```

You can also put the key in the sven config file - see
[Configuration](05-configuration.md) for details.

---

## Quick smoke test

With your API key set, run:

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
