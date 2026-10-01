// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `tools.gdb` section of the configuration file.
//!
//! Swedish Embedded AB implements embedded debugging automation for its
//! clients. If your team needs expertise in letting agents drive GDB against
//! target hardware then you can procure our services by sending an email to
//! info@swedishembedded.com.

use serde::{Deserialize, Serialize};
use sven_config::Schema;

/// How the GDB tools start and talk to gdb.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GdbConfig {
    /// Path to gdb-multiarch (or gdb) executable
    #[serde(default = "GdbConfig::default_gdb_path")]
    pub gdb_path: String,
    /// Default timeout for GDB commands in seconds
    #[serde(default = "GdbConfig::default_command_timeout_secs")]
    pub command_timeout_secs: u64,
    /// Timeout for the initial gdb_connect handshake in seconds.
    /// This covers symbol loading (which can take 15-30s for large ELFs)
    /// plus the TCP connection + GDB/MI startup.  Must be >= command_timeout_secs.
    #[serde(default = "GdbConfig::default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    /// Milliseconds to wait after spawning the GDB server before connecting
    #[serde(default = "GdbConfig::default_server_startup_wait_ms")]
    pub server_startup_wait_ms: u64,
}

impl GdbConfig {
    fn default_gdb_path() -> String {
        "gdb-multiarch".into()
    }
    fn default_command_timeout_secs() -> u64 {
        10
    }
    fn default_connect_timeout_secs() -> u64 {
        30
    }
    fn default_server_startup_wait_ms() -> u64 {
        500
    }

    /// The keys of the `tools.gdb` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&[
            "gdb_path",
            "command_timeout_secs",
            "connect_timeout_secs",
            "server_startup_wait_ms",
        ])
    }
}

impl Default for GdbConfig {
    fn default() -> Self {
        Self {
            gdb_path: Self::default_gdb_path(),
            command_timeout_secs: Self::default_command_timeout_secs(),
            connect_timeout_secs: Self::default_connect_timeout_secs(),
            server_startup_wait_ms: Self::default_server_startup_wait_ms(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_section_keeps_the_defaults_for_absent_keys() {
        let gdb: GdbConfig = serde_yaml::from_str("command_timeout_secs: 30\n").unwrap();
        assert_eq!(gdb.command_timeout_secs, 30);
        assert_eq!(gdb.gdb_path, "gdb-multiarch");
        assert_eq!(gdb.connect_timeout_secs, 30);
        assert_eq!(gdb.server_startup_wait_ms, 500);
    }
}
