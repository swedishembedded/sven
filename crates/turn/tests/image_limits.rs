// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping multimodal agent
// requests within what the serving model accepts, for its clients. If your
// team needs expertise in multimodal LLM serving then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec: the request sent to a model that states per-request image limits
//! carries the newest images that fit, each older one replaced in place by a
//! note; the conversation the caller holds keeps every image; a model that
//! states no limit gets every image.

use sven_model::catalog::ImageLimits;
use sven_model::{ContentPart, Message, MessageContent, ResponseEvent};
use sven_model_mock::ScriptedMockProvider;
use sven_turn::{stream_turn, TurnLimits};
use tokio::sync::mpsc;

fn png_url(w: u32, h: u32) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let mut bytes = Vec::new();
    image::DynamicImage::new_rgb8(w, h)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    format!("data:image/png;base64,{}", STANDARD.encode(bytes))
}

/// One user message per image, as a conversation with four attachments.
fn conversation(sizes: &[(u32, u32)]) -> Vec<Message> {
    sizes
        .iter()
        .map(|&(w, h)| Message::user_with_parts(vec![ContentPart::image(png_url(w, h))]))
        .collect()
}

/// Sends `messages` through one turn; returns the request the model saw.
async fn sent(provider: &ScriptedMockProvider, messages: Vec<Message>) -> Vec<Message> {
    let (tx, mut rx) = mpsc::channel(64);
    stream_turn(
        provider,
        messages,
        vec![],
        None,
        None,
        None,
        None,
        TurnLimits::default(),
        &tx,
    )
    .await
    .expect("the turn completes");
    drop(tx);
    while rx.recv().await.is_some() {}
    let request = provider.last_request.lock().unwrap().take();
    request.expect("the model was called").messages
}

fn provider(limits: ImageLimits) -> ScriptedMockProvider {
    ScriptedMockProvider::new(vec![vec![ResponseEvent::Done]])
        .with_vision()
        .with_image_limits(limits)
}

fn notes(messages: &[Message]) -> Vec<&str> {
    messages
        .iter()
        .filter_map(|m| match &m.content {
            MessageContent::Text(t) if t.starts_with("[earlier image left out") => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_server_taking_two_images_gets_the_newest_two_and_notes_for_the_rest() {
    let held = conversation(&[(1, 1), (2, 1), (3, 1), (4, 1)]);
    let limits = ImageLimits {
        max_images: Some(2),
        max_pixels: None,
    };
    let request = sent(&provider(limits), held.clone()).await;

    let images = |ms: &[Message]| ms.iter().map(|m| m.image_urls().len()).sum::<usize>();
    assert_eq!(images(&request), 2);
    assert_eq!(request[2].image_urls(), held[2].image_urls());
    assert_eq!(request[3].image_urls(), held[3].image_urls());
    assert_eq!(notes(&request).len(), 2);
    assert_eq!(
        images(&held),
        4,
        "the caller's conversation keeps every image"
    );
}

#[tokio::test]
async fn a_pixel_total_over_the_limit_leaves_out_the_oldest_images() {
    let held = conversation(&[(10, 10), (10, 10), (10, 10)]);
    let limits = ImageLimits {
        max_images: None,
        max_pixels: Some(250),
    };
    let request = sent(&provider(limits), held).await;
    assert_eq!(notes(&request).len(), 1);
    assert!(request[0].image_urls().is_empty());
    assert_eq!(request[2].image_urls().len(), 1);
}

#[tokio::test]
async fn a_server_that_states_no_limit_gets_every_image() {
    let held = conversation(&[(1, 1), (2, 1), (3, 1), (4, 1), (5, 1)]);
    let request = sent(&provider(ImageLimits::default()), held).await;
    assert!(notes(&request).is_empty());
    assert_eq!(
        request.iter().map(|m| m.image_urls().len()).sum::<usize>(),
        5
    );
}
