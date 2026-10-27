// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping one conversation in
// step across a terminal, a browser and a server for its clients. If your
// team needs expertise in collaborative data and CRDTs then you can procure
// our services by sending an email to info@swedishembedded.com.
//! The shared conversation document.
//!
//! One conversation is open in several places at once - the terminal running
//! the session, any number of browser tabs, the server that keeps it - and
//! any of them may add to it, online or not. Each holds a replica of this
//! document; replicas exchange updates in any order, any number of times, and
//! all reach the same messages in the same order. The merge is `yrs` (the
//! Rust Yjs); this crate is the schema on top of it, so every replica - native
//! sven, the browser build, a server that loads the browser build - agrees on
//! what a conversation is.
//!
//! # Schema
//!
//! * `order`: an array of message ids, in conversation order. Concurrent
//!   additions from two replicas are both kept, in an order every replica
//!   agrees on.
//! * `messages`: a map of message id to a map of `role`, `author`, `state`
//!   and `parts` (the message's content as JSON, in whatever part vocabulary
//!   the application uses).
//!
//! A message's content is written whole, at checkpoints (an answer when it
//! ends, or every so often while it streams), never per token: per-token
//! edits would make every replica store every token forever.
//!
//! # What this does not decide
//!
//! Convergence is not authority. Two replicas can both add a message and
//! both changes survive; whether a replica was allowed to make a change, and
//! in which order an agent acts on messages, are decided by whoever accepts
//! updates (the server) and runs the session, not by the merge.

use serde::{Deserialize, Serialize};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{
    Array, ArrayRef, Doc, Map, MapPrelim, MapRef, Out, ReadTxn, StateVector, Transact, Update,
};

const ORDER: &str = "order";
const MESSAGES: &str = "messages";

/// A replica's view of one message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocMessage {
    pub id: String,
    /// `user`, `assistant`, or another role the application uses.
    pub role: String,
    /// Who wrote it: a person's id, or the agent.
    pub author: String,
    /// `streaming`, `complete`, `failed`, or another state the application uses.
    pub state: String,
    /// The content, as JSON text.
    pub parts: String,
}

/// An update that is not one this document can apply.
#[derive(Debug, thiserror::Error)]
#[error("not a conversation update: {0}")]
pub struct BadUpdate(String);

/// One replica of a conversation.
pub struct ConversationDoc {
    doc: Doc,
    order: ArrayRef,
    messages: MapRef,
}

impl Default for ConversationDoc {
    fn default() -> Self {
        Self::new()
    }
}

impl ConversationDoc {
    /// An empty replica with a random replica id.
    #[must_use]
    pub fn new() -> Self {
        Self::from_doc(Doc::new())
    }

    /// An empty replica with replica id `client_id`. Two replicas must never
    /// share one; tests use this to make orderings reproducible.
    #[must_use]
    pub fn with_client_id(client_id: u64) -> Self {
        Self::from_doc(Doc::with_client_id(client_id))
    }

    fn from_doc(doc: Doc) -> Self {
        let order = doc.get_or_insert_array(ORDER);
        let messages = doc.get_or_insert_map(MESSAGES);
        Self {
            doc,
            order,
            messages,
        }
    }

    /// Adds `message` at the end of the conversation as this replica sees it.
    /// A message id already present is updated instead, never added twice.
    pub fn add_message(&self, message: &DocMessage) {
        let mut txn = self.doc.transact_mut();
        if self.messages.get(&txn, &message.id).is_some() {
            drop(txn);
            self.update_message(&message.id, &message.state, &message.parts);
            return;
        }
        self.messages.insert(
            &mut txn,
            message.id.as_str(),
            MapPrelim::from([
                ("role", message.role.clone()),
                ("author", message.author.clone()),
                ("state", message.state.clone()),
                ("parts", message.parts.clone()),
            ]),
        );
        self.order.push_back(&mut txn, message.id.as_str());
    }

    /// Replaces message `id`'s state and content. Whether it was there or not,
    /// nothing else changes.
    pub fn update_message(&self, id: &str, state: &str, parts: &str) {
        let mut txn = self.doc.transact_mut();
        if let Some(Out::YMap(message)) = self.messages.get(&txn, id) {
            message.insert(&mut txn, "state", state);
            message.insert(&mut txn, "parts", parts);
        }
    }

    /// The conversation as this replica sees it, in order. An id in the order
    /// whose message has not arrived yet is skipped until it does.
    #[must_use]
    pub fn messages(&self) -> Vec<DocMessage> {
        let txn = self.doc.transact();
        let mut seen = std::collections::HashSet::new();
        self.order
            .iter(&txn)
            .filter_map(|out| match out {
                Out::Any(yrs::Any::String(id)) => Some(id.to_string()),
                _ => None,
            })
            .filter(|id| seen.insert(id.clone()))
            .filter_map(|id| {
                let Some(Out::YMap(message)) = self.messages.get(&txn, &id) else {
                    return None;
                };
                let field = |name: &str| match message.get(&txn, name) {
                    Some(Out::Any(yrs::Any::String(text))) => Some(text.to_string()),
                    _ => None,
                };
                Some(DocMessage {
                    id,
                    role: field("role")?,
                    author: field("author")?,
                    state: field("state")?,
                    parts: field("parts")?,
                })
            })
            .collect()
    }

    /// What this replica has, compactly: send it to another replica to ask
    /// for what it lacks.
    #[must_use]
    pub fn state_vector(&self) -> Vec<u8> {
        self.doc.transact().state_vector().encode_v1()
    }

    /// Everything this replica has that a replica with `state_vector` lacks
    /// (everything, for an empty one).
    ///
    /// # Errors
    ///
    /// `state_vector` is not one.
    pub fn diff(&self, state_vector: &[u8]) -> Result<Vec<u8>, BadUpdate> {
        let remote = if state_vector.is_empty() {
            StateVector::default()
        } else {
            StateVector::decode_v1(state_vector).map_err(|e| BadUpdate(e.to_string()))?
        };
        Ok(self.doc.transact().encode_diff_v1(&remote))
    }

    /// Merges an update from another replica. Applying one twice, or in any
    /// order relative to others, changes nothing further.
    ///
    /// # Errors
    ///
    /// `update` is not one.
    pub fn apply(&self, update: &[u8]) -> Result<(), BadUpdate> {
        let update = Update::decode_v1(update).map_err(|e| BadUpdate(e.to_string()))?;
        self.doc
            .transact_mut()
            .apply_update(update)
            .map_err(|e| BadUpdate(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, role: &str, text: &str) -> DocMessage {
        DocMessage {
            id: id.into(),
            role: role.into(),
            author: if role == "user" {
                "user_ada".into()
            } else {
                "agent".into()
            },
            state: "complete".into(),
            parts: serde_json::json!([{"type": "text", "text": text}]).to_string(),
        }
    }

    /// Everything `from` has that `to` lacks, applied to `to`.
    fn sync(from: &ConversationDoc, to: &ConversationDoc) {
        to.apply(&from.diff(&to.state_vector()).unwrap()).unwrap();
    }

    #[test]
    fn replicas_that_wrote_at_the_same_time_reach_the_same_conversation() {
        let terminal = ConversationDoc::with_client_id(1);
        let browser = ConversationDoc::with_client_id(2);
        terminal.add_message(&message("m1", "user", "list files"));
        sync(&terminal, &browser);

        // Offline from each other, both add a message.
        terminal.add_message(&message("m2", "assistant", "README.md"));
        browser.add_message(&message("m3", "user", "and the tests?"));
        sync(&terminal, &browser);
        sync(&browser, &terminal);

        assert_eq!(terminal.messages(), browser.messages());
        let ids: Vec<String> = terminal.messages().into_iter().map(|m| m.id).collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(ids[0], "m1", "what both had before stays first");
    }

    #[test]
    fn updates_arriving_in_any_order_and_any_number_of_times_converge() {
        let writer = ConversationDoc::with_client_id(7);
        let mut updates = Vec::new();
        let mut before = writer.state_vector();
        for (i, text) in ["one", "two", "three", "four"].iter().enumerate() {
            writer.add_message(&message(&format!("m{i}"), "user", text));
            updates.push(writer.diff(&before).unwrap());
            before = writer.state_vector();
        }
        writer.update_message("m1", "failed", "[]");
        updates.push(writer.diff(&before).unwrap());

        // Every rotation, reversed, and each update delivered twice.
        for start in 0..updates.len() {
            for reverse in [false, true] {
                let reader = ConversationDoc::with_client_id(100 + start as u64);
                let mut order: Vec<&Vec<u8>> = updates
                    .iter()
                    .cycle()
                    .skip(start)
                    .take(updates.len())
                    .collect();
                if reverse {
                    order.reverse();
                }
                for update in order.iter().chain(order.iter()) {
                    reader.apply(update).unwrap();
                }
                assert_eq!(
                    reader.messages(),
                    writer.messages(),
                    "start {start}, reversed {reverse}"
                );
            }
        }
    }

    #[test]
    fn an_answer_is_written_at_checkpoints_and_ends_complete_everywhere() {
        let server = ConversationDoc::with_client_id(1);
        let tab = ConversationDoc::with_client_id(2);
        server.add_message(&message("m1", "user", "hi"));
        server.add_message(&DocMessage {
            state: "streaming".into(),
            ..message("m2", "assistant", "Hel")
        });
        sync(&server, &tab);
        server.update_message(
            "m2",
            "complete",
            &serde_json::json!([{"type": "text", "text": "Hello."}]).to_string(),
        );
        sync(&server, &tab);
        let answer = tab.messages().pop().unwrap();
        assert_eq!(answer.state, "complete");
        assert!(answer.parts.contains("Hello."));
    }

    #[test]
    fn a_message_is_never_added_twice_and_garbage_is_refused() {
        let doc = ConversationDoc::with_client_id(1);
        doc.add_message(&message("m1", "user", "hi"));
        doc.add_message(&DocMessage {
            state: "failed".into(),
            ..message("m1", "user", "hi")
        });
        assert_eq!(doc.messages().len(), 1);
        assert_eq!(doc.messages()[0].state, "failed");
        assert!(doc.apply(b"not an update").is_err());
        assert!(doc.diff(b"\xff\xff").is_err());
        assert_eq!(
            doc.diff(&[]).unwrap(),
            doc.diff(&ConversationDoc::new().state_vector()).unwrap()
        );
    }
}
