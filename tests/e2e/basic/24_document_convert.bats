#!/usr/bin/env bats
# 24_document_convert.bats - PDF to Markdown: the CLI and the agent tool.
#
# Covers:
#   • `sven document convert` on a born-digital PDF (no model involved)
#   • rerun resumes from the manifest
#   • `document_convert` called by the (mock) model returns a control record
#     with no page text, and writes the file
#   • with no OCR model the tool result tells the model to report and to ask
#     before trying command-line converters

load helpers

FIXTURE="${BATS_TEST_DIRNAME}/fixtures/two-page-text.pdf"
SCAN="${BATS_TEST_DIRNAME}/fixtures/one-page-scan.pdf"

setup() {
    WORK="$(mktemp -d "${TMPDIR:-/tmp}/sven_doc_XXXXXX")"
    cp "${FIXTURE}" "${WORK}/in.pdf"
    cp "${SCAN}" "${WORK}/scan.pdf"
}

teardown() {
    rm -rf "${WORK}"
}

@test "24.01 document convert writes Markdown and a manifest" {
    run "$BIN" document convert "${WORK}/in.pdf" "${WORK}/out.md" --ocr never --quiet
    [ "${status}" -eq 0 ]
    grep -q "committee budget review" "${WORK}/out.md"
    grep -q "library and the staff" "${WORK}/out.md"
    [ -f "${WORK}/out.md.manifest.json" ]
}

@test "24.02 a rerun reuses the accepted pages" {
    "$BIN" document convert "${WORK}/in.pdf" "${WORK}/out.md" --ocr never --quiet
    run "$BIN" document convert "${WORK}/in.pdf" "${WORK}/out.md" --ocr never --quiet --json
    [ "${status}" -eq 0 ]
    [[ "${output}" == *'"pages_reused": 2'* ]]
}

@test "24.03 a page selection outside the document is refused and writes nothing" {
    run "$BIN" document convert "${WORK}/in.pdf" "${WORK}/out.md" --ocr never --pages 5
    [ "${status}" -ne 0 ]
    [ ! -e "${WORK}/out.md" ]
}

@test "24.04 the model's tool call returns a record, never the page text" {
    cat > "${WORK}/mock.yaml" <<EOF
responses:
  - match_type: contains
    pattern: "convert the pdf"
    tool_calls:
      - id: tc1
        tool: document_convert
        args:
          source: "${WORK}/in.pdf"
          destination: "${WORK}/agent.md"
          ocr: never
    after_tool_reply: "Conversion finished."
EOF
    run bash -c 'cd "'"${WORK}"'" && echo "convert the pdf" | SVEN_MOCK_RESPONSES="'"${WORK}"'/mock.yaml" "$BIN" --headless --model mock --output-format json 2>/dev/null'
    [ "${status}" -eq 0 ]
    grep -q "committee budget review" "${WORK}/agent.md"
    [[ "${output}" == *'\"status\":\"ok\"'* ]]
    [[ "${output}" != *"committee budget review"* ]]
}

@test "24.05 without an OCR model the result instructs the model to report and ask" {
    cat > "${WORK}/mock.yaml" <<EOF
responses:
  - match_type: contains
    pattern: "convert the pdf"
    tool_calls:
      - id: tc1
        tool: document_convert
        args:
          source: "${WORK}/scan.pdf"
          destination: "${WORK}/agent.md"
          model: "nonexistent ocr"
    after_tool_reply: "Reported."
EOF
    cat > "${WORK}/config.yaml" <<EOF
providers: {}
EOF
    run bash -c 'cd "'"${WORK}"'" && echo "convert the pdf" | SVEN_MOCK_RESPONSES="'"${WORK}"'/mock.yaml" "$BIN" --config "'"${WORK}"'/config.yaml" --headless --model mock --output-format json 2>/dev/null'
    [ "${status}" -eq 0 ]
    [ ! -e "${WORK}/agent.md" ]
    [[ "${output}" == *no_ocr_model* ]]
    [[ "${output}" == *"Do you want me to try"* ]]
}

@test "24.06 the CLI reports a scanned page without an OCR model and exits 3" {
    echo "providers: {}" > "${WORK}/config.yaml"
    run "$BIN" --config "${WORK}/config.yaml" document convert "${WORK}/scan.pdf" "${WORK}/scan.md" --quiet
    [ "${status}" -eq 3 ]
    [[ "${output}" == *"no OCR model is available"* ]]
    [ ! -e "${WORK}/scan.md" ]
}
