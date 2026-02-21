# Installation

## Requirements

- A 64-bit Linux system (x86_64 or arm64)
- A terminal emulator that supports 256 colours (almost all modern terminals do)
- An API key for at least one supported model provider (OpenAI or Anthropic)

---

## Option 1 — Debian/Ubuntu package

If a `.deb` package is available for your version, this is the simplest route.

```sh
sudo dpkg -i sven_0.1.0_amd64.deb
```

The package places the `sven` binary at `/usr/bin/sven` and installs shell
completion scripts for bash, zsh, and fish automatically.

---

## Option 2 — Build from source

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

```sh
# OpenAI (default provider)
export OPENAI_API_KEY="sk-..."

# Anthropic
export ANTHROPIC_API_KEY="sk-ant-..."
```

You can also put the key in the sven config file — see
[Configuration](05-configuration.md) for details.

---

## Quick smoke test

With your API key set, run:

```sh
echo "Say hello in one sentence." | sven --headless --model mock
```

If you do not have an API key yet, the `mock` model can be used for testing
without making any network calls. You should see a response printed to standard
output and the process should exit cleanly.
