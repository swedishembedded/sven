// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for speech input to agent
// interfaces for its clients. If your team needs expertise in live
// transcription pipelines then you can procure our services by sending an
// email to info@swedishembedded.com.
//! One listening session: audio from a [`VoiceSource`] through the
//! [`Windower`], each phrase encoded as Ogg/Opus and transcribed, the results
//! reported as [`VoiceEvent`]s.
//!
//! Ownership: the session task owns the microphone and the windower. Final
//! phrases go through one worker, so their texts arrive in phrase order and
//! the load on the server stays at one final request at a time; a partial is
//! its own short task, at most one per phrase in flight (the windower sees
//! to that).

use anyhow::Context;
use tokio::sync::{mpsc, oneshot};

use super::windower::{WindowEvent, Windower};
use super::{ogg_opus, StopReason, VoiceEvent};

/// Level reports are sent every this many frames (80 ms), as the loudest of
/// them, so the UI is not flooded at 50 per second.
const LEVEL_EVERY_FRAMES: u8 = 4;

/// Where audio comes from: 16 kHz mono f32 in chunks of any size.
pub(crate) trait VoiceSource: Send + 'static {
    /// Opens the device and starts delivering audio.
    ///
    /// # Errors
    /// If there is no device or it cannot be opened.
    fn start(&mut self) -> anyhow::Result<Capture>;
}

/// An open source. Dropping it closes the device.
pub(crate) struct Capture {
    frames: mpsc::Receiver<Vec<f32>>,
    /// Whatever keeps the device open; released with the capture.
    _device: Box<dyn Send>,
}

impl Capture {
    pub(crate) fn new(frames: mpsc::Receiver<Vec<f32>>, device: Box<dyn Send>) -> Self {
        Self {
            frames,
            _device: device,
        }
    }
}

/// Where and how to transcribe.
#[derive(Debug, Clone)]
pub(crate) struct Settings {
    /// OpenAI-style base URL, ending in `/v1`.
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub language: Option<String>,
    /// Send the open phrase about once a second for provisional text.
    pub live_preview: bool,
}

/// A running session. Dropping the handle cancels it: the microphone closes
/// and unanswered phrases are abandoned. [`Self::stop`] ends it gracefully.
pub(crate) struct VoiceHandle {
    stop: oneshot::Sender<()>,
}

impl VoiceHandle {
    /// Stops listening; phrases already heard are still transcribed and
    /// reported, then [`VoiceEvent::Stopped`] follows.
    pub(crate) fn stop(self) {
        let _ = self.stop.send(());
    }
}

/// Starts a session on the current tokio runtime.
pub(crate) fn spawn(
    source: Box<dyn VoiceSource>,
    settings: Settings,
    events: mpsc::Sender<VoiceEvent>,
) -> VoiceHandle {
    let (stop, stop_rx) = oneshot::channel();
    tokio::spawn(run(source, settings, events, stop_rx));
    VoiceHandle { stop }
}

async fn run(
    mut source: Box<dyn VoiceSource>,
    settings: Settings,
    events: mpsc::Sender<VoiceEvent>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut capture = match source.start() {
        Ok(capture) => capture,
        Err(e) => {
            let _ = events.send(VoiceEvent::Error(format!("{e:#}"))).await;
            return;
        }
    };
    let settings = std::sync::Arc::new(settings);
    let (finals_tx, finals_rx) = mpsc::unbounded_channel();
    let (partial_done_tx, mut partial_done) = mpsc::unbounded_channel();
    let worker = tokio::spawn(final_worker(finals_rx, settings.clone(), events.clone()));
    let mut windower = Windower::new(settings.live_preview);
    let mut reporter = Reporter {
        settings,
        events: events.clone(),
        finals: finals_tx,
        partial_done: partial_done_tx,
        peak: 0.0,
        frames: 0,
    };

    let reason = loop {
        tokio::select! {
            chunk = capture.frames.recv() => match chunk {
                Some(chunk) => {
                    if let Some(reason) = reporter.report(windower.push(&chunk)) {
                        break Some(reason);
                    }
                }
                // The device went away: flush what was heard.
                None => break reporter.report(windower.stop()),
            },
            asked = &mut stop => match asked {
                Ok(()) => break reporter.report(windower.stop()),
                Err(_) => break None,
            },
            Some(()) = partial_done.recv() => windower.partial_done(),
        }
    };

    // The microphone is done either way; closing the queue lets the worker
    // finish the phrases already handed to it.
    drop(capture);
    drop(reporter);
    let Some(reason) = reason else {
        worker.abort();
        return;
    };
    if worker.await.unwrap_or(false) {
        let _ = events.send(VoiceEvent::Stopped(reason)).await;
    }
}

/// Turns windower events into requests and UI events.
struct Reporter {
    settings: std::sync::Arc<Settings>,
    events: mpsc::Sender<VoiceEvent>,
    finals: mpsc::UnboundedSender<(u64, Vec<f32>)>,
    partial_done: mpsc::UnboundedSender<()>,
    peak: f32,
    frames: u8,
}

impl Reporter {
    /// Acts on `window_events`; the stop reason if one of them is it.
    fn report(&mut self, window_events: Vec<WindowEvent>) -> Option<StopReason> {
        let mut stopped = None;
        for event in window_events {
            match event {
                WindowEvent::Level(rms) => {
                    self.peak = self.peak.max(rms);
                    self.frames += 1;
                    if self.frames == LEVEL_EVERY_FRAMES {
                        // A full queue only costs a frame of the meter.
                        let _ = self.events.try_send(VoiceEvent::Level(self.peak));
                        self.peak = 0.0;
                        self.frames = 0;
                    }
                }
                WindowEvent::Partial { seq, samples } => {
                    let settings = self.settings.clone();
                    let events = self.events.clone();
                    let done = self.partial_done.clone();
                    tokio::spawn(async move {
                        let result = transcribe(&settings, samples).await;
                        let _ = done.send(());
                        let event = match result {
                            Ok(text) => VoiceEvent::Partial { seq, text },
                            Err(e) => VoiceEvent::Error(format!("{e:#}")),
                        };
                        let _ = events.send(event).await;
                    });
                }
                WindowEvent::Phrase { seq, samples } => {
                    let _ = self.finals.send((seq, samples));
                }
                WindowEvent::Stopped(reason) => stopped = Some(reason),
            }
        }
        stopped
    }
}

/// Transcribes final phrases one after the other; false if one failed.
async fn final_worker(
    mut phrases: mpsc::UnboundedReceiver<(u64, Vec<f32>)>,
    settings: std::sync::Arc<Settings>,
    events: mpsc::Sender<VoiceEvent>,
) -> bool {
    while let Some((seq, samples)) = phrases.recv().await {
        match transcribe(&settings, samples).await {
            Ok(text) => {
                if events.send(VoiceEvent::Final { seq, text }).await.is_err() {
                    return false;
                }
            }
            Err(e) => {
                let _ = events.send(VoiceEvent::Error(format!("{e:#}"))).await;
                return false;
            }
        }
    }
    true
}

async fn transcribe(settings: &Settings, samples: Vec<f32>) -> anyhow::Result<String> {
    let ogg = tokio::task::spawn_blocking(move || ogg_opus::encode(&samples))
        .await
        .context("encoder task")??;
    sven_model_drivers::transcribe(
        &settings.base_url,
        &settings.api_key,
        &settings.model,
        settings.language.as_deref(),
        ogg,
    )
    .await
}
