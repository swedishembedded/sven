// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for speech input to agent
// interfaces for its clients. If your team needs expertise in voice
// dictation for terminal applications then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Voice dictation in the app: Ctrl+G toggles listening, the session's
//! events become level meter, provisional text and finished text in the
//! input of the session that was active when listening started. Nothing is
//! ever submitted.

use crate::{
    app::{ui_state::Toast, App},
    ui::VoiceStatus,
    voice::{StopReason, VoiceEvent},
};

impl App {
    /// Ctrl+G: start listening; stop it (the last phrases are still
    /// transcribed); or, while those are being transcribed, give up on them.
    #[cfg(feature = "voice")]
    pub(crate) fn toggle_voice(&mut self) {
        use crate::voice::VoicePhase;

        match self.voice.phase {
            VoicePhase::Idle => self.start_voice(),
            VoicePhase::Listening => {
                self.voice.phase = VoicePhase::Finishing;
                if let Some(handle) = self.voice.handle.take() {
                    handle.stop();
                }
            }
            VoicePhase::Finishing => {
                self.voice.reset();
                self.ui.push_toast(Toast::info("Voice input cancelled"));
            }
        }
    }

    /// Ctrl+G in a build without the `voice` feature.
    #[cfg(not(feature = "voice"))]
    pub(crate) fn toggle_voice(&mut self) {
        self.ui.push_toast(Toast::warning(
            "This build of sven has no voice input (built without the `voice` feature)",
        ));
    }

    #[cfg(feature = "voice")]
    fn start_voice(&mut self) {
        use crate::voice::{capture::DefaultMicrophone, session, VoicePhase};

        if self.edit.active() {
            self.ui
                .push_toast(Toast::warning("Finish editing before dictating"));
            return;
        }
        let voice = &self.tui_config.voice;
        let Some(endpoint) = self.config.voice_endpoint(voice.provider.as_deref()) else {
            self.ui.push_toast(Toast::error(format!(
                "Voice input needs a sven account or API key for provider '{}': \
                 run `sven auth login`, or set tui.voice.provider",
                voice.provider.as_deref().unwrap_or("sven")
            )));
            return;
        };
        let Some(events) = self.voice.tx.clone() else {
            self.ui
                .push_toast(Toast::error("Voice input is not available here"));
            return;
        };
        let settings = session::Settings {
            base_url: endpoint.base_url,
            api_key: endpoint.api_key,
            model: voice.model.clone(),
            language: voice.language.clone(),
            live_preview: voice.live_preview,
        };
        #[cfg(test)]
        let source: Box<dyn session::VoiceSource> = self
            .voice
            .test_source
            .take()
            .unwrap_or_else(|| Box::new(DefaultMicrophone));
        #[cfg(not(test))]
        let source: Box<dyn session::VoiceSource> = Box::new(DefaultMicrophone);

        self.voice.reset();
        self.voice.phase = VoicePhase::Listening;
        self.voice.target = Some(self.sessions.active_id.clone());
        self.voice.handle = Some(session::spawn(source, settings, events));
    }

    /// Applies what the listening session reports.
    pub(crate) fn handle_voice_event(&mut self, event: VoiceEvent) {
        // A cancelled or failed session may still have events on the way.
        if !self.voice.is_active() {
            return;
        }
        match event {
            VoiceEvent::Level(rms) => self.voice.push_level(rms),
            VoiceEvent::Partial { seq, text } => {
                if self.voice.last_final.is_none_or(|done| seq > done) {
                    self.voice.provisional = text;
                }
            }
            VoiceEvent::Final { seq, text } => {
                self.voice.last_final = Some(seq);
                self.voice.provisional.clear();
                self.insert_dictation(&text);
            }
            VoiceEvent::Error(message) => {
                self.voice.reset();
                self.ui
                    .push_toast(Toast::error(format!("Voice input: {message}")));
            }
            VoiceEvent::Stopped(reason) => {
                self.voice.reset();
                match reason {
                    StopReason::User => {}
                    StopReason::Silence => self
                        .ui
                        .push_toast(Toast::info("Voice input stopped: no speech for 30 seconds")),
                    StopReason::Limit => self
                        .ui
                        .push_toast(Toast::info("Voice input stopped: 10 minute limit reached")),
                }
            }
        }
    }

    /// Puts final text into the input of the session listening began in.
    fn insert_dictation(&mut self, text: &str) {
        let Some(target) = self.voice.target.clone() else {
            return;
        };
        if target == self.sessions.active_id {
            self.input.insert_dictation(text);
            self.ui.completion = None;
        } else if let Some(entry) = self.sessions.get_mut(&target) {
            // Its input is parked in the entry until the person switches back.
            let mut buffer = entry.stored_input_buffer.take().unwrap_or_default();
            let mut cursor = entry.stored_input_cursor.unwrap_or(buffer.len());
            crate::app::input_state::insert_dictation(&mut buffer, &mut cursor, text);
            entry.stored_input_buffer = Some(buffer);
            entry.stored_input_cursor = Some(cursor);
        } else {
            self.ui.push_toast(Toast::warning(
                "Voice input: the session it was for is gone, text dropped",
            ));
        }
    }

    /// What the input pane shows of the dictation: only in the session it
    /// is for.
    pub(crate) fn voice_status(&self) -> Option<VoiceStatus<'_>> {
        (self.voice.is_active() && self.voice.target.as_ref() == Some(&self.sessions.active_id))
            .then_some(VoiceStatus {
                listening: self.voice.is_listening(),
                levels: &self.voice.levels,
                provisional: &self.voice.provisional,
            })
    }
}

#[cfg(all(test, feature = "voice"))]
mod tests {
    //! Spec: Ctrl+G listens; each finished phrase is transcribed over HTTP and
    //! inserted into the input at the cursor; nothing is ever submitted; a
    //! failure stops listening with a toast. A fake microphone and a local
    //! HTTP server stand in for the device and the service.

    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    use super::*;
    use crate::keys::Action;
    use crate::voice::session::{Capture, VoiceSource};
    use crate::voice::VoicePhase;

    /// Plays `signal` into the session as soon as it is started, and stays
    /// open until the session is stopped.
    struct FakeMicrophone(Vec<f32>);

    impl VoiceSource for FakeMicrophone {
        fn start(&mut self) -> anyhow::Result<Capture> {
            let (tx, rx) = mpsc::channel(256);
            for chunk in self.0.chunks(1600) {
                tx.try_send(chunk.to_vec()).expect("room for the signal");
            }
            Ok(Capture::new(rx, Box::new(tx)))
        }
    }

    struct Failing;

    impl VoiceSource for Failing {
        fn start(&mut self) -> anyhow::Result<Capture> {
            Err(anyhow::anyhow!("no microphone found"))
        }
    }

    fn tone(seconds: f32) -> Vec<f32> {
        (0..(seconds * 16_000.0) as usize)
            .map(|i| 0.3 * (std::f32::consts::TAU * 440.0 * i as f32 / 16_000.0).sin())
            .collect()
    }

    fn silence(seconds: f32) -> Vec<f32> {
        vec![0.0; (seconds * 16_000.0) as usize]
    }

    /// What the mock service saw of one request.
    struct Seen {
        path: String,
        authorization: String,
        body: Vec<u8>,
    }

    impl Seen {
        fn has(&self, needle: &str) -> bool {
            self.body
                .windows(needle.len())
                .any(|w| w == needle.as_bytes())
        }
    }

    /// Answers every request with `status` and `answer`.
    async fn serve(
        status: u16,
        answer: serde_json::Value,
    ) -> (String, mpsc::UnboundedReceiver<Seen>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!(
            "http://127.0.0.1:{}/v1",
            listener.local_addr().unwrap().port()
        );
        let (seen_tx, seen_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let seen_tx = seen_tx.clone();
                let answer = answer.to_string();
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut reader = BufReader::new(read);
                    let mut line = String::new();
                    reader.read_line(&mut line).await.unwrap();
                    let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                    let (mut length, mut authorization) = (0usize, String::new());
                    loop {
                        let mut header = String::new();
                        reader.read_line(&mut header).await.unwrap();
                        let header = header.trim();
                        if header.is_empty() {
                            break;
                        }
                        if let Some((k, v)) = header.split_once(": ") {
                            match k.to_ascii_lowercase().as_str() {
                                "content-length" => length = v.parse().unwrap_or(0),
                                "authorization" => authorization = v.to_owned(),
                                _ => {}
                            }
                        }
                    }
                    let mut body = vec![0u8; length];
                    reader.read_exact(&mut body).await.unwrap();
                    let _ = seen_tx.send(Seen {
                        path,
                        authorization,
                        body,
                    });
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    );
                    let _ = write.write_all(reply.as_bytes()).await;
                });
            }
        });
        (base, seen_rx)
    }

    /// An app whose `tui.voice.provider` is a local service at `base`, with
    /// `signal` on the fake microphone.
    fn dictating_app(
        base: &str,
        signal: Vec<f32>,
        live_preview: bool,
    ) -> (App, mpsc::Receiver<VoiceEvent>) {
        let (mut app, _agent_rx) = App::for_testing();
        let mut config = sven_bootstrap::Config::default();
        config.providers.insert(
            "local".into(),
            sven_model_drivers::ProviderEntry {
                name: "openai".into(),
                base_url: Some(base.into()),
                api_key: Some("key-for-test".into()),
                ..Default::default()
            },
        );
        app.config = Arc::new(config);
        app.tui_config.voice.provider = Some("local".into());
        app.tui_config.voice.language = Some("sv".into());
        app.tui_config.voice.live_preview = live_preview;
        let (tx, rx) = mpsc::channel(64);
        app.voice.tx = Some(tx);
        app.voice.test_source = Some(Box::new(FakeMicrophone(signal)));
        (app, rx)
    }

    /// Feeds the app the session's events until `done` holds.
    async fn pump(
        app: &mut App,
        events: &mut mpsc::Receiver<VoiceEvent>,
        done: impl Fn(&App, &VoiceEvent) -> bool,
    ) {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
                .await
                .expect("the session reports in time")
                .expect("the channel stays open");
            app.handle_voice_event(event.clone());
            if done(app, &event) {
                return;
            }
        }
    }

    fn toasts(app: &App) -> Vec<String> {
        app.ui.toasts.iter().map(|t| t.message.clone()).collect()
    }

    #[tokio::test]
    async fn a_phrase_is_transcribed_inserted_at_the_cursor_and_never_submitted() {
        let (base, mut seen) = serve(
            200,
            json!({"text": "hej världen", "usage": {"seconds": 1.0}}),
        )
        .await;
        let signal = [tone(0.8), silence(0.6)].concat();
        let (mut app, mut events) = dictating_app(&base, signal, false);
        let (agent_tx, mut agent_rx) = mpsc::channel(8);
        app.agent.tx = Some(agent_tx);
        app.inject_input("fix");

        app.dispatch_action(Action::ToggleVoice).await;
        assert_eq!(app.voice.phase, VoicePhase::Listening);

        pump(&mut app, &mut events, |_, e| {
            matches!(e, VoiceEvent::Final { .. })
        })
        .await;
        assert_eq!(app.input_buffer_for_test(), "fix hej världen");
        assert_eq!(app.input.cursor, "fix hej världen".len());
        assert!(agent_rx.try_recv().is_err(), "dictation never submits");

        let request = seen.recv().await.unwrap();
        assert_eq!(request.path, "/v1/audio/transcriptions");
        assert_eq!(request.authorization, "Bearer key-for-test");
        assert!(request.has("name=\"file\"") && request.has("OggS"));
        assert!(request.has("name=\"model\"") && request.has("qwen/qwen3-asr-0.6b"));
        assert!(request.has("name=\"language\""));

        // Stopping with nothing left open ends cleanly.
        app.dispatch_action(Action::ToggleVoice).await;
        assert_eq!(app.voice.phase, VoicePhase::Finishing);
        pump(&mut app, &mut events, |_, e| {
            matches!(e, VoiceEvent::Stopped(_))
        })
        .await;
        assert_eq!(app.voice.phase, VoicePhase::Idle);
        assert_eq!(app.input_buffer_for_test(), "fix hej världen");
        assert!(agent_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn live_preview_shows_provisional_text_that_the_final_replaces() {
        let (base, _seen) = serve(200, json!({"text": "hej"})).await;
        let (mut app, mut events) = dictating_app(&base, tone(1.5), true);
        app.dispatch_action(Action::ToggleVoice).await;

        pump(&mut app, &mut events, |_, e| {
            matches!(e, VoiceEvent::Partial { .. })
        })
        .await;
        assert_eq!(app.voice.provisional, "hej");
        assert_eq!(
            app.input_buffer_for_test(),
            "",
            "not in the buffer until final"
        );
        assert!(app.voice_status().is_some_and(|s| s.provisional == "hej"));

        app.dispatch_action(Action::ToggleVoice).await;
        pump(&mut app, &mut events, |_, e| {
            matches!(e, VoiceEvent::Stopped(_))
        })
        .await;
        assert_eq!(app.input_buffer_for_test(), "hej");
        assert_eq!(app.voice.provisional, "");
    }

    #[tokio::test]
    async fn live_preview_off_sends_only_the_final_request() {
        let (base, mut seen) = serve(200, json!({"text": "hej"})).await;
        let (mut app, mut events) = dictating_app(&base, tone(2.5), false);
        app.dispatch_action(Action::ToggleVoice).await;
        // Let the session take in the whole signal, then stop it.
        tokio::time::sleep(Duration::from_millis(200)).await;
        app.dispatch_action(Action::ToggleVoice).await;
        pump(&mut app, &mut events, |_, e| {
            matches!(e, VoiceEvent::Stopped(_))
        })
        .await;
        assert_eq!(app.input_buffer_for_test(), "hej");
        seen.recv().await.unwrap();
        assert!(seen.try_recv().is_err(), "one request for one phrase");
    }

    #[tokio::test]
    async fn a_refused_request_stops_listening_with_the_servers_message_as_a_toast() {
        let (base, _seen) = serve(402, json!({"error": {"message": "balance exhausted"}})).await;
        let (mut app, mut events) = dictating_app(&base, [tone(0.8), silence(0.6)].concat(), false);
        app.inject_input("keep");
        app.dispatch_action(Action::ToggleVoice).await;
        pump(&mut app, &mut events, |_, e| {
            matches!(e, VoiceEvent::Error(_))
        })
        .await;
        assert_eq!(app.voice.phase, VoicePhase::Idle);
        assert_eq!(app.input_buffer_for_test(), "keep");
        let toasts = toasts(&app);
        assert!(
            toasts
                .iter()
                .any(|t| t.contains("402") && t.contains("balance exhausted")),
            "{toasts:?}"
        );
    }

    #[tokio::test]
    async fn no_microphone_is_a_toast_not_a_panic() {
        let (base, _seen) = serve(200, json!({"text": "x"})).await;
        let (mut app, mut events) = dictating_app(&base, Vec::new(), false);
        app.voice.test_source = Some(Box::new(Failing));
        app.dispatch_action(Action::ToggleVoice).await;
        pump(&mut app, &mut events, |_, e| {
            matches!(e, VoiceEvent::Error(_))
        })
        .await;
        assert_eq!(app.voice.phase, VoicePhase::Idle);
        assert!(toasts(&app)
            .iter()
            .any(|t| t.contains("no microphone found")));
    }

    #[tokio::test]
    async fn without_a_key_for_the_provider_listening_does_not_start() {
        let (mut app, _events) = dictating_app("http://127.0.0.1:9/v1", Vec::new(), false);
        app.tui_config.voice.provider = Some("nobody".into());
        app.dispatch_action(Action::ToggleVoice).await;
        assert_eq!(app.voice.phase, VoicePhase::Idle);
        assert!(toasts(&app).iter().any(|t| t.contains("nobody")));
    }

    #[tokio::test]
    async fn text_goes_to_the_session_that_was_listening_not_the_one_in_view() {
        let (mut app, _events) = dictating_app("http://127.0.0.1:9/v1", Vec::new(), false);
        let mut other = crate::app::session_manager::SessionEntry::new_blank("other");
        other.stored_input_buffer = Some("draft".into());
        other.stored_input_cursor = Some(5);
        let other_id = other.id.clone();
        let active = app.sessions.active_id.clone();
        app.sessions.add_child_session(active, other);

        app.voice.phase = VoicePhase::Listening;
        app.voice.target = Some(other_id.clone());
        app.inject_input("mine");
        app.handle_voice_event(VoiceEvent::Final {
            seq: 0,
            text: "ö".into(),
        });

        assert_eq!(
            app.input_buffer_for_test(),
            "mine",
            "the session in view is untouched"
        );
        let entry = app.sessions.get(&other_id).unwrap();
        assert_eq!(entry.stored_input_buffer.as_deref(), Some("draft ö"));
        assert!(
            app.voice_status().is_none(),
            "nothing is shown outside its session"
        );
    }

    #[test]
    fn a_late_partial_of_a_finished_phrase_is_ignored() {
        let (mut app, _agent) = App::for_testing();
        app.voice.phase = VoicePhase::Listening;
        app.voice.target = Some(app.sessions.active_id.clone());
        app.handle_voice_event(VoiceEvent::Final {
            seq: 3,
            text: "done".into(),
        });
        app.handle_voice_event(VoiceEvent::Partial {
            seq: 3,
            text: "late".into(),
        });
        assert_eq!(app.voice.provisional, "");
        app.handle_voice_event(VoiceEvent::Partial {
            seq: 4,
            text: "next".into(),
        });
        assert_eq!(app.voice.provisional, "next");
    }
}

#[cfg(test)]
mod insertion_tests {
    use super::*;
    use crate::voice::VoicePhase;

    #[test]
    fn final_text_lands_at_the_cursor_in_multibyte_input_and_clears_completion() {
        let (mut app, mut agent_rx) = App::for_testing();
        app.voice.phase = VoicePhase::Listening;
        app.voice.target = Some(app.sessions.active_id.clone());
        app.inject_input("åäö senare");
        app.input.cursor = "åäö".len();
        app.ui.completion = Some(crate::overlay::completion::CompletionOverlay::new(
            Vec::new(),
        ));

        app.handle_voice_event(VoiceEvent::Final {
            seq: 0,
            text: "världen ".into(),
        });

        assert_eq!(app.input_buffer_for_test(), "åäö världen senare");
        assert_eq!(app.input.cursor, "åäö världen".len());
        assert!(app.ui.completion.is_none());
        assert!(agent_rx.try_recv().is_err(), "nothing is submitted");
    }
}
