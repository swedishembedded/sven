# Scheduler

Sven has a built-in scheduler for automating recurring tasks. The scheduler
supports cron expressions, fixed-interval jobs, one-shot timers, and a
configurable heartbeat that wakes the agent periodically.

## Heartbeat

The heartbeat is the simplest form of automation: the agent wakes up on a
fixed interval, reviews its standing instructions, and acts.

```yaml
# ~/.config/sven/node.yaml
scheduler:
  heartbeat:
    enabled: true
    every: "30m"
    prompt: |
      Heartbeat check: review pending tasks, emails, and notifications.
      Act on anything urgent. Reply HEARTBEAT_OK if nothing requires action.
```

### HEARTBEAT.md

Place a `HEARTBEAT.md` file in your workspace root to provide standing
instructions that are appended to every heartbeat turn:

```markdown
# Heartbeat Instructions

- Check the inbox for emails from VIP contacts (Alice, Bob, Carol).
- If there are new GitHub issues labeled "urgent", create a task for them.
- Send a morning summary to Telegram at 08:00 UTC if not already sent today.
- Monitor competitor pricing page: https://competitor.example.com/pricing
```

## Cron Jobs

The agent can create cron jobs using the `schedule` tool:

```
Create a daily job at 08:00 UTC that sends me a morning briefing via Telegram.
```

Or directly:

```
schedule create "morning-briefing" with cron "0 8 * * *" and prompt
"Review today's calendar, unread emails, and news. Send a summary to Telegram chat 123456789."
```

### schedule tool

| Action | Description |
|--------|-------------|
| `create` | Create a new job (interval, cron, or one-shot) |
| `list` | List all scheduled jobs |
| `delete` | Remove a job by ID |
| `enable` | Re-enable a disabled job |
| `disable` | Temporarily pause a job |

#### Examples

**Every 2 hours — check competitor prices:**

```json
{
  "action": "create",
  "name": "competitor-pricing",
  "every": "2h",
  "prompt": "Fetch https://competitor.example.com/pricing, compare with last check, report any changes."
}
```

**Daily at 09:00 UTC — email digest:**

```json
{
  "action": "create",
  "name": "email-digest",
  "cron": "0 9 * * *",
  "prompt": "List unread emails from the last 24 hours, categorize by priority, draft replies to urgent ones.",
  "deliver_to": "telegram:123456789"
}
```

**One-shot reminder:**

```json
{
  "action": "create",
  "name": "dentist-reminder",
  "at": "2026-04-15T08:00:00Z",
  "prompt": "Remind me: dentist appointment at 10:00 AM today."
}
```

## Jobs File

Jobs are persisted in YAML at `~/.config/sven/scheduler/jobs.yaml`:

```yaml
scheduler:
  jobs_file: "~/.config/sven/scheduler/jobs.yaml"  # default
```

The file is written atomically after every modification. Jobs survive restarts.

## Configuration Reference

```yaml
scheduler:
  heartbeat:
    enabled: false        # default: disabled
    every: "30m"          # interval: "15m", "1h", "24h", etc.
    prompt: "Heartbeat check..."
  jobs_file: "~/.config/sven/scheduler/jobs.yaml"
```
