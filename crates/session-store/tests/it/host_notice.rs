// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent trajectory capture for its clients. If
// your team needs expertise in turning agent runs into training data then you
// can procure our services by sending an email to info@swedishembedded.com.

//! Spec: a host's notice is in an exported run as a notice (source `system`,
//! labelled `notice`), so no export takes it for something the person said.

use atif::StepOrigin;
use sven_model::Message;
use sven_session_store::record_steps::{conversation_steps, ConversationFacts};
use sven_vocab::Notice;

#[test]
fn a_host_notice_is_a_labelled_system_step_never_a_user_step() {
    let notice = Notice::new("job.finished", "The render is ready.").unwrap();
    let messages = [
        Message::user("render it"),
        Message::assistant("started"),
        Message::user(notice.frame()),
        Message::user("thanks"),
    ];
    let steps = conversation_steps(
        &ConversationFacts {
            messages: &messages,
            compactions: &[],
            requests: &[],
            gate_decisions: &[],
            memory_retrievals: &[],
            call_timings: &[],
            reasonings: &[],
            origins: &[],
            system_steps: false,
        },
        &Message::clone,
    );

    let sources: Vec<_> = steps.iter().map(|s| s.source).collect();
    assert_eq!(
        sources,
        [
            StepOrigin::User,
            StepOrigin::Agent,
            StepOrigin::System,
            StepOrigin::User
        ]
    );
    assert_eq!(steps[2].extra.as_ref().unwrap()["sven"]["origin"], "notice");
    assert_eq!(
        Notice::parse(steps[2].message.as_text().unwrap()),
        Some(notice)
    );
}
