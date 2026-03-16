# Calendar Integration

Sven integrates with CalDAV servers and Google Calendar for reading and
creating events. This powers daily briefings, meeting preparation, CRM
action item tracking, and appointment scheduling.

## Configuration

### CalDAV (Nextcloud, Radicale, iCloud, Fastmail)

```yaml
tools:
  calendar:
    backend: caldav
    url: "https://nextcloud.example.com/remote.php/dav/calendars/user/personal/"
    username: "${CALDAV_USER}"
    password: "${CALDAV_PASSWORD}"
```

**Finding your CalDAV URL:**

- **Nextcloud**: Settings → Personal → Mobile & Desktop → Calendar
- **iCloud**: `https://caldav.icloud.com`
- **Google Workspace**: `https://apidata.googleusercontent.com/caldav/v2/`
- **Fastmail**: `https://caldav.fastmail.com/dav/`

### Google Calendar

```yaml
tools:
  calendar:
    backend: google
    oauth_client_id: "${GCAL_CLIENT_ID}"
    oauth_client_secret: "${GCAL_CLIENT_SECRET}"
    oauth_token_path: "~/.config/sven/gcal-token.json"
```

## calendar tool

| Action | Description |
|--------|-------------|
| `today` | List today's events |
| `upcoming` | Events in the next N days (default: 7) |
| `list` | Events in a specific date range |
| `create` | Create a new event |
| `update` | Update an existing event |
| `delete` | Delete an event |

### Examples

**Today's schedule:**

```json
{ "action": "today" }
```

**Upcoming week:**

```json
{ "action": "upcoming", "days": 7 }
```

**Create a meeting:**

```json
{
  "action": "create",
  "title": "Strategy call with Alice",
  "start": "2026-04-15T14:00:00Z",
  "end": "2026-04-15T15:00:00Z",
  "description": "Quarterly strategy review",
  "attendees": ["alice@example.com"]
}
```

**Update event time:**

```json
{
  "action": "update",
  "id": "event_abc123",
  "start": "2026-04-15T15:00:00Z",
  "end": "2026-04-15T16:00:00Z"
}
```

**Delete event:**

```json
{ "action": "delete", "id": "event_abc123" }
```

## Use Cases

- **Morning briefing** — include today's calendar in the daily summary
- **Meeting prep** — before a meeting, look up notes and context about attendees
- **CRM integration** — after a meeting, create follow-up events and save notes
- **Appointment booking** — agent checks availability and creates events on request
- **Voice call scheduling** — use `calendar create` after a voice call confirms appointment
