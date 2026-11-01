// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for speech input to agent
// interfaces for its clients. If your team needs expertise in turning a
// continuous microphone signal into phrases for speech recognition then you
// can procure our services by sending an email to info@swedishembedded.com.
//! Cuts a continuous 16 kHz mono signal into phrases for transcription.
//!
//! Pure and device-free: samples go in, [`WindowEvent`]s come out. The
//! constants and rules are the normative windowing of the voice-input plan,
//! the same as the browser's, so both clients cut the same signal the same
//! way. One refinement the table leaves open: the noise floor is calibrated
//! on the first 10 frames that are not already speech by [`SPEECH_FLOOR`]
//! alone, so a recording that opens in mid-word does not take its own voice
//! for background noise.

use std::collections::VecDeque;

use super::StopReason;

/// Sample rate the windower expects.
pub const SAMPLE_RATE: usize = 16_000;
/// Level and speech are decided per frame of this length.
pub const FRAME_MS: usize = 20;
/// Samples in one frame.
pub const FRAME_SAMPLES: usize = SAMPLE_RATE * FRAME_MS / 1000;
/// A frame is speech if its RMS is above max(this, 3 x noise floor).
pub const SPEECH_FLOOR: f32 = 0.01;
/// A phrase starts this long before its first speech frame.
pub const PRE_ROLL_MS: usize = 200;
/// Less speech than this in a phrase is dropped, never sent.
pub const MIN_SPEECH_MS: usize = 300;
/// This much consecutive silence after speech closes the phrase, keeping the
/// first [`PRE_ROLL_MS`] of it.
pub const END_SILENCE_MS: usize = 400;
/// A phrase this long is cut at the quietest frame of its last second.
pub const MAX_PHRASE_MS: usize = 8000;
/// With live preview on, the open phrase so far is offered this often.
pub const PARTIAL_EVERY_MS: usize = 1000;
/// This long without a speech frame stops listening.
pub const AUTO_STOP_SILENCE_MS: usize = 30_000;
/// Listening stops after this long.
pub const MAX_SESSION_MS: usize = 600_000;

/// Frames of calibration for the noise floor.
const CALIBRATION_FRAMES: usize = 10;
/// Weight of a new non-speech frame in the noise floor.
const NOISE_ALPHA: f32 = 0.05;
/// Speech is a frame above this many times the noise floor.
const NOISE_MARGIN: f32 = 3.0;
/// The window the quietest frame is looked for when a phrase is cut.
const CUT_WINDOW_MS: usize = 1000;

const fn frames(ms: usize) -> usize {
    ms / FRAME_MS
}

/// What the windower reports.
#[derive(Debug, Clone, PartialEq)]
pub enum WindowEvent {
    /// RMS of one frame, for the level meter.
    Level(f32),
    /// The open phrase so far; its text is provisional.
    Partial { seq: u64, samples: Vec<f32> },
    /// A closed phrase; its text is final.
    Phrase { seq: u64, samples: Vec<f32> },
    /// Listening ended; nothing follows.
    Stopped(StopReason),
}

#[derive(Clone)]
struct Frame {
    samples: Vec<f32>,
    rms: f32,
    speech: bool,
}

/// The phrase being collected.
struct Open {
    frames: Vec<Frame>,
    /// Length of the phrase, in frames, at its last partial tick.
    ticked_at: usize,
}

impl Open {
    /// A phrase opened by `frames`, the last of which is its first speech.
    fn opened_by(frames: Vec<Frame>) -> Self {
        Self {
            ticked_at: frames.len() - 1,
            frames,
        }
    }

    /// The remainder of a cut phrase, which ticks from its start.
    fn continued_by(frames: Vec<Frame>) -> Self {
        Self {
            frames,
            ticked_at: 0,
        }
    }

    fn speech_frames(&self) -> usize {
        self.frames.iter().filter(|f| f.speech).count()
    }

    fn trailing_silence(&self) -> usize {
        self.frames.iter().rev().take_while(|f| !f.speech).count()
    }

    fn has_enough_speech(&self) -> bool {
        self.speech_frames() >= frames(MIN_SPEECH_MS)
    }

    fn samples(&self) -> Vec<f32> {
        self.frames
            .iter()
            .flat_map(|f| f.samples.iter().copied())
            .collect()
    }
}

/// Windows a signal into phrases. See the module documentation.
pub struct Windower {
    live_preview: bool,
    /// Samples short of a whole frame.
    carry: Vec<f32>,
    frames_seen: usize,
    /// Sum and count of the quiet calibration frames.
    calibration: (f32, usize),
    noise_floor: f32,
    /// The latest frames before a phrase, kept for its pre-roll.
    pre_roll: VecDeque<Frame>,
    open: Option<Open>,
    /// Sequence number of the open phrase, or of the next one.
    seq: u64,
    partial_in_flight: bool,
    silent_frames: usize,
    stopped: bool,
}

impl Windower {
    /// A windower; `live_preview` enables partials.
    #[must_use]
    pub fn new(live_preview: bool) -> Self {
        Self {
            live_preview,
            carry: Vec::new(),
            frames_seen: 0,
            calibration: (0.0, 0),
            noise_floor: 0.0,
            pre_roll: VecDeque::new(),
            open: None,
            seq: 0,
            partial_in_flight: false,
            silent_frames: 0,
            stopped: false,
        }
    }

    /// Takes in more of the signal.
    pub fn push(&mut self, samples: &[f32]) -> Vec<WindowEvent> {
        let mut events = Vec::new();
        if self.stopped {
            return events;
        }
        self.carry.extend_from_slice(samples);
        let mut start = 0;
        while !self.stopped && self.carry.len() - start >= FRAME_SAMPLES {
            let frame = self.carry[start..start + FRAME_SAMPLES].to_vec();
            start += FRAME_SAMPLES;
            self.frame(frame, &mut events);
        }
        self.carry.drain(..start);
        events
    }

    /// Stops on the person's request, flushing the open phrase.
    pub fn stop(&mut self) -> Vec<WindowEvent> {
        let mut events = Vec::new();
        self.finish(StopReason::User, &mut events);
        events
    }

    /// The request for the last partial has finished, so another may be sent.
    pub fn partial_done(&mut self) {
        self.partial_in_flight = false;
    }

    fn frame(&mut self, samples: Vec<f32>, events: &mut Vec<WindowEvent>) {
        let rms = rms(&samples);
        let speech = self.classify(rms);
        events.push(WindowEvent::Level(rms));
        self.frames_seen += 1;
        self.silent_frames = if speech { 0 } else { self.silent_frames + 1 };
        let frame = Frame {
            samples,
            rms,
            speech,
        };

        match self.open.take() {
            None if speech => {
                let mut frames: Vec<Frame> = self.pre_roll.drain(..).collect();
                frames.push(frame);
                self.open = Some(Open::opened_by(frames));
                self.partial_in_flight = false;
            }
            None => self.remember(frame),
            Some(mut open) => {
                open.frames.push(frame);
                self.open = Some(open);
                self.advance_open(events);
            }
        }

        if self.silent_frames >= frames(AUTO_STOP_SILENCE_MS) {
            self.finish(StopReason::Silence, events);
        } else if self.frames_seen >= frames(MAX_SESSION_MS) {
            self.finish(StopReason::Limit, events);
        }
    }

    /// Whether a frame of this RMS is speech; learns the noise floor.
    fn classify(&mut self, rms: f32) -> bool {
        let speech = rms > SPEECH_FLOOR.max(NOISE_MARGIN * self.noise_floor);
        if self.frames_seen < CALIBRATION_FRAMES {
            if rms <= SPEECH_FLOOR {
                self.calibration.0 += rms;
                self.calibration.1 += 1;
            }
            if self.frames_seen + 1 == CALIBRATION_FRAMES && self.calibration.1 > 0 {
                self.noise_floor = self.calibration.0 / self.calibration.1 as f32;
            }
        } else if !speech {
            self.noise_floor += NOISE_ALPHA * (rms - self.noise_floor);
        }
        speech
    }

    fn remember(&mut self, frame: Frame) {
        self.pre_roll.push_back(frame);
        while self.pre_roll.len() > frames(PRE_ROLL_MS) {
            self.pre_roll.pop_front();
        }
    }

    /// Closes, cuts or previews the open phrase after a frame joined it.
    fn advance_open(&mut self, events: &mut Vec<WindowEvent>) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        if open.trailing_silence() >= frames(END_SILENCE_MS) {
            let mut open = self.open.take().expect("open");
            // Keep the first PRE_ROLL_MS of the silence, hand the rest on as
            // the pre-roll of whatever comes next.
            let kept = open.frames.len() - open.trailing_silence() + frames(PRE_ROLL_MS);
            let tail = open.frames.split_off(kept);
            self.pre_roll = tail.into_iter().collect();
            while self.pre_roll.len() > frames(PRE_ROLL_MS) {
                self.pre_roll.pop_front();
            }
            self.close(open, events);
            return;
        }
        if open.frames.len() >= frames(MAX_PHRASE_MS) {
            let mut open = self.open.take().expect("open");
            let window = frames(CUT_WINDOW_MS).min(open.frames.len());
            let first = open.frames.len() - window;
            let mut cut = first;
            for (i, f) in open.frames.iter().enumerate().skip(first) {
                if f.rms < open.frames[cut].rms {
                    cut = i;
                }
            }
            let rest = open.frames.split_off(cut);
            self.close(open, events);
            self.resume(rest);
            return;
        }
        if open.frames.len() - open.ticked_at >= frames(PARTIAL_EVERY_MS) {
            open.ticked_at = open.frames.len();
            if self.live_preview && open.has_enough_speech() && !self.partial_in_flight {
                self.partial_in_flight = true;
                events.push(WindowEvent::Partial {
                    seq: self.seq,
                    samples: open.samples(),
                });
            }
        }
    }

    /// The remainder of a cut phrase: it opens the next phrase if it holds
    /// speech, and is pre-roll otherwise.
    fn resume(&mut self, rest: Vec<Frame>) {
        if rest.iter().any(|f| f.speech) {
            self.open = Some(Open::continued_by(rest));
        } else {
            self.pre_roll = rest.into_iter().collect();
            while self.pre_roll.len() > frames(PRE_ROLL_MS) {
                self.pre_roll.pop_front();
            }
        }
    }

    /// Ends the phrase: a final if it has enough speech, nothing otherwise.
    fn close(&mut self, open: Open, events: &mut Vec<WindowEvent>) {
        let seq = self.seq;
        self.seq += 1;
        self.partial_in_flight = false;
        if open.has_enough_speech() {
            events.push(WindowEvent::Phrase {
                seq,
                samples: open.samples(),
            });
        }
    }

    fn finish(&mut self, reason: StopReason, events: &mut Vec<WindowEvent>) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        if let Some(mut open) = self.open.take() {
            let silence = open.trailing_silence();
            let drop = silence.saturating_sub(frames(PRE_ROLL_MS));
            open.frames.truncate(open.frames.len() - drop);
            self.close(open, events);
        }
        events.push(WindowEvent::Stopped(reason));
    }
}

fn rms(samples: &[f32]) -> f32 {
    let energy: f32 = samples.iter().map(|s| s * s).sum();
    (energy / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(seconds: f32) -> Vec<f32> {
        (0..(seconds * SAMPLE_RATE as f32) as usize)
            .map(|i| 0.3 * (std::f32::consts::TAU * 440.0 * i as f32 / SAMPLE_RATE as f32).sin())
            .collect()
    }

    fn silence(seconds: f32) -> Vec<f32> {
        vec![0.0; (seconds * SAMPLE_RATE as f32) as usize]
    }

    /// Feeds `signal` in odd-sized chunks, as a device would.
    fn run(w: &mut Windower, signal: &[f32]) -> Vec<WindowEvent> {
        signal.chunks(441).flat_map(|c| w.push(c)).collect()
    }

    fn phrases(events: &[WindowEvent]) -> Vec<&Vec<f32>> {
        events
            .iter()
            .filter_map(|e| match e {
                WindowEvent::Phrase { samples, .. } => Some(samples),
                _ => None,
            })
            .collect()
    }

    fn partials(events: &[WindowEvent]) -> Vec<(u64, usize)> {
        events
            .iter()
            .filter_map(|e| match e {
                WindowEvent::Partial { seq, samples } => Some((*seq, samples.len())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn burst_pause_burst_is_two_final_phrases() {
        let mut w = Windower::new(false);
        let signal = [tone(0.6), silence(0.6), tone(0.6)].concat();
        let mut events = run(&mut w, &signal);
        events.extend(w.stop());
        let found = phrases(&events);
        assert_eq!(found.len(), 2, "{events:?}");
        // Each holds its 0.6 s burst plus at most the 200 ms of silence
        // around it; neither takes in the whole pause.
        for p in found {
            let seconds = p.len() as f32 / SAMPLE_RATE as f32;
            assert!((0.6..=1.01).contains(&seconds), "{seconds}");
        }
        assert_eq!(events.last(), Some(&WindowEvent::Stopped(StopReason::User)));
    }

    #[test]
    fn the_first_phrase_closes_on_silence_without_waiting_for_stop() {
        let mut w = Windower::new(false);
        let events = run(&mut w, &[tone(0.6), silence(0.5)].concat());
        assert_eq!(phrases(&events).len(), 1);
    }

    #[test]
    fn continuous_speech_is_cut_at_eight_seconds_and_loses_nothing() {
        let mut w = Windower::new(false);
        let signal = tone(20.0);
        let mut events = run(&mut w, &signal);
        events.extend(w.stop());
        let found = phrases(&events);
        assert!(found.len() >= 3);
        for p in &found {
            assert!(p.len() <= MAX_PHRASE_MS * SAMPLE_RATE / 1000);
        }
        let total: usize = found.iter().map(|p| p.len()).sum();
        assert_eq!(total, signal.len());
        let seqs: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                WindowEvent::Phrase { seq, .. } => Some(*seq),
                _ => None,
            })
            .collect();
        assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<_>>());
    }

    #[test]
    fn forty_seconds_of_silence_stops_at_thirty_with_no_phrase() {
        let mut w = Windower::new(true);
        let events = run(&mut w, &silence(40.0));
        assert!(phrases(&events).is_empty() && partials(&events).is_empty());
        assert_eq!(
            events.last(),
            Some(&WindowEvent::Stopped(StopReason::Silence))
        );
        let levels = events
            .iter()
            .filter(|e| matches!(e, WindowEvent::Level(_)))
            .count();
        assert_eq!(levels, AUTO_STOP_SILENCE_MS / FRAME_MS, "stopped at 30 s");
        assert!(w.push(&tone(1.0)).is_empty(), "nothing after stopped");
    }

    #[test]
    fn a_session_ends_at_the_limit_even_while_speech_continues() {
        let mut w = Windower::new(false);
        let events = run(&mut w, &tone(601.0));
        assert_eq!(
            events.last(),
            Some(&WindowEvent::Stopped(StopReason::Limit))
        );
        let total: usize = phrases(&events).iter().map(|p| p.len()).sum();
        assert_eq!(total, MAX_SESSION_MS * SAMPLE_RATE / 1000);
    }

    #[test]
    fn a_blip_shorter_than_the_minimum_is_dropped() {
        let mut w = Windower::new(false);
        let mut events = run(&mut w, &[silence(0.5), tone(0.2), silence(1.0)].concat());
        events.extend(w.stop());
        assert!(phrases(&events).is_empty());
    }

    #[test]
    fn a_phrase_starts_with_pre_roll() {
        let mut w = Windower::new(false);
        let mut events = run(&mut w, &[silence(1.0), tone(0.6)].concat());
        events.extend(w.stop());
        let found = phrases(&events);
        assert_eq!(found.len(), 1);
        let seconds = found[0].len() as f32 / SAMPLE_RATE as f32;
        assert!(
            (0.79..=0.81).contains(&seconds),
            "0.6 s + 0.2 s pre-roll: {seconds}"
        );
    }

    #[test]
    fn partials_come_every_second_once_there_is_enough_speech_when_enabled() {
        let mut w = Windower::new(true);
        let mut seen = Vec::new();
        for chunk in tone(3.5).chunks(SAMPLE_RATE / 10) {
            let events = w.push(chunk);
            if !partials(&events).is_empty() {
                w.partial_done();
            }
            seen.extend(partials(&events));
        }
        let rate = SAMPLE_RATE;
        assert_eq!(seen, vec![(0, rate), (0, 2 * rate), (0, 3 * rate)]);
    }

    #[test]
    fn no_partials_with_live_preview_off() {
        let mut w = Windower::new(false);
        assert!(partials(&run(&mut w, &tone(3.5))).is_empty());
    }

    #[test]
    fn a_partial_in_flight_is_skipped_never_queued() {
        let mut w = Windower::new(true);
        let events = run(&mut w, &tone(3.5));
        assert_eq!(
            partials(&events).len(),
            1,
            "no partial_done, no more partials"
        );
        w.partial_done();
        let later = run(&mut w, &tone(1.0));
        assert_eq!(partials(&later).len(), 1);
    }

    #[test]
    fn stopping_flushes_the_open_phrase() {
        let mut w = Windower::new(false);
        assert!(phrases(&run(&mut w, &tone(1.0))).is_empty());
        let events = w.stop();
        assert_eq!(phrases(&events).len(), 1);
        assert_eq!(events.last(), Some(&WindowEvent::Stopped(StopReason::User)));
    }
}
