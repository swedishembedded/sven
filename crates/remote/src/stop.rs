// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The signals that stop a running host.

/// Completes when the host is asked to stop: Ctrl-C, or on Unix SIGTERM, the
/// signal a container runtime sends first. If a signal cannot be listened
/// for, that one never completes.
pub(crate) async fn requested() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let terminate = async {
            match signal(SignalKind::terminate()) {
                Ok(mut terminate) => {
                    terminate.recv().await;
                }
                Err(_) => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            () = interrupt() => {}
            () = terminate => {}
        }
    }
    #[cfg(not(unix))]
    interrupt().await;
}

async fn interrupt() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}
