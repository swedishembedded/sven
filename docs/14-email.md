# Email Integration

Sven can read and send email via IMAP/SMTP or the Gmail REST API. This enables
automation of inbox triage, auto-responses, CRM extraction, and daily email
briefings.

## Configuration

### IMAP/SMTP (any provider)

Works with Gmail (app password), Outlook, Fastmail, ProtonMail Bridge, and
any IMAP-capable mail server.

```yaml
# ~/.config/sven/config.yaml
tools:
  email:
    backend: imap
    imap_host: "imap.gmail.com"
    imap_port: 993
    smtp_host: "smtp.gmail.com"
    smtp_port: 587
    username: "${EMAIL_USER}"
    password: "${EMAIL_PASSWORD}"   # For Gmail use an App Password
```

For Gmail, generate an App Password at <https://myaccount.google.com/apppasswords>
(requires 2FA to be enabled).

### Gmail API (recommended for Gmail users)

The Gmail API provides richer access: labels, threads, search operators.

1. Create a Google Cloud project and enable the Gmail API.
2. Create OAuth2 credentials (Desktop application type).
3. Run the authorization flow once to obtain tokens.
4. Configure:

```yaml
tools:
  email:
    backend: gmail
    oauth_client_id: "${GMAIL_CLIENT_ID}"
    oauth_client_secret: "${GMAIL_CLIENT_SECRET}"
    oauth_token_path: "~/.config/sven/gmail-token.json"
```

## email tool

| Action | Description |
|--------|-------------|
| `list` | List recent messages |
| `read` | Read full message by ID |
| `send` | Send a new email |
| `reply` | Reply to a message by ID |
| `search` | Search by keyword |

### Examples

**List unread emails:**

```json
{ "action": "list", "unread_only": true, "limit": 10 }
```

**Read a message:**

```json
{ "action": "read", "id": "msg_123" }
```

**Send an email:**

```json
{
  "action": "send",
  "to": "alice@example.com",
  "subject": "Meeting follow-up",
  "body": "Hi Alice, thanks for the call. Here are the action items: ..."
}
```

**Reply to a message:**

```json
{
  "action": "reply",
  "id": "msg_456",
  "body": "Thank you for your message. I'll get back to you by Friday."
}
```

**Search by keyword:**

```json
{ "action": "search", "query": "invoice" }
```

## Use Cases

- **Daily email briefing** — schedule a cron job at 08:00 to list and summarize unread emails
- **Auto-response** — heartbeat checks inbox, replies to common questions automatically
- **CRM extraction** — read emails, extract contact details, save to semantic memory
- **Gmail Pub/Sub** — use the webhook integration ([webhooks guide](18-webhooks.md)) to trigger the agent in real-time when new mail arrives

## Gmail Real-Time Push (Pub/Sub)

For instant notification of new emails (rather than polling):

1. Enable the Gmail API and create a Pub/Sub topic.
2. Run `gmail.users.watch` to subscribe.
3. Configure the Pub/Sub push subscription to POST to `/hooks/gmail`.
4. Configure a webhook mapping:

```yaml
hooks:
  token: "${HOOKS_TOKEN}"
  mappings:
    gmail:
      path: "/hooks/gmail"
      prompt: "New email notification received. Check inbox for new unread emails and process them."
```

See [webhooks](18-webhooks.md) for the full setup.
