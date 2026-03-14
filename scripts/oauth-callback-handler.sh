#!/usr/bin/env bash
# OAuth callback handler for cursor://cursor.mcp when using Sven with Atlassian MCP.
#
# Register this script as the handler for the cursor:// protocol so that when
# the OAuth server redirects to cursor://cursor.mcp/callback?code=...&state=...,
# the callback is forwarded to Sven's local server.
#
# Default callback port: 5598 (must match oauth.callback_port in config).
CALLBACK_PORT="${SVEN_OAUTH_CALLBACK_PORT:-5598}"

# Usage: oauth-callback-handler.sh "cursor://cursor.mcp/callback?code=X&state=Y"
# The URL is typically passed as $1 when the OS invokes the protocol handler.
URL="$1"
if [[ -z "$URL" ]]; then
    echo "Usage: $0 <cursor://...>" >&2
    exit 1
fi

# Extract query string (everything after ?)
if [[ "$URL" == *"?"* ]]; then
    QUERY="${URL#*?}"
    curl -s -o /dev/null "http://127.0.0.1:${CALLBACK_PORT}/callback?${QUERY}"
else
    curl -s -o /dev/null "http://127.0.0.1:${CALLBACK_PORT}/callback"
fi
