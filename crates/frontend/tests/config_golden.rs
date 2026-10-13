// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The configuration file format, pinned.
//!
//! Every file under `tests/fixtures/config/` - the examples the documentation
//! gives, the configurations the end-to-end suite writes, and files built to
//! exercise the loader's warnings - is loaded the way `sven` loads it, and the
//! result is compared with the golden record beside it: the effective
//! configuration, in the on-disk layout `sven show-config` prints, and every
//! warning the load logged. A change to how a file is read, defaulted,
//! expanded or reported shows up here as a diff against that record.
//!
//! `--bless` as a command-line argument (`make generate/config/golden`)
//! rewrites the records; review the diff before keeping it. This is a
//! `harness = false` binary so that switch is a real flag; the libtest
//! arguments `cargo test` forwards are ignored.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;

/// One fixture loaded: the effective configuration and the warnings logged.
fn load_fixture(path: &Path) -> Value {
    let logged = Arc::new(Mutex::new(Vec::<u8>::new()));
    let writer = {
        let logged = Arc::clone(&logged);
        move || LogWriter(Arc::clone(&logged))
    };
    let subscriber = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_max_level(tracing::Level::WARN)
        .without_time()
        .with_ansi(false)
        .with_target(false)
        .finish();
    let config = tracing::subscriber::with_default(subscriber, || {
        sven_frontend::Settings::load(Some(path)).expect("the fixture loads")
    });
    let logged = String::from_utf8(logged.lock().unwrap().clone()).expect("utf-8 log");
    // The fixture's own location is the one thing about it that differs
    // between checkouts.
    let dir = path
        .parent()
        .expect("a fixture directory")
        .display()
        .to_string();
    // Sorted: some are logged walking a hash map, in no fixed order.
    let mut warnings: Vec<String> = logged
        .lines()
        .map(|line| line.trim().replace(&dir, "<fixtures>"))
        .collect();
    warnings.sort();
    let loaded = serde_json::json!({
        "config": serde_json::to_value(&config).expect("the configuration serializes"),
        "warnings": warnings,
    });
    // Through text, as the record is: a 32-bit float widened in memory does
    // not compare equal to the same number read back.
    serde_json::from_str(&loaded.to_string()).expect("json round-trips")
}

struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("the fixture directory")
        .map(|entry| entry.expect("a fixture").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
        .collect();
    files.sort();
    files
}

/// Only the fixture is read: no user, system or project configuration, no
/// model detected from the environment, and the variables the fixtures
/// expand set to known values. One test in this binary, so the process
/// environment it sets is its own.
fn isolate_environment(home: &Path) {
    std::env::set_var("HOME", home);
    std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
    std::env::set_current_dir(home).expect("an empty working directory");
    std::env::set_var("SVEN_DISABLE_BRAIN_AUTODETECT", "1");
    for key in [
        "OPENROUTER_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "BRAIN_API_KEY",
        "BRAIN_API_KEYS_FILE",
        "SVEN_GOLDEN_UNSET",
        "SVEN_GOLDEN_ALSO_UNSET",
    ] {
        std::env::remove_var(key);
    }
    std::env::set_var("SVEN_GOLDEN_MODEL", "openrouter/auto");
}

fn main() {
    let bless = std::env::args().skip(1).any(|arg| arg == "--bless");
    every_config_fixture_loads_as_recorded(bless);
}

fn every_config_fixture_loads_as_recorded(bless: bool) {
    let home = tempfile::tempdir().expect("a temporary home");
    isolate_environment(home.path());

    let mut mismatched = Vec::new();
    for fixture in fixtures() {
        let loaded = load_fixture(&fixture);
        let golden = fixture.with_extension("golden.json");
        if bless {
            let text = serde_json::to_string_pretty(&loaded).expect("json") + "\n";
            std::fs::write(&golden, text).expect("write the golden record");
            continue;
        }
        let recorded: Value = serde_json::from_str(
            &std::fs::read_to_string(&golden)
                .unwrap_or_else(|e| panic!("{}: {e}", golden.display())),
        )
        .expect("a golden record is json");
        if !same_configuration(&recorded, &loaded) {
            mismatched.push(format!(
                "{}\n--- recorded\n{}\n--- loaded\n{}",
                fixture.display(),
                serde_json::to_string_pretty(&recorded).unwrap(),
                serde_json::to_string_pretty(&loaded).unwrap(),
            ));
        }
    }
    assert!(mismatched.is_empty(), "{}", mismatched.join("\n\n"));
}

/// Whether two effective configurations are the same configuration.
///
/// Numbers compare within a relative tolerance a fraction of a percent of a
/// config value could never cross: the two documents travel through a text
/// round-trip (see `load_fixture`), and how many digits the last bit of a
/// 32-bit float survives depends on which parse path the build links - the
/// workspace can link a `serde_json` build whose float parse is one bit more
/// precise than a lone crate's. That tail is the formatter's, not the
/// configuration's; pinning it would break the gate on an unrelated
/// dependency build, and never on a real configuration change.
fn same_configuration(recorded: &Value, loaded: &Value) -> bool {
    match (recorded, loaded) {
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64(), b.as_f64());
            match (a, b) {
                (Some(a), Some(b)) => (a - b).abs() <= a.abs().max(b.abs()) * 1e-9,
                // Integers too large for f64 compare exactly.
                _ => a == b,
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_configuration(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| same_configuration(a, b)))
        }
        (a, b) => a == b,
    }
}
