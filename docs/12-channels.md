# Messaging Channels

Sven can receive messages from and send messages to popular messaging platforms.
This lets you interact with your agent via your preferred chat app and have the
agent proactively deliver alerts, briefings, and summaries to you.

## Supported Channels

| Channel | Transport | Requirements |
|---------|-----------|--------------|
| Telegram | Bot API (long-polling) | Bot token from @BotFather |
| Discord | REST API (polling) | Bot token + Message Content Intent |
| WhatsApp | Business Cloud API (webhook) | Meta Business account |
| Signal | signal-cli subprocess | signal-cli binary + registered number |
| Matrix | Client-Server API (sync) | Account on any homeserver |
| IRC | TCP socket | IRC server credentials |

## Quick Start

### Telegram

1. Open [@BotFather](https://t.me/BotFather) in Telegram, run `/newbot`.
2. Copy the bot token.
3. Add to `~/.config/sven/node.yaml`:

```yaml
channels:
  telegram:
    bot_token: "${TELEGRAM_BOT_TOKEN}"
    allowed_users: [123456789]  # your Telegram user ID; empty = allow all
```

1. Set the environment variable: `export TELEGRAM_BOT_TOKEN=your_token`
2. Start the node: `sven node start`
3. Message your bot — sven responds.

### Discord

1. Create a Discord application at <https://discord.com/developers/applications>
2. Add a Bot under the "Bot" section and copy the token.
3. Enable **Message Content Intent** in Bot settings.
4. Invite the bot to your server with the OAuth2 URL generator (scope: `bot`).
5. Add to `~/.config/sven/node.yaml`:

```yaml
channels:
  discord:
    bot_token: "${DISCORD_BOT_TOKEN}"
    guild_ids: []           # empty = all guilds
    allowed_channel_ids: [] # empty = all text channels
```

### WhatsApp

WhatsApp requires a verified Meta Business account and a phone number registered
in the WhatsApp Business Platform.

1. Register at <https://developers.facebook.com> and create a Business App.
2. Add the WhatsApp product. Note your `phone_number_id` and create a permanent access token.
3. Configure a webhook in the Meta portal pointing to `https://<your-node>/channels/whatsapp`.
4. Set the verify token to the same value in the config:

```yaml
channels:
  whatsapp:
    phone_number_id: "1234567890"
    access_token: "${WHATSAPP_TOKEN}"
    verify_token: "${WHATSAPP_VERIFY_TOKEN}"
```

### Signal

Requires [signal-cli](https://github.com/AsamK/signal-cli/releases) installed and a registered phone number.

```sh
signal-cli -u +12065551234 register
signal-cli -u +12065551234 verify <code>
```

```yaml
channels:
  signal:
    signal_cli_path: "/usr/local/bin/signal-cli"
    phone_number: "+12065551234"
```

### Matrix

```yaml
channels:
  matrix:
    homeserver: "https://matrix.org"
    username: "@sven:matrix.org"
    password: "${MATRIX_PASSWORD}"
    room_ids: ["!roomid:matrix.org"]  # empty = all invited rooms
```

### IRC

```yaml
channels:
  irc:
    server: "irc.libera.chat"
    port: 6697
    tls: true
    nickname: "sven-bot"
    channels: ["#sven"]
    password: "${IRC_NICKSERV_PASSWORD}"
```

## Sending Messages from the Agent

The agent can proactively send messages via the `send_message` tool:

```
Send a Telegram message to user 123456789 saying: "Your daily briefing is ready!"
```

The tool signature:

```json
{
  "channel": "telegram",
  "recipient": "123456789",
  "text": "Your daily briefing is ready!"
}
```

See [use cases](19-use-cases.md) for practical automation patterns.
