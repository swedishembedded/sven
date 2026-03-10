// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! File watcher for chat YAML files using inotify (via notify crate).
//!
//! Watches the active chat YAML file and signals when it has been modified
//! externally (e.g., by another sven process). This allows the TUI to reload
//! the conversation when the file changes.

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

/// File change event sent to the app when the watched file is modified.
#[derive(Debug)]
pub enum FileChangeEvent {
    /// The watched file was modified externally.
    Modified(PathBuf),
    /// An error occurred while watching.
    Error(String),
}

/// File watcher that monitors a chat YAML file for external changes.
pub struct ChatFileWatcher {
    /// The notify watcher handle.
    watcher: Option<RecommendedWatcher>,
    /// Receiver for file change events.
    receiver: Receiver<Result<Event, notify::Error>>,
    /// The currently watched file path.
    watched_path: Option<PathBuf>,
}

impl ChatFileWatcher {
    /// Create a new chat file watcher.
    pub fn new() -> Self {
        let (tx, rx) = channel();

        // Create a watcher with a reasonable poll interval
        let watcher = RecommendedWatcher::new(
            move |res| {
                let _ = tx.send(res);
            },
            Config::default().with_poll_interval(Duration::from_secs(1)),
        )
        .ok();

        Self {
            watcher,
            receiver: rx,
            watched_path: None,
        }
    }

    /// Start watching a chat YAML file. If already watching a different file,
    /// it will be unwatched first.
    pub fn watch(&mut self, path: &PathBuf) -> Result<(), String> {
        // Stop watching the previous file if any
        if let (Some(watcher), Some(old_path)) = (self.watcher.as_ref(), self.watched_path.as_ref())
        {
            if watcher.watch(old_path, RecursiveMode::NonRecursive).is_ok() {
                // Ignore unwatch errors - file might have been deleted
            }
        }

        // Watch the new file
        if let Some(watcher) = self.watcher.as_ref() {
            watcher
                .watch(path, RecursiveMode::NonRecursive)
                .map_err(|e| format!("failed to watch file: {}", e))?;
            self.watched_path = Some(path.clone());
        }

        Ok(())
    }

    /// Stop watching the current file.
    pub fn unwatch(&mut self) {
        if let (Some(watcher), Some(path)) = (self.watcher.as_ref(), self.watched_path.take()) {
            let _ = watcher.unwatch(&path);
        }
    }

    /// Check if there are any pending file change events.
    /// Returns the latest modification event if the file was modified.
    pub fn check_for_changes(&self) -> Option<FileChangeEvent> {
        // Drain all events and return the most recent modification
        let mut latest_mod: Option<PathBuf> = None;

        while let Ok(result) = self.receiver.try_recv() {
            match result {
                Ok(event) => {
                    // Only care about modify events on files
                    if matches!(event.kind, notify::EventKind::Modify(_)) {
                        for path in event.paths {
                            // Ignore temporary files (.yaml.tmp)
                            if let Some(ext) = path.extension() {
                                if ext == "tmp" {
                                    continue;
                                }
                            }
                            latest_mod = Some(path);
                        }
                    }
                }
                Err(e) => {
                    return Some(FileChangeEvent::Error(e.to_string()));
                }
            }
        }

        latest_mod.map(FileChangeEvent::Modified)
    }
}

impl Default for ChatFileWatcher {
    fn default() -> Self {
        Self::new()
    }
}
