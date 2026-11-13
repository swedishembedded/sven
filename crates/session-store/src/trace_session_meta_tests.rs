// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `trace_session`'s session metadata: sven's own metadata in
//! a trajectory, model and usage fields, session ids, parent and child
//! linkage, and the agent version. Split from `trace_session_tests.rs` for
//! size; included via `#[path]` in `trace_session.rs`.

use super::*;

// ── SvenSessionMeta round-trip ──────────────────────────────────────────

#[test]
fn sven_session_meta_round_trips_through_extra() {
    let agent = default_agent_profile();
    let mut trajectory = Trajectory::new(ATIF_SCHEMA_VERSION, agent);
    let mut meta = SvenSessionMeta::new("My session");
    meta.status = ChatStatus::Completed;
    meta.mode = Some("code".to_string());
    meta.parent_session_id = Some("parent-123".to_string());
    meta.apply_to_trajectory(&mut trajectory);

    let restored = SvenSessionMeta::from_trajectory(&trajectory).expect("meta present");
    assert_eq!(restored, meta);
}

#[test]
fn sven_session_meta_absent_returns_none() {
    let trajectory = Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile());
    assert!(SvenSessionMeta::from_trajectory(&trajectory).is_none());
}

#[test]
fn sven_session_meta_preserves_other_extra_keys() {
    let mut trajectory = Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile());
    trajectory.extra = Some(serde_json::json!({ "other_tool": { "foo": "bar" } }));
    let meta = SvenSessionMeta::new("Title");
    meta.apply_to_trajectory(&mut trajectory);

    let extra = trajectory.extra.as_ref().unwrap();
    assert_eq!(extra["other_tool"]["foo"], "bar");
    assert!(SvenSessionMeta::from_trajectory(&trajectory).is_some());
}

#[test]
fn sven_session_meta_json_shape_is_nested_object() {
    let mut trajectory = Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile());
    SvenSessionMeta::new("Title").apply_to_trajectory(&mut trajectory);
    let extra = trajectory.extra.as_ref().unwrap();
    assert!(
        extra.get("sven").is_some(),
        "must nest under a single `sven` key"
    );
    assert_eq!(extra["sven"]["title"], "Title");
}

// ── model / usage native-field mapping ──────────────────────────────────

#[test]
fn model_maps_to_agent_model_name() {
    let agent = default_agent_profile().with_model("anthropic/claude-sonnet-4-20250514");
    assert_eq!(
        agent.model_name.as_deref(),
        Some("anthropic/claude-sonnet-4-20250514")
    );
}

#[test]
fn chat_usage_maps_to_final_metrics_native_fields() {
    let usage = ChatUsage {
        total_input_tokens: 1234,
        total_output_tokens: 567,
        total_cache_read_tokens: 100,
        total_cache_write_tokens: 200,
        total_cost_usd: 0.042,
    };
    let metrics = chat_usage_to_final_metrics(&usage);
    assert_eq!(metrics.total_prompt_tokens, Some(1234));
    assert_eq!(metrics.total_completion_tokens, Some(567));
    assert_eq!(metrics.total_cached_tokens, Some(100));
    assert!((metrics.total_cost_usd.unwrap() - 0.042).abs() < 1e-9);
    assert_eq!(metrics.extra.unwrap()["total_cache_write_tokens"], 200);
}

#[test]
fn chat_usage_final_metrics_round_trip() {
    let usage = ChatUsage {
        total_input_tokens: 10,
        total_output_tokens: 20,
        total_cache_read_tokens: 5,
        total_cache_write_tokens: 7,
        total_cost_usd: 1.5,
    };
    let metrics = chat_usage_to_final_metrics(&usage);
    let back = final_metrics_to_chat_usage(&metrics);
    assert_eq!(back, usage);
}

#[test]
fn chat_usage_zero_cache_write_omits_extra() {
    let usage = ChatUsage {
        total_cache_write_tokens: 0,
        ..Default::default()
    };
    let metrics = chat_usage_to_final_metrics(&usage);
    assert!(metrics.extra.is_none());
}

// ── session id ────────────────────────────────────────────────────────

#[test]
fn new_session_id_is_nonempty_uuid_like() {
    let id = new_session_id();
    assert_eq!(id.len(), 36);
    assert!(id.chars().filter(|c| *c == '-').count() == 4);
}

#[test]
fn new_session_id_is_unique() {
    assert_ne!(new_session_id(), new_session_id());
}

// ── parent/child linkage ─────────────────────────────────────────────

#[test]
fn parent_session_id_round_trips_on_child() {
    let mut child = Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile());
    let mut meta = SvenSessionMeta::new("Subagent task");
    meta.parent_session_id = Some("root-session-1".to_string());
    meta.apply_to_trajectory(&mut child);

    let restored = SvenSessionMeta::from_trajectory(&child).unwrap();
    assert_eq!(
        restored.parent_session_id.as_deref(),
        Some("root-session-1")
    );
}

// ── agent.version ─────────────────────────────────────────────────────

#[test]
fn default_agent_profile_reports_the_real_sven_binary_version_not_a_frozen_crate_version() {
    // Guards against this crate carrying its own frozen `version = "1.0.0"`:
    // `crates/session-store` inherits `version.workspace = true` from the
    // root Cargo.toml's `[workspace.package].version`, so
    // `env!("CARGO_PKG_VERSION")` here always matches the sven release
    // version stamped into every ATIF trajectory.
    let profile = default_agent_profile();
    assert_ne!(
        profile.version, "1.0.0",
        "agent.version is a frozen crate version, not the sven release version"
    );
    assert_eq!(profile.version, env!("CARGO_PKG_VERSION"));
}
