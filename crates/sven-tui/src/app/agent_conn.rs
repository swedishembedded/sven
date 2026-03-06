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
    /// Context window usage for the last turn (0–100 %), relative to the
    /// usable input budget (`max_tokens − max_output_tokens`).
    pub context_pct: u8,
    /// Exact input token count for the last turn (provider-reported).
    /// Equals `input_tokens + cache_read_tokens + cache_write_tokens`.
    pub context_tokens: u32,
    /// Exact output token count for the last turn (provider-reported).
    /// Zero until the provider's usage event arrives for this turn.
    pub output_tokens: u32,
    /// Cache-hit rate for the last turn (0–100 %).
    pub cache_hit_pct: u8,
    /// Live approximate output token count for the current turn (chars/4).
    /// Used only for visual animation while the model is generating and the
    /// exact output count has not yet been reported by the provider.
    /// Reset to 0 on TurnComplete / Aborted.
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
    /// Clock-driven animation frame counter, incremented every ~80 ms by the
    /// main event loop tick when the agent is busy.  Unlike `spinner_frame`
    /// (which is event-driven and reflects streaming speed), `anim_frame`
    /// advances at a steady rate regardless of how fast events arrive.
    pub anim_frame: u8,
}

impl AgentConn {
    pub fn new() -> Self {
        Self {
            busy: false,
            current_tool: None,
            context_pct: 0,
            context_tokens: 0,
            output_tokens: 0,
            cache_hit_pct: 0,
            streaming_tokens: 0,
            spinner_frame: 0,
            cancel: Arc::new(tokio::sync::Mutex::new(None)),
            tx: None,
            event_rx: None,
            tool_start_times: HashMap::new(),
            anim_frame: 0,
        }
    }
}
