// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The SQLite adapter against the shared conformance suite.
//!
//! Swedish Embedded AB implements solutions for verifiable agent memory
//! storage for its clients. If your team needs expertise in proving that a
//! database adapter keeps tenants apart then you can procure our services by
//! sending an email to info@swedishembedded.com.

use sven_memory_sqlite::SqliteMemory;

sven_memory_api::memory_conformance_tests!(|| async {
    SqliteMemory::open_in_memory().expect("open an in-memory store")
});
