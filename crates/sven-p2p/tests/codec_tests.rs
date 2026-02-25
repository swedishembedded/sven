//! CBOR round-trip tests for every wire-protocol type.
//!
//! Each test encodes a value to CBOR bytes and decodes it back, asserting
//! that the result is byte-for-byte equal to the original.

use sven_p2p::protocol::{
    codec::{cbor_decode, cbor_encode},
    types::{
        AgentCard, ContentBlock, LogEntry, P2pRequest, P2pResponse, TaskRequest, TaskResponse,
        TaskStatus,
    },
};
use uuid::Uuid;

fn roundtrip<T>(value: &T) -> T
where
    T: serde::Serialize + for<'de> serde::Deserialize<'de> + std::fmt::Debug + PartialEq,
{
    let bytes = cbor_encode(value).expect("encode");
    let decoded: T = cbor_decode(&bytes).expect("decode");
    decoded
}

// ── AgentCard ─────────────────────────────────────────────────────────────────

#[test]
fn agent_card_roundtrip() {
    let card = AgentCard {
        peer_id: "12D3KooWTest".into(),
        name: "alice".into(),
        description: "general purpose Rust agent".into(),
        capabilities: vec!["rust".into(), "electrical".into()],
        version: "0.1.0".into(),
    };
    assert_eq!(card, roundtrip(&card));
}

#[test]
fn agent_card_empty_capabilities() {
    let card = AgentCard {
        peer_id: "peer1".into(),
        name: "minimal".into(),
        description: String::new(),
        capabilities: vec![],
        version: "0.1.0".into(),
    };
    assert_eq!(card, roundtrip(&card));
}

// ── ContentBlock ──────────────────────────────────────────────────────────────

#[test]
fn content_block_text_roundtrip() {
    let block = ContentBlock::text("Hello, world!");
    assert_eq!(block, roundtrip(&block));
}

#[test]
fn content_block_text_unicode() {
    let block = ContentBlock::text("こんにちは 🎉");
    assert_eq!(block, roundtrip(&block));
}

#[test]
fn content_block_image_roundtrip() {
    let data: Vec<u8> = (0u8..=255).collect();
    let block = ContentBlock::Image {
        data: data.clone(),
        mime_type: "image/png".into(),
        detail: Some("high".into()),
    };
    let decoded = roundtrip(&block);
    match &decoded {
        ContentBlock::Image { data: d, mime_type, detail } => {
            assert_eq!(d, &data, "binary data must survive CBOR round-trip byte-for-byte");
            assert_eq!(mime_type, "image/png");
            assert_eq!(detail.as_deref(), Some("high"));
        }
        _ => panic!("wrong variant: {decoded:?}"),
    }
}

#[test]
fn content_block_image_no_detail() {
    let block = ContentBlock::Image {
        data: vec![0x89, 0x50, 0x4e, 0x47],
        mime_type: "image/png".into(),
        detail: None,
    };
    assert_eq!(block, roundtrip(&block));
}

#[test]
fn content_block_json_roundtrip() {
    let block = ContentBlock::json(serde_json::json!({
        "tool": "shell",
        "args": ["ls", "-la"],
        "nested": { "count": 42 }
    }));
    assert_eq!(block, roundtrip(&block));
}

#[test]
fn content_block_json_null() {
    let block = ContentBlock::json(serde_json::Value::Null);
    assert_eq!(block, roundtrip(&block));
}

// ── TaskRequest ───────────────────────────────────────────────────────────────

#[test]
fn task_request_roundtrip() {
    let req = TaskRequest::new(
        "room1",
        "Implement a BCD counter in VHDL",
        vec![
            ContentBlock::text("Use synchronous reset. Clock at 10 MHz."),
            ContentBlock::json(serde_json::json!({ "target": "xilinx-7series" })),
        ],
    );
    assert_eq!(req, roundtrip(&req));
}

#[test]
fn task_request_preserves_uuid() {
    let id = Uuid::new_v4();
    let req = TaskRequest {
        id,
        originator_room: "test".into(),
        description: "test".into(),
        payload: vec![],
    };
    let decoded = roundtrip(&req);
    assert_eq!(decoded.id, id);
}

#[test]
fn task_request_with_image_payload() {
    let image_data: Vec<u8> = (0..512).map(|i| (i % 256) as u8).collect();
    let req = TaskRequest::new(
        "vision-room",
        "Describe this schematic",
        vec![
            ContentBlock::text("What components do you see?"),
            ContentBlock::Image {
                data: image_data.clone(),
                mime_type: "image/jpeg".into(),
                detail: None,
            },
        ],
    );
    let decoded = roundtrip(&req);
    match &decoded.payload[1] {
        ContentBlock::Image { data, .. } => {
            assert_eq!(data, &image_data, "image bytes must be identical after CBOR round-trip");
        }
        _ => panic!("expected Image block"),
    }
}

// ── TaskResponse ──────────────────────────────────────────────────────────────

#[test]
fn task_response_completed() {
    let req_id = Uuid::new_v4();
    let resp = TaskResponse {
        request_id: req_id,
        agent: AgentCard {
            peer_id: "p1".into(),
            name: "ee-agent".into(),
            description: "electrical engineer".into(),
            capabilities: vec!["pcb".into()],
            version: "0.1.0".into(),
        },
        result: vec![ContentBlock::text("Here is the BCD counter implementation…")],
        status: TaskStatus::Completed,
        duration_ms: 4200,
    };
    let decoded = roundtrip(&resp);
    assert_eq!(decoded.request_id, req_id);
    assert_eq!(decoded.duration_ms, 4200);
    assert!(matches!(decoded.status, TaskStatus::Completed));
}

#[test]
fn task_response_failed() {
    let resp = TaskResponse {
        request_id: Uuid::new_v4(),
        agent: AgentCard {
            peer_id: "p2".into(),
            name: "agent".into(),
            description: String::new(),
            capabilities: vec![],
            version: "0.1.0".into(),
        },
        result: vec![],
        status: TaskStatus::Failed { reason: "context limit exceeded".into() },
        duration_ms: 0,
    };
    let decoded = roundtrip(&resp);
    match decoded.status {
        TaskStatus::Failed { reason } => assert_eq!(reason, "context limit exceeded"),
        _ => panic!("expected Failed status"),
    }
}

#[test]
fn task_response_partial() {
    let resp = TaskResponse {
        request_id: Uuid::new_v4(),
        agent: AgentCard {
            peer_id: "p3".into(),
            name: "a".into(),
            description: String::new(),
            capabilities: vec![],
            version: "0.1.0".into(),
        },
        result: vec![ContentBlock::text("partial output…")],
        status: TaskStatus::Partial,
        duration_ms: 1000,
    };
    assert!(matches!(roundtrip(&resp).status, TaskStatus::Partial));
}

// ── P2pRequest / P2pResponse envelopes ────────────────────────────────────────

#[test]
fn p2p_request_announce() {
    let card = AgentCard {
        peer_id: "abc".into(),
        name: "bob".into(),
        description: "plumber".into(),
        capabilities: vec!["pipes".into()],
        version: "0.1.0".into(),
    };
    let req = P2pRequest::Announce(card.clone());
    match roundtrip(&req) {
        P2pRequest::Announce(c) => assert_eq!(c, card),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn p2p_request_task() {
    let task = TaskRequest::new("r", "do something", vec![ContentBlock::text("hello")]);
    let req = P2pRequest::Task(task.clone());
    match roundtrip(&req) {
        P2pRequest::Task(t) => assert_eq!(t, task),
        _ => panic!("wrong variant"),
    }
}

#[test]
fn p2p_response_ack() {
    let resp = P2pResponse::Ack;
    assert!(matches!(roundtrip(&resp), P2pResponse::Ack));
}

#[test]
fn p2p_response_task_result() {
    let task_resp = TaskResponse {
        request_id: Uuid::new_v4(),
        agent: AgentCard {
            peer_id: "x".into(),
            name: "x".into(),
            description: String::new(),
            capabilities: vec![],
            version: "0.1.0".into(),
        },
        result: vec![ContentBlock::text("done")],
        status: TaskStatus::Completed,
        duration_ms: 10,
    };
    let resp = P2pResponse::TaskResult(task_resp.clone());
    match roundtrip(&resp) {
        P2pResponse::TaskResult(r) => assert_eq!(r, task_resp),
        _ => panic!("wrong variant"),
    }
}

// ── LogEntry ──────────────────────────────────────────────────────────────────

#[test]
fn log_entry_roundtrip() {
    let entry = LogEntry {
        level: "WARN".into(),
        target: "sven_p2p::node".into(),
        message: "connection refused".into(),
    };
    let decoded: LogEntry = cbor_decode(&cbor_encode(&entry).unwrap()).unwrap();
    assert_eq!(decoded.level, entry.level);
    assert_eq!(decoded.target, entry.target);
    assert_eq!(decoded.message, entry.message);
}

// ── Encode determinism ────────────────────────────────────────────────────────

#[test]
fn same_value_encodes_identically() {
    let card = AgentCard {
        peer_id: "x".into(),
        name: "y".into(),
        description: "z".into(),
        capabilities: vec!["a".into()],
        version: "1.0".into(),
    };
    let a = cbor_encode(&card).unwrap();
    let b = cbor_encode(&card).unwrap();
    assert_eq!(a, b, "CBOR encoding must be deterministic for the same value");
}
