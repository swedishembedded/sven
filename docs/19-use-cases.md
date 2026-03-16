# Automation Use Cases

This guide maps seven real-world automation patterns to sven's configuration
and tools. Each use case combines multiple integrations into a cohesive workflow.

---

## 1. Autonomous Personal CRM

Analyze emails and calendar events to build a knowledge graph of contacts,
track action items, and prepare context before calls.

### What the agent does
- Reads incoming emails and extracts contact details, preferences, and action items
- Saves everything to semantic memory with entity tags
- Reviews context before meetings (pulling related memories)
- Sends follow-up reminders via Telegram

### Configuration

```yaml
# ~/.config/sven/node.yaml
scheduler:
  heartbeat:
    enabled: true
    every: "1h"

channels:
  telegram:
    bot_token: "${TELEGRAM_BOT_TOKEN}"
    allowed_users: [123456789]
```

```yaml
# ~/.config/sven/config.yaml
tools:
  email:
    backend: imap
    imap_host: "imap.gmail.com"
    username: "${EMAIL_USER}"
    password: "${EMAIL_PASSWORD}"
  memory:
    backend: sqlite
  calendar:
    backend: caldav
    url: "${CALDAV_URL}"
    username: "${CALDAV_USER}"
    password: "${CALDAV_PASSWORD}"
```

### HEARTBEAT.md

```markdown
# CRM Heartbeat

Every hour:
1. List unread emails (email list unread_only=true limit=20)
2. For each email, extract: sender name, company, phone, key facts, action items
3. Save extracted info with semantic_memory remember (entity=name, source=email, tags=[contact,crm])
4. Check tomorrow's calendar events; for each meeting attendee, recall their profile
5. If any action item is overdue, send reminder via Telegram
```

---

## 2. Business on Autopilot

Monitor competitors, scrape pricing, draft content, manage email, and track campaign metrics.

### Configuration

```yaml
scheduler:
  heartbeat:
    enabled: true
    every: "2h"
  jobs:
    # defined via schedule tool at runtime
```

```yaml
tools:
  email:
    backend: gmail
    oauth_client_id: "${GMAIL_CLIENT_ID}"
    oauth_client_secret: "${GMAIL_CLIENT_SECRET}"
```

### Example Scheduled Jobs

Create these once via the `schedule` tool:

```
schedule create "competitor-monitor" with cron "0 */4 * * *" and prompt
"Fetch https://competitor.example.com/pricing. Compare with last week's prices saved in memory.
If any price changed by more than 5%, send a Telegram alert."
```

```
schedule create "content-draft" with cron "0 9 * * 1" and prompt
"Draft 3 LinkedIn posts about recent industry news. Save drafts to /workspace/content/linkedin-drafts.md"
```

```
schedule create "email-triage" with cron "0 8,13,18 * * *" and prompt
"List unread emails. Auto-reply to common inquiries using templates in /workspace/email-templates/.
Flag anything requiring personal attention."
```

---

## 3. Proactive Daily Briefings

Wake up, review tasks, emails, news, and send a personalized summary via Telegram or WhatsApp.

### Configuration

```yaml
scheduler:
  heartbeat:
    enabled: false  # Use cron instead for precise timing

channels:
  telegram:
    bot_token: "${TELEGRAM_BOT_TOKEN}"
    allowed_users: [123456789]
```

### Scheduled Job

```json
{
  "action": "create",
  "name": "morning-briefing",
  "cron": "0 7 * * *",
  "prompt": "Morning briefing:\n1. Fetch top 5 headlines from https://news.ycombinator.com\n2. List today's calendar events\n3. List urgent unread emails\n4. Check open tasks\n5. Compose a concise summary and send to Telegram chat 123456789",
  "deliver_to": "telegram:123456789"
}
```

### Output Example

The agent sends you:

```
Good morning! Here's your briefing for Tuesday, April 15:

📅 Today's Schedule:
• 9:00 AM — Standup with engineering team
• 2:00 PM — Strategy call with Alice (Acme Corp)
• 4:30 PM — 1:1 with Bob

📧 Urgent Emails (3):
• Invoice from Cloudflare — due Friday
• Alice: "Can we move the call to 3pm?" → Replied ✓
• New customer inquiry from TechCorp

📰 Top News:
• OpenAI releases GPT-5 with 1M context
• Rust 2.0 stabilizes async traits
• Y Combinator opens W2026 applications

✅ Open Tasks: 4 (2 due today)
```

---

## 4. 3D Model Generation and Search

Research existing 3D models online or generate custom ones using AI tools.

### Configuration

No special integrations required — uses web search and shell tools.

### Example Prompts

```
Search Thingiverse for "Raspberry Pi 5 case with fan mount". Download the top 3 results,
evaluate their dimensions, and recommend the best fit for a 40mm fan.
```

```
I need a custom bracket for a 28mm diameter pipe with a 15-degree angle.
Research if any existing models match. If not, generate OpenSCAD code for it and save to /workspace/bracket.scad
```

### Skill Template

Create `/workspace/.sven/skills/3d-models.md`:

```markdown
# 3D Model Research and Generation

## Search
- Thingiverse: https://www.thingiverse.com/search?q={query}
- Printables: https://www.printables.com/search/models?q={query}
- MyMiniFactory: https://www.myminifactory.com/search/?query={query}

## Generation
- Use OpenSCAD syntax for parametric models
- Default resolution: $fn=50
- Include comments explaining parameters
- Save to /workspace/models/{name}.scad
```

---

## 5. Second Brain Knowledge Management

Text anything to remember — the agent builds a searchable local knowledge base.

### Configuration

```yaml
channels:
  telegram:
    bot_token: "${TELEGRAM_BOT_TOKEN}"
    allowed_users: [123456789]

tools:
  memory:
    backend: sqlite
```

### HEARTBEAT.md

```markdown
# Second Brain Agent

When a user sends a message:
- If it looks like something to remember (fact, idea, contact, task): save with semantic_memory remember
- If it's a question: search memory with semantic_memory recall and answer
- Categorize automatically: people, ideas, tasks, references, notes
- Confirm what was saved in your reply
```

### Usage

Just text your Telegram bot:
- "Remember: dentist appointment April 20 at 2pm"
- "Alice from Acme prefers calls after 2pm, doesn't do Mondays"
- "Good article on Rust async: https://..."
- "What's Alice's preference for meetings?"
- "What do I know about Acme Corp?"

---

## 6. Automated Content Creation

Watch batched videos, write captions, and schedule uploads.

### Configuration

```yaml
scheduler:
  heartbeat:
    enabled: true
    every: "6h"
```

### Workflow

1. Drop video files in `/workspace/content/raw/`
2. Agent detects new files and processes them:

```markdown
# Content Creation Heartbeat

Check /workspace/content/raw/ for new video files.
For each unprocessed video:
1. Get video metadata with: run_terminal_command ffprobe -v quiet -print_format json -show_format {file}
2. If it's a short (< 90 seconds), generate a TikTok/Reels caption
3. If it's longer, generate a YouTube title, description, and tags
4. Save metadata to /workspace/content/metadata/{filename}.json
5. Move processed files to /workspace/content/ready/
```

### Skill Template

```markdown
# Video Content Guidelines

## Short-form (TikTok/Reels)
- Hook in first 3 words
- 3-5 hashtags
- Call to action at end
- Max 150 chars

## YouTube
- Title: benefit-driven, 60 chars max, include keyword
- Description: first 150 chars is above fold
- Include timestamps for videos > 10 min
- Tags: 10-15, mix broad and specific
```

---

## 7. AI Voice Call Agents

Make voice calls to confirm appointments, collect information, and summarize conversations.

### Configuration

```yaml
tools:
  voice:
    tts_provider: "elevenlabs"
    tts_api_key: "${ELEVENLABS_API_KEY}"
    tts_voice_id: "21m00Tcm4TlvDq8ikWAM"
    call_provider: "twilio"
    twilio_account_sid: "${TWILIO_SID}"
    twilio_auth_token: "${TWILIO_TOKEN}"
    twilio_phone_number: "+12065551234"

  calendar:
    backend: caldav
    url: "${CALDAV_URL}"
```

### Appointment Confirmation

Create a scheduled job that calls clients before appointments:

```json
{
  "action": "create",
  "name": "appointment-confirmations",
  "cron": "0 16 * * *",
  "prompt": "Check tomorrow's calendar events that include an attendee phone number in the description. For each unconfirmed appointment, place a confirmation call. Use the voice call tool with a friendly script: 'Hi, this is sven calling on behalf of [your name]. I'm calling to confirm your appointment tomorrow at [time]. Please call back to reschedule if needed. Thank you!'. Save call result to memory."
}
```

### Usage

```
Make a call to +12065551234 to confirm the 2pm appointment tomorrow.
Tell them it's with Alice at Acme Corp and they can reschedule by calling back.
```

```
Transcribe the recording at /recordings/client-call-2026-04-15.mp3
and save the key points and any action items to memory.
```

---

## Complete Example Config

```yaml
# ~/.config/sven/node.yaml
scheduler:
  heartbeat:
    enabled: true
    every: "30m"
    prompt: "Heartbeat: check inbox, calendar, pending tasks. Act on urgent items."

channels:
  telegram:
    bot_token: "${TELEGRAM_BOT_TOKEN}"
    allowed_users: [123456789]

hooks:
  token: "${HOOKS_TOKEN}"
  mappings:
    gmail:
      path: "/hooks/gmail"
      prompt: "New email notification. Process inbox."

# ~/.config/sven/config.yaml
tools:
  email:
    backend: imap
    imap_host: "imap.gmail.com"
    username: "${EMAIL_USER}"
    password: "${EMAIL_PASSWORD}"
  calendar:
    backend: caldav
    url: "${CALDAV_URL}"
    username: "${CALDAV_USER}"
    password: "${CALDAV_PASSWORD}"
  memory:
    backend: sqlite
  voice:
    tts_provider: "elevenlabs"
    tts_api_key: "${ELEVENLABS_API_KEY}"
    call_provider: "twilio"
    twilio_account_sid: "${TWILIO_SID}"
    twilio_auth_token: "${TWILIO_TOKEN}"
    twilio_phone_number: "+12065551234"
```
