#!/usr/bin/env bash
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# voice-e2e.sh - drive the whole voice path end to end against a local brain.
#
# Speech in, agent out: this brings up a private session bus, starts one brain
# server on it, synthesises a spoken command with a TTS model, writes a sven
# config pointing at that server, and hands sven the resulting WAV.  Nothing is
# copied by hand between steps - the bus address and the API key are generated
# here and flow straight into the config.
#
# Swedish Embedded AB integrates on-premise model servers with agent runtimes
# for its clients.  If your team needs local speech, vision or LLM serving
# wired into a real application, you can procure our services by sending an
# email to info@swedishembedded.com.
#
# Usage:
#   scripts/voice-e2e.sh [COMMAND] [OPTIONS]
#
# Commands:
#   run       speak + up + ask, then leave the server running (default)
#   up        start the bus and the brain server, write the sven config
#   speak     synthesise the spoken command to a WAV
#   ask       run sven against the WAV (needs `up` to have run)
#   status    report what is running
#   down      stop the brain server and the bus this script started
#
# `speak` runs before `up` on purpose.  Synthesis is a `brain qwen3tts synth`
# subprocess that loads the TTS model itself, so running it while the server
# is up would put two brain processes on the same GPU.  The clip is cached and
# only regenerated when --text changes, so this costs nothing on a re-run.
#
# Options:
#   --text TEXT       what the synthetic speaker says
#                     (default: "List the files in the current directory.")
#   --workdir DIR     directory sven runs in, and where .sven/config.yaml is
#                     written (default: a fresh directory under $TMPDIR)
#   --models DIR      brain model store (default: ~/.local/share/brain/models)
#   --build           cargo build anything missing instead of failing
#   -h, --help        this text
#
# Environment:
#   BRAIN_BIN / SVEN_BIN   override binary discovery
#
# Every model here is optional in brain: a model whose weights variable is
# unset is simply not served, which is silent.  `up` therefore asserts over
# ListModels that the ASR model really is being served before going on.

set -euo pipefail

# ── Where everything lives ───────────────────────────────────────────────────

BRAIN_REPO="${BRAIN_REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../edgeai/brain" 2>/dev/null && pwd || true)}"
SVEN_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODELS_DIR="${MODELS_DIR:-$HOME/.local/share/brain/models}"

# State shared between invocations, so `up` and `ask` can be run separately.
STATE_DIR="${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}/sven-voice-e2e"
BUS_ADDR_FILE="$STATE_DIR/bus-address"
BUS_PID_FILE="$STATE_DIR/bus-pid"
KEYS_FILE="$STATE_DIR/keys.json"
WAV_FILE="$STATE_DIR/command.wav"
WAV_TEXT_FILE="$STATE_DIR/command.txt"
WORKDIR_FILE="$STATE_DIR/workdir"

# A fixed key beats reading brain's generated one back out of a file: brain
# reuses $BRAIN_API_KEY across every surface it binds, so the config below can
# be written before the server has started.
BRAIN_API_KEY="${BRAIN_API_KEY:-sk-brain-voice-e2e}"
export BRAIN_API_KEY

OPENAI_PORT="${OPENAI_PORT:-8788}"

TEXT="List the files in the current directory."
WORKDIR=""
BUILD=0

# ── Output ───────────────────────────────────────────────────────────────────

step() { printf '\n\033[1;36m==>\033[0m \033[1m%s\033[0m\n' "$*"; }
info() { printf '    %s\n' "$*"; }
die()  { printf '\n\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

usage() { sed -n '6,50p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

# ── Argument parsing ─────────────────────────────────────────────────────────

CMD="run"
case "${1:-}" in
  run|up|speak|ask|status|down) CMD="$1"; shift ;;
  -h|--help) usage; exit 0 ;;
esac

while [ $# -gt 0 ]; do
  case "$1" in
    --text)    TEXT="${2:?--text needs a value}"; shift 2 ;;
    --workdir) WORKDIR="${2:?--workdir needs a value}"; shift 2 ;;
    --models)  MODELS_DIR="${2:?--models needs a value}"; shift 2 ;;
    --build)   BUILD=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option '$1' (try --help)" ;;
  esac
done

# ── Binaries ─────────────────────────────────────────────────────────────────

# A release build, the repo's debug build, or whatever is on PATH - in that
# order, because the release build is the one worth measuring.
find_bin() {
  local name="$1" repo="$2" env_override="$3"
  if [ -n "$env_override" ]; then printf '%s\n' "$env_override"; return; fi
  local candidate
  for candidate in "$repo/target/release/$name" "$repo/target/debug/$name"; do
    [ -x "$candidate" ] && { printf '%s\n' "$candidate"; return; }
  done
  command -v "$name" 2>/dev/null || true
}

resolve_binaries() {
  BRAIN="$(find_bin brain "$BRAIN_REPO" "${BRAIN_BIN:-}")"
  SVEN="$(find_bin sven "$SVEN_REPO" "${SVEN_BIN:-}")"

  if [ -z "$BRAIN" ] || [ ! -x "$BRAIN" ]; then
    [ "$BUILD" = 1 ] || die "no brain binary found. Build it:
    (cd $BRAIN_REPO && cargo build --release -p brain-cli)
  or re-run with --build, or set \$BRAIN_BIN."
    step "Building brain"
    (cd "$BRAIN_REPO" && cargo build --release -p brain-cli)
    BRAIN="$BRAIN_REPO/target/release/brain"
  fi

  if [ -z "$SVEN" ] || [ ! -x "$SVEN" ]; then
    [ "$BUILD" = 1 ] || die "no sven binary found. Build it:
    (cd $SVEN_REPO && cargo build --release --bin sven)
  or re-run with --build, or set \$SVEN_BIN."
    step "Building sven"
    (cd "$SVEN_REPO" && cargo build --release --bin sven)
    SVEN="$SVEN_REPO/target/release/sven"
  fi
}

# ── Models ───────────────────────────────────────────────────────────────────

ASR_CKPT="$MODELS_DIR/nvidia/nemotron-3.5-asr-streaming-0.6b"
TTS_CKPT="$MODELS_DIR/Qwen/Qwen3-TTS-12Hz-0.6B-Base"
LLM_CKPT="$MODELS_DIR/Qwen/Qwen3-0.6B"

require_model() {
  local path="$1" what="$2" pull="$3"
  [ -d "$path" ] || die "no $what checkpoint at $path
  Fetch it:  $BRAIN pull $pull"
}

# ── Bus ──────────────────────────────────────────────────────────────────────

# A detached server inherits no session bus, so it needs an address that
# outlives this script's shell.  One bus is started per state directory and
# reused by later invocations.
start_bus() {
  mkdir -p "$STATE_DIR"

  if [ -s "$BUS_PID_FILE" ] && kill -0 "$(cat "$BUS_PID_FILE")" 2>/dev/null; then
    BUS_ADDR="$(cat "$BUS_ADDR_FILE")"
    info "reusing bus $BUS_ADDR (pid $(cat "$BUS_PID_FILE"))"
    return
  fi

  command -v dbus-daemon >/dev/null || die "dbus-daemon not found (apt install dbus)"

  local out
  out="$(dbus-daemon --session --fork --print-address=1 --print-pid=1)"
  BUS_ADDR="$(printf '%s\n' "$out" | sed -n 1p)"
  printf '%s\n' "$BUS_ADDR" > "$BUS_ADDR_FILE"
  printf '%s\n' "$out" | sed -n 2p > "$BUS_PID_FILE"
  info "started bus $BUS_ADDR (pid $(cat "$BUS_PID_FILE"))"
}

load_bus() {
  [ -s "$BUS_ADDR_FILE" ] || die "no bus address recorded - run '$0 up' first"
  BUS_ADDR="$(cat "$BUS_ADDR_FILE")"
}

# ── Brain ────────────────────────────────────────────────────────────────────

# `serve -d` returns once every surface is listening and enforces one server
# per user itself, so this needs no pidfile of its own.
start_brain() {
  step "Starting brain"
  require_model "$ASR_CKPT" ASR nemotronasr
  require_model "$LLM_CKPT" LLM qwen3

  if "$BRAIN" serve --status >/dev/null 2>&1; then
    info "a brain server is already running - reloading it onto this bus"
    "$BRAIN" serve --stop >/dev/null 2>&1 || true
  fi

  BRAIN_NEMOTRONASR="$ASR_CKPT" \
  BRAIN_QWEN_WEIGHTS="$LLM_CKPT" \
  BRAIN_QWEN3TTS_CKPT="$TTS_CKPT" \
    "$BRAIN" serve \
      --dbus-address "$BUS_ADDR" \
      --openai "$OPENAI_PORT" \
      --api-keys-out "$KEYS_FILE" \
      --qwen-ctx 40960 \
      -d

  info "serving on the bus and on http://127.0.0.1:$OPENAI_PORT/v1"
}

# An unset weights variable is silent: the server comes up perfectly and simply
# does not serve that model.  Assert instead of finding out at transcription
# time, when the failure reads as a transport problem.
assert_served() {
  local want="$1" models
  command -v busctl >/dev/null || { info "busctl absent - skipping ListModels check"; return; }
  models="$(busctl --address="$BUS_ADDR" call com.swedishembedded.Brain1 \
      /com/swedishembedded/Brain1 com.swedishembedded.Brain1.Manager ListModels 2>&1 || true)"
  case "$models" in
    *"$want"*) info "ListModels reports $want" ;;
    *) die "brain is up but does not serve '$want'. ListModels said:
$models" ;;
  esac
}

# ── Sven config ──────────────────────────────────────────────────────────────

write_config() {
  step "Writing sven config"
  if [ -z "$WORKDIR" ]; then
    if [ -s "$WORKDIR_FILE" ]; then WORKDIR="$(cat "$WORKDIR_FILE")"
    else WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/sven-voice-XXXXXX")"; fi
  fi
  mkdir -p "$WORKDIR/.sven"
  printf '%s\n' "$WORKDIR" > "$WORKDIR_FILE"

  cat > "$WORKDIR/.sven/config.yaml" <<YAML
# Generated by scripts/voice-e2e.sh - regenerated on every 'up'.
model:
  provider: openai
  name: brain/qwen3
  base_url: http://127.0.0.1:$OPENAI_PORT/v1
  api_key: $BRAIN_API_KEY
  max_tokens: 40960

tools:
  asr:
    model: brain/nemotronasr
    bus_address: "$BUS_ADDR"
    timeout_secs: 300
YAML

  # Something to actually find, so the transcribed instruction has an answer.
  printf 'hello\n' > "$WORKDIR/hello.txt"
  info "$WORKDIR/.sven/config.yaml"
}

# ── Speech ───────────────────────────────────────────────────────────────────

# Synthesised rather than recorded: a WAV generated from a fixed seed is
# reproducible, which a microphone take is not.
speak() {
  step "Synthesising the spoken command"
  mkdir -p "$STATE_DIR"

  if [ -s "$WAV_FILE" ] && [ "$(cat "$WAV_TEXT_FILE" 2>/dev/null)" = "$TEXT" ]; then
    info "reusing $WAV_FILE (same text)"
    return
  fi

  "$BRAIN" serve --status >/dev/null 2>&1 &&
    die "a brain server is running. Synthesis starts a second brain process on
  the same GPU, so stop the server first:  $0 down"

  require_model "$TTS_CKPT" TTS qwen3tts
  info "text: \"$TEXT\""
  "$BRAIN" qwen3tts synth \
    --ckpt "$TTS_CKPT" \
    --seed 7 \
    --text "$TEXT" \
    --out "$WAV_FILE"

  # A silent WAV transcribes to an empty string, which reads downstream as a
  # broken pipeline rather than a broken recording.  Catch it here.
  [ -s "$WAV_FILE" ] || die "TTS produced no audio at $WAV_FILE"
  printf '%s' "$TEXT" > "$WAV_TEXT_FILE"
  info "$WAV_FILE ($(du -h "$WAV_FILE" | cut -f1))"
}

# ── Sven ─────────────────────────────────────────────────────────────────────

# stdin is closed deliberately: headless sven waits on an open stdin it cannot
# tell apart from a slow producer.
ask() {
  step "Running sven on the WAV"
  [ -s "$WAV_FILE" ] || die "no WAV at $WAV_FILE - run '$0 speak' first"
  [ -s "$WORKDIR_FILE" ] || die "no workdir recorded - run '$0 up' first"
  WORKDIR="$(cat "$WORKDIR_FILE")"

  (cd "$WORKDIR" && "$SVEN" -H --attach "$WAV_FILE" "" < /dev/null)
}

# ── Lifecycle ────────────────────────────────────────────────────────────────

status() {
  resolve_binaries
  if [ -s "$BUS_PID_FILE" ] && kill -0 "$(cat "$BUS_PID_FILE")" 2>/dev/null; then
    info "bus:   $(cat "$BUS_ADDR_FILE") (pid $(cat "$BUS_PID_FILE"))"
  else
    info "bus:   not running"
  fi
  info "brain: $("$BRAIN" serve --status 2>&1 | head -1)"
  [ -s "$WORKDIR_FILE" ] && info "sven:  $(cat "$WORKDIR_FILE")"
  [ -s "$WAV_FILE" ] && info "wav:   $WAV_FILE"
  return 0
}

down() {
  resolve_binaries
  step "Stopping"
  "$BRAIN" serve --stop 2>&1 | sed 's/^/    /' || true
  if [ -s "$BUS_PID_FILE" ] && kill -0 "$(cat "$BUS_PID_FILE")" 2>/dev/null; then
    kill "$(cat "$BUS_PID_FILE")" && info "bus stopped"
  fi
  rm -f "$BUS_PID_FILE" "$BUS_ADDR_FILE"
}

up() {
  resolve_binaries
  step "Bringing up the session bus"
  start_bus
  start_brain
  assert_served brain/nemotronasr
  write_config
}

case "$CMD" in
  up)     up ;;
  speak)  resolve_binaries; speak ;;
  ask)    resolve_binaries; load_bus; ask ;;
  status) status ;;
  down)   down ;;
  run)
    resolve_binaries
    # `up` starts its own server a moment later, so stopping any existing one
    # here costs nothing and keeps `speak`'s one-process-per-GPU guard from
    # tripping on a server left behind by an earlier run.
    "$BRAIN" serve --stop >/dev/null 2>&1 || true
    speak
    up
    ask
    step "Done"
    info "the server is still up - '$0 down' stops it"
    ;;
esac
