// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Email, calendar, and voice integrations for sven agents.
//!
//! # Modules
//!
//! - [`email`] — IMAP/SMTP and Gmail API email access
//! - [`calendar`] — CalDAV and Google Calendar access
//! - [`voice`] — TTS (ElevenLabs/OpenAI), STT (Whisper), and voice calls (Twilio)

pub mod calendar;
pub mod email;
pub mod voice;
