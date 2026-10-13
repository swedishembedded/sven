// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec: the Brain adapter speaks the recorded `POST /v1/run` wire, maps every
//! failure to a decision error class, honours the deadline, bounds its
//! concurrency, caches versioned decisions and discovers models without
//! loading them.

mod contract;
mod diagnostics;
mod support;
mod transport;
