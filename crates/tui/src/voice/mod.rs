// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for speech input to agent
// interfaces for its clients. If your team needs expertise in voice
// dictation for terminal and web applications then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Voice dictation (Ctrl+G): the events a listening session reports to the
//! UI, and the UI's record of the session.
//!
//! The session itself needs the `voice` feature (microphone, Opus,
//! transcription request); without it only this vocabulary is compiled, so
//! the event loop and the input pane have one shape either way. Nothing
//! produces these events in such a build, hence the `dead_code` allowances.

#[cfg(feature = "voice")]
pub(crate) mod capture;
#[cfg(feature = "voice")]
pub(crate) mod ogg_opus;
#[cfg(feature = "voice")]
pub(crate) mod session;
#[cfg(feature = "voice")]
pub(crate) mod windower;

use sven_session_store::SessionId;
use tokio::sync::mpsc;

/// Cells of the level meter.
pub(crate) const METER_CELLS: usize = 8;

/// Why listening stopped.
#[cfg_attr(not(feature = "voice"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The person stopped it.
    User,
    /// Thirty seconds without speech.
    Silence,
    /// The ten-minute limit.
    Limit,
}

/// What a listening session tells the UI.
#[cfg_attr(not(feature = "voice"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceEvent {
    /// RMS level of the latest stretch of audio.
    Level(f32),
    /// Provisional text of the open phrase `seq`.
    Partial { seq: u64, text: String },
    /// Final text of the closed phrase `seq`. Finals arrive in order.
    Final { seq: u64, text: String },
    /// Listening failed and is over: nothing follows.
    Error(String),
    /// Listening is over and every phrase has been answered.
    Stopped(StopReason),
}

/// Where a dictation is in its life.
#[cfg_attr(not(feature = "voice"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum VoicePhase {
    #[default]
    Idle,
    /// The microphone is open.
    Listening,
    /// The microphone is closed; the last phrases are being transcribed.
    Finishing,
}

/// The UI's record of the dictation in progress.
#[derive(Default)]
pub(crate) struct VoiceState {
    pub phase: VoicePhase,
    /// The session whose input receives the text: the one that was active
    /// when listening started.
    pub target: Option<SessionId>,
    /// Latest levels, oldest first, for the meter.
    pub levels: [f32; METER_CELLS],
    /// Text of the open phrase, shown dimmed and not yet in the buffer.
    pub provisional: String,
    /// Highest phrase already final, so a late partial of it is ignored.
    pub last_final: Option<u64>,
    /// Where a session reports; set by the event loop.
    pub tx: Option<mpsc::Sender<VoiceEvent>>,
    #[cfg(feature = "voice")]
    pub handle: Option<session::VoiceHandle>,
    /// Stands in for the microphone in tests.
    #[cfg(all(test, feature = "voice"))]
    pub test_source: Option<Box<dyn session::VoiceSource>>,
}

impl VoiceState {
    /// Whether the input pane shows a dictation.
    pub fn is_active(&self) -> bool {
        self.phase != VoicePhase::Idle
    }

    /// Whether the meter moves.
    pub fn is_listening(&self) -> bool {
        self.phase == VoicePhase::Listening
    }

    pub fn push_level(&mut self, rms: f32) {
        self.levels.rotate_left(1);
        self.levels[METER_CELLS - 1] = rms;
    }

    /// Back to idle; whatever was provisional is dropped.
    pub fn reset(&mut self) {
        self.phase = VoicePhase::Idle;
        self.target = None;
        self.levels = [0.0; METER_CELLS];
        self.provisional.clear();
        self.last_final = None;
        #[cfg(feature = "voice")]
        {
            self.handle = None;
        }
    }
}
