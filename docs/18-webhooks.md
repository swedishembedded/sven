# Webhooks

Sven's node can receive HTTP webhook calls from external services and trigger
agent runs in response. This enables real-time integrations with Gmail, GitHub,
payment processors, monitoring systems, and any other service that supports webhooks.

## Configuration

```yaml
# ~/.config/sven/node.yaml
hooks:
  token: "${HOOKS_TOKEN}"      # Bearer token required for all webhook calls
  mappings:
    gmail:
      path: "/hooks/gmail"
      prompt: "New Gmail notification received. Check your inbox for new unread emails and process them."
    github:
      path: "/hooks/github"
      prompt: "GitHub event received. Review and take appropriate action: {payload}"
      isolated: true           # Run in dedicated session (default: false)
```

Set `HOOKS_TOKEN` to a strong random string: `openssl rand -hex 32`

## Endpoints

### `POST /hooks/wake`

Wake the main agent session with an optional message.

```sh
curl -X POST https://myagent.example.com/hooks/wake \
  -H "Authorization: Bearer $HOOKS_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"message": "New competitor pricing update detected."}'
```

### `POST /hooks/agent`

Spawn an agent run with a custom prompt (isolated session).

```sh
curl -X POST https://myagent.example.com/hooks/agent \
  -H "Authorization: Bearer $HOOKS_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"prompt": "Summarize the latest news about AI and send to Telegram."}'
```

### `POST /hooks/{name}`

Custom-named hooks. The `{payload}` placeholder in the prompt template is
replaced with the raw request body.

```sh
curl -X POST https://myagent.example.com/hooks/github \
  -H "Authorization: Bearer $HOOKS_TOKEN" \
  -d '{"action":"opened","issue":{"title":"Bug: login broken"}}'
```

## Gmail Pub/Sub Integration

Gmail can push notifications to your webhook endpoint in real-time.

### Setup

1. Create a Google Cloud Pub/Sub topic:

   ```sh
   gcloud pubsub topics create gmail-push
   ```

2. Grant Gmail permission to publish:

   ```sh
   gcloud pubsub topics add-iam-policy-binding gmail-push \
     --member="serviceAccount:gmail-api-push@system.gserviceaccount.com" \
     --role="roles/pubsub.publisher"
   ```

3. Create a push subscription pointing to your sven node:

   ```sh
   gcloud pubsub subscriptions create sven-gmail \
     --topic=gmail-push \
     --push-endpoint="https://myagent.example.com/hooks/gmail" \
     --push-auth-service-account="your-sa@project.iam.gserviceaccount.com"
   ```

4. Subscribe your Gmail account to push notifications:

   ```sh
   curl -X POST \
     -H "Authorization: Bearer $(gcloud auth print-access-token)" \
     https://gmail.googleapis.com/gmail/v1/users/me/watch \
     -d '{"topicName":"projects/my-project/topics/gmail-push","labelIds":["INBOX"]}'
   ```

5. Configure the hook mapping:

   ```yaml
   hooks:
     token: "${HOOKS_TOKEN}"
     mappings:
       gmail:
         path: "/hooks/gmail"
         prompt: "New email arrived. List unread emails, identify important ones, and draft responses where appropriate."
   ```

## GitHub Webhooks

1. In your GitHub repository: Settings → Webhooks → Add webhook
2. Set the payload URL to `https://myagent.example.com/hooks/github`
3. Set content type to `application/json`
4. Set the secret to your `HOOKS_TOKEN`
5. Choose events (e.g., Issues, Pull requests)

```yaml
hooks:
  token: "${HOOKS_TOKEN}"
  mappings:
    github:
      path: "/hooks/github"
      prompt: "GitHub webhook received: {payload}. If this is an issue labeled 'bug', create a task to investigate."
      isolated: true
```

## Security

- All webhook endpoints require Bearer token authentication.
- The token is compared using constant-time comparison (no timing attacks).
- TLS is on by default (self-signed cert; use `sven node install-ca` to trust it).
- Set `insecure_dev_mode: false` (the default) in production.
- Webhooks are only active when `hooks.token` is set — omitting the token disables all endpoints.
