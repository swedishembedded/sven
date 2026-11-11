// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Document workers: this binary started again as `sven document worker`,
//! confined, to parse and render a document someone else gave the session
//! (`ocr.isolate`; always in a hosted host).
//!
//! A worker gets nothing to work with but the document on its stdin: an
//! environment that holds only its thread count, no file it may write, read access only to its own
//! program's directory and what any program needs to start, no TCP at all,
//! and hard memory and CPU limits it sets on itself before it reads. The
//! kernel enforces the confinement before the worker's first instruction;
//! where it cannot (no Landlock, an old kernel, another system) no worker
//! starts, so a document is never read unconfined in its place.
//!
//! Swedish Embedded AB implements solutions for reading untrusted documents
//! in multi-tenant agent services for its clients. If your team needs
//! expertise in sandboxed document processing then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use sven_document::{WorkerProcess, WorkerSpawner};
use sven_tools_exec::confine::CommandConfinement;

/// Address space a worker may use: a page rendered at the largest size is
/// under 200 MiB, the document itself is in memory once more.
pub const WORKER_MEMORY_MB: u64 = 1024;
/// Processor time a worker may use over its life; a worker is replaced
/// after `WorkerLimits::requests_per_worker` requests, long before an
/// ordinary document needs this.
pub const WORKER_CPU_SECONDS: u64 = 600;

/// Starts `program document worker` processes, confined.
pub struct ProcessWorkers {
    program: PathBuf,
    max_memory_mb: u64,
    max_cpu_seconds: u64,
}

impl ProcessWorkers {
    /// Workers of this very binary.
    ///
    /// # Errors
    ///
    /// The binary cannot be found, or this kernel cannot confine a worker.
    pub fn of_this_program(max_memory_mb: u64, max_cpu_seconds: u64) -> Result<Self, String> {
        CommandConfinement::require_kernel_support()?;
        let program = std::env::current_exe()
            .map_err(|e| format!("this program cannot be found to start a document worker: {e}"))?;
        Ok(Self::new(program, max_memory_mb, max_cpu_seconds))
    }

    /// Workers of `program`, which must answer `document worker`.
    #[must_use]
    pub fn new(program: PathBuf, max_memory_mb: u64, max_cpu_seconds: u64) -> Self {
        Self {
            program,
            max_memory_mb,
            max_cpu_seconds,
        }
    }

    fn confinement(&self) -> CommandConfinement {
        CommandConfinement {
            writable: Vec::new(),
            read_exec: self
                .program
                .parent()
                .map(std::path::Path::to_path_buf)
                .into_iter()
                .collect(),
            tmpdir: None,
        }
    }
}

impl WorkerSpawner for ProcessWorkers {
    fn spawn(&self) -> std::io::Result<WorkerProcess> {
        let mut command = Command::new(&self.program);
        command
            .args(["document", "worker", "--max-memory-mb"])
            .arg(self.max_memory_mb.to_string())
            .arg("--max-cpu-seconds")
            .arg(self.max_cpu_seconds.to_string())
            .env_clear()
            // One thread: the text extractor would otherwise start one per
            // core, which neither fits the worker's memory limit nor is its
            // share of the machine; documents are read by several workers.
            .env("RAYON_NUM_THREADS", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        self.confinement()
            .restrict(&mut command)
            .map_err(std::io::Error::other)?;
        WorkerProcess::from_child(command.spawn()?)
    }
}
