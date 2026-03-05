// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Agent connection state: channels, cancellation handle, and run-time metrics.

use std::{collections::HashMap, sync::Arc, time::Instant};

use sven_core::AgentEvent;
use tokio::sync::mpsc;

use crate::agent::AgentRequest;

/// State for the background agent task connection.
pub(crate) struct AgentConn {
    /// True while the agent is processing a turn.
    pub busy: bool,
    /// Name of the tool currently executing (shown in the status bar).
    pub current_tool: Option<String>,
    /// Context window usage for the last turn (0–100 %).
    pub context_pct: u8,
    /// Cache-hit rate for the last turn (0–100 %).
    pub cache_hit_pct: u8,
    /// Total tokens streamed in the current turn (reset on TurnComplete / Aborted).
    pub streaming_tokens: u32,
    /// Spinner frame index (0–9), incremented on each TextDelta event.
    pub spinner_frame: u8,
    /// Shared cancel handle: sending on this oneshot cancels the running turn.
    pub cancel: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    /// Channel to send requests to the agent background task.
    pub tx: Option<mpsc::Sender<AgentRequest>>,
    /// Channel to receive events from the agent background task.
    pub event_rx: Option<mpsc::Receiver<AgentEvent>>,
    /// Wall-clock start time for each in-progress tool call, keyed by call_id.
    pub tool_start_times: HashMap<String, Instant>,
}

impl AgentConn {
    pub fn new() -> Self {
        Self {
            busy: false,
            current_tool: None,
            context_pct: 0,
            cache_hit_pct: 0,
            streaming_tokens: 0,
            spinner_frame: 0,
            cancel: Arc::new(tokio::sync::Mutex::new(None)),
            tx: None,
            event_rx: None,
            tool_start_times: HashMap::new(),
        }
    }
}
