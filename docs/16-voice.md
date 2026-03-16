# Voice Integration

Sven supports text-to-speech synthesis, speech-to-text transcription, and
outbound voice calls. This powers AI voice call agents that can confirm
appointments, collect information, and summarize conversations.

## Configuration

```yaml
tools:
  voice:
    # Text-to-speech
    tts_provider: "elevenlabs"       # elevenlabs | openai
    tts_api_key: "${ELEVENLABS_API_KEY}"
    tts_voice_id: "21m00Tcm4TlvDq8ikWAM"  # ElevenLabs Rachel voice

    # Speech-to-text
    stt_provider: "openai"           # openai (Whisper)
    # stt_api_key uses tts_api_key if same provider

    # Voice calls
    call_provider: "twilio"
    twilio_account_sid: "${TWILIO_SID}"
    twilio_auth_token: "${TWILIO_TOKEN}"
    twilio_phone_number: "+12065551234"
    webhook_base_url: "https://myagent.example.com"
```

## Providers

### Text-to-Speech

**ElevenLabs** (recommended for natural voices):

1. Sign up at <https://elevenlabs.io>
2. Copy your API key from the profile page
3. Browse voices at <https://elevenlabs.io/voice-library> and copy a voice ID

**OpenAI TTS** (integrated, lower latency):

```yaml
tts_provider: "openai"
tts_api_key: "${OPENAI_API_KEY}"
tts_voice_id: "alloy"    # alloy | echo | fable | onyx | nova | shimmer
```

### Speech-to-Text

**OpenAI Whisper** (via REST API):

```yaml
stt_provider: "openai"
# uses OPENAI_API_KEY
```

### Voice Calls

**Twilio** outbound calls:

1. Sign up at <https://twilio.com>
2. Purchase a phone number
3. Copy Account SID and Auth Token from the console

## voice tool

| Action | Description |
|--------|-------------|
| `call` | Place an outbound voice call |
| `synthesize` | Convert text to audio file |
| `transcribe` | Convert audio file to text |

### Examples

**Place a call:**

```json
{
  "action": "call",
  "to": "+12065551234",
  "script": "Hello! This is an automated call from sven. Your appointment is confirmed for Tuesday at 2pm. Press 1 to confirm or call us back to reschedule. Goodbye!"
}
```

**Synthesize speech:**

```json
{
  "action": "synthesize",
  "text": "Your daily briefing is ready. Today you have 3 meetings and 12 unread emails.",
  "output_path": "/tmp/briefing.mp3"
}
```

**Transcribe a recording:**

```json
{
  "action": "transcribe",
  "audio_path": "/recordings/call-2026-04-15.mp3"
}
```

## Voice Call Agents

For sophisticated call flows (collecting user input, branching logic), configure
Twilio TwiML webhooks to POST to sven's webhook endpoint:

```yaml
voice:
  webhook_base_url: "https://myagent.example.com"

hooks:
  token: "${HOOKS_TOKEN}"
  mappings:
    twilio-callback:
      path: "/hooks/twilio"
      prompt: "Twilio call event received: {payload}. Process the user's response and decide next steps."
```

## Use Cases

- **Appointment confirmation** — call clients, confirm/reschedule appointments
- **Daily audio briefing** — synthesize the morning summary as an MP3
- **Meeting transcription** — transcribe recorded meetings for notes
- **Cold outreach** — automated introductory calls with personalized scripts
