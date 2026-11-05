#!/usr/bin/env bats
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# Video from an agent, end to end: a scripted model asks `models` and
# `run_model`, sven drives a real `brain serve` (its deterministic mock video
# model, no weights, no GPU) through the OpenRouter /videos API, and a playable
# MP4 lands in the working directory. Asking again never renders twice; an
# edit continues the clip.
#
# Needs BRAIN_BIN, a brain binary; skipped, loudly, without one:
#   BRAIN_BIN=../brain/target/debug/brain make test/e2e/media

load ../basic/helpers

setup_file() {
    [[ -n "${BRAIN_BIN:-}" && -x "${BRAIN_BIN}" ]] || skip "media suite: set BRAIN_BIN to a brain binary"
    export MEDIA_DIR="$(mktemp -d)"
    export PORT="${MEDIA_BRAIN_PORT:-8941}"
    mkdir -p "${MEDIA_DIR}/models" "${MEDIA_DIR}/work"
    BRAIN_MOCK=1 BRAIN_DEVICE=cpu "${BRAIN_BIN}" serve --models-dir "${MEDIA_DIR}/models" \
        --openai "${PORT}" --api-keys-out "${MEDIA_DIR}/keys.json" --ready-file "${MEDIA_DIR}/ready" \
        >"${MEDIA_DIR}/brain.log" 2>&1 &
    echo $! >"${MEDIA_DIR}/brain.pid"
    for _ in $(seq 1 60); do
        [[ -e "${MEDIA_DIR}/ready" ]] && break
        sleep 0.5
    done
    [[ -e "${MEDIA_DIR}/ready" ]] || { cat "${MEDIA_DIR}/brain.log" >&3; return 1; }
    local key
    key="$(jq -r .openai "${MEDIA_DIR}/keys.json")"
    mkdir -p "${XDG_CONFIG_HOME}/sven"
    cat >"${XDG_CONFIG_HOME}/sven/config.yaml" <<YAML
providers:
  local-brain:
    name: brain
    base_url: http://127.0.0.1:${PORT}/v1
    api_key: ${key}
tools:
  media:
    provider: local-brain
YAML
}

teardown_file() {
    [[ -f "${MEDIA_DIR:-}/brain.pid" ]] && kill -9 "$(cat "${MEDIA_DIR}/brain.pid")" 2>/dev/null || true
    [[ -n "${MEDIA_DIR:-}" ]] && rm -rf "${MEDIA_DIR}" "${XDG_CONFIG_HOME}/sven/config.yaml"
}

media_sven() {
    (cd "${MEDIA_DIR}/work" && SVEN_MOCK_RESPONSES="${BATS_TEST_DIRNAME}/mock_responses.yaml" "${BIN}" --headless --model mock --mode agent 2>&1)
}

@test "01.01 models lists the mock video model with the edit it offers" {
    run media_sven <<<"which video models can I use?"
    [ "${status}" -eq 0 ]
    assert_output_contains "brain/mock-video: text->video"
    assert_output_contains "edits: extend"
}

@test "01.02 run_model writes a playable MP4 into the working directory" {
    run media_sven <<<"make a video of a red ball"
    [ "${status}" -eq 0 ]
    assert_output_contains "Saved the video to .sven/media/"
    local clip
    clip="$(ls "${MEDIA_DIR}"/work/.sven/media/*.mp4)"
    [ "$(head -c 8 "${clip}" | tail -c 4)" = "ftyp" ]
    cp "${clip}" "${MEDIA_DIR}/work/clip.mp4"
}

@test "01.03 the same request answers the clip already made, never a second render" {
    run media_sven <<<"make a video of a red ball"
    [ "${status}" -eq 0 ]
    assert_output_contains "made earlier"
    [ "$(ls "${MEDIA_DIR}"/work/.sven/media/*.mp4 | wc -l)" -eq 1 ]
}

@test "01.04 an edit continues a clip into a new file" {
    [ -f "${MEDIA_DIR}/work/clip.mp4" ] || skip "needs 01.02's clip"
    run media_sven <<<"continue clip.mp4"
    [ "${status}" -eq 0 ]
    assert_output_contains "Saved the video to .sven/media/"
    [ "$(ls "${MEDIA_DIR}"/work/.sven/media/*.mp4 | wc -l)" -eq 2 ]
}
