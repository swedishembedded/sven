# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

# Runs the tests and records the verdict where a verification step can read
# it: result.json holds {"passed": true|false}. Always exits 0, so a failing
# test is data for the harness, not a failed tool call.
if sh tests/test_total.sh; then
  echo '{"passed": true}' > result.json
else
  echo '{"passed": false}' > result.json
fi
exit 0
