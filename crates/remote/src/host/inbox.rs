// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements solutions for running agent sessions on a
// person's own machine for a server for its clients. If your team needs
// expertise in remotely driven agents then you can procure our services by
// sending an email to info@swedishembedded.com.

//! The server's commands for this host: read from its stream, each once,
//! handed to the router, and acknowledged only once the router has it, so a
//! command is never acknowledged and then left undone.

use std::time::Duration;

use futures::StreamExt;
use sven_control::ControlCommand;
use tokio::sync::mpsc;

use super::upload::Upload;
use super::MAX_BACKOFF;
use crate::client::{EnvironmentTaken, RemoteClient, TokenRefused, UnreadableCommand};

pub(super) async fn command_loop(
    client: &RemoteClient,
    routed: &mpsc::Sender<ControlCommand>,
    uploads: &mpsc::Sender<Upload>,
) -> anyhow::Result<()> {
    let mut taken = 0u64;
    let mut backoff = Duration::from_millis(500);
    loop {
        match client.commands(taken).await {
            Ok(stream) => {
                backoff = Duration::from_millis(500);
                futures::pin_mut!(stream);
                while let Some(item) = stream.next().await {
                    let envelope = match item {
                        Ok(envelope) => envelope,
                        // Sent again from the same place on every reconnect, so
                        // a command that cannot be read is passed over, not
                        // retried: it would wedge this host for good.
                        Err(e) => match e.downcast_ref::<UnreadableCommand>() {
                            Some(UnreadableCommand { seq: Some(seq), .. }) => {
                                tracing::warn!("skipping command {seq}: {e:#}");
                                taken = taken.max(*seq);
                                let _ = uploads.send(Upload::Ack(taken)).await;
                                continue;
                            }
                            _ => {
                                tracing::warn!("command stream: {e:#}");
                                break;
                            }
                        },
                    };
                    // At-least-once delivery: a command already taken is
                    // sent again after a reconnect and dropped here.
                    if envelope.seq <= taken {
                        continue;
                    }
                    // Acknowledged only once the router has it: one that has
                    // stopped carries out nothing, so the host stops too,
                    // and the server sends the command to the next one.
                    if routed.send(envelope.command).await.is_err() {
                        anyhow::bail!("the host stopped carrying out commands");
                    }
                    taken = envelope.seq;
                    let _ = uploads.send(Upload::Ack(taken)).await;
                }
            }
            // Signed out, or replaced by another `sven rc` here: retrying
            // cannot help, and two hosts must never run one environment.
            Err(e)
                if e.downcast_ref::<TokenRefused>().is_some()
                    || e.downcast_ref::<EnvironmentTaken>().is_some() =>
            {
                return Err(e)
            }
            Err(e) => tracing::warn!("command stream: {e:#}"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server that sends `body` for the commands after `after=0`, then
    /// `rest` for every later ask; what each ask's `after` was.
    fn serve(body: &'static str, rest: &'static str) -> (String, mpsc::UnboundedReceiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        let (asked, asked_rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            loop {
                let (mut conn, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = conn.read(&mut buf).await.unwrap();
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let after = request
                    .split("after=")
                    .nth(1)
                    .unwrap()
                    .split(['&', ' '])
                    .next()
                    .unwrap()
                    .to_string();
                let _ = asked.send(after.clone());
                let data = if after == "0" { body } else { rest };
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
                let _ = conn
                    .write_all(format!("{head}data: {data}\n\n").as_bytes())
                    .await;
            }
        });
        (base, asked_rx)
    }

    const CANCEL: &str = r#"{"seq":2,"command":{"type":"cancel_session","session_id":"00000000-0000-0000-0000-000000000000"}}"#;

    /// A command this host cannot read is passed over: the server sends it
    /// again from the same place on every reconnect, so retrying it would
    /// keep the host from ever reaching the commands after it.
    #[tokio::test]
    async fn a_command_that_cannot_be_read_does_not_wedge_the_command_loop() {
        let poison = r#"{"seq":1,"command":{"type":"from_the_future"}}"#;
        let (base, mut asked_rx) = serve(poison, CANCEL);
        let client = RemoteClient::new(&format!("{base}/api"), "token").unwrap();
        let (routed, mut routed_rx) = mpsc::channel(8);
        let (uploads, mut uploads_rx) = mpsc::channel(8);
        let looping = tokio::spawn(async move { command_loop(&client, &routed, &uploads).await });
        let command = tokio::time::timeout(Duration::from_secs(10), routed_rx.recv())
            .await
            .expect("the command after the unreadable one arrives")
            .unwrap();
        assert!(matches!(command, ControlCommand::CancelSession { .. }));
        assert_eq!(asked_rx.recv().await.unwrap(), "0");
        assert_eq!(
            asked_rx.recv().await.unwrap(),
            "1",
            "it resumed after the skipped command"
        );
        assert!(matches!(uploads_rx.recv().await, Some(Upload::Ack(1))));
        looping.abort();
    }

    /// A command the router can no longer take is never acknowledged: the
    /// host stops, saying so, and the server keeps the command for the
    /// host that takes over.
    #[tokio::test]
    async fn a_command_is_acknowledged_only_once_the_router_has_it() {
        let (base, _asked) = serve(CANCEL, CANCEL);
        let client = RemoteClient::new(&format!("{base}/api"), "token").unwrap();
        let (routed, routed_rx) = mpsc::channel(8);
        drop(routed_rx);
        let (uploads, mut uploads_rx) = mpsc::channel(8);
        let ended = tokio::time::timeout(
            Duration::from_secs(10),
            command_loop(&client, &routed, &uploads),
        )
        .await
        .expect("the host stops");
        assert!(format!("{:#}", ended.unwrap_err()).contains("stopped carrying out commands"));
        assert!(uploads_rx.try_recv().is_err(), "nothing acknowledged");
    }
}
