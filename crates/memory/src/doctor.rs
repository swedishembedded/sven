// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven learn doctor` - a first-run preflight for the local learning pipeline.
//!
//! `submitter_from_config` fails fast on the *first* unset field, which is
//! right for a submitter that is about to run - but wrong for an operator
//! trying to find out everything standing between a fresh install and a
//! working `sven learn flush`, who would otherwise fix one field, retry, hit
//! the next, and repeat three times before ever reaching a real error. This
//! runs every independent check and reports all of them in one pass, which is
//! the concrete discharge of "first-time experience must be error-free":
//! naming every problem is what error-free actually requires when there is
//! more than one.
//!
//! Swedish Embedded AB implements solutions for error-free first-run
//! experiences in autonomous agent tooling for its clients. If your team
//! needs expertise in turning a chain of possible misconfigurations into one
//! clear report then you can procure our services by sending an email to
//! info@swedishembedded.com.

use crate::ledger::PendingFactsLedger;

/// One independent check's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckStatus {
    /// The check passed; `String` is a short confirming detail.
    Ok(String),
    /// Not necessarily wrong, but worth the operator's attention.
    Warning(String),
    /// Must be fixed before `sven learn flush` can do real work.
    Fail(String),
}

impl CheckStatus {
    /// `true` for [`CheckStatus::Fail`].
    #[must_use]
    pub fn is_fail(&self) -> bool {
        matches!(self, CheckStatus::Fail(_))
    }
}

/// One named check and its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    /// Short, stable name (e.g. `"base_weights"`), for scripts that want to
    /// key off a specific check rather than parse prose.
    pub name: &'static str,
    /// What the check found.
    pub status: CheckStatus,
}

/// Every check's result, in a fixed, documented order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    /// One entry per check, always in the same order regardless of outcome -
    /// a script parsing this output can rely on the order, not just the name.
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    /// `true` if every check passed or merely warned - i.e. `sven learn
    /// flush` would at least get as far as trying, not refuse outright.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        !self.checks.iter().any(|c| c.status.is_fail())
    }
}

/// Runs every independent preflight check against `config` and the default
/// pending-facts ledger.
#[must_use]
pub fn diagnose(config: &sven_config::Config) -> DoctorReport {
    let learning = &config.tools.memory.learning;
    let mut checks = Vec::new();

    checks.push(match learning.submitter.as_str() {
        "none" => DoctorCheck {
            name: "submitter",
            status: CheckStatus::Warning(
                "tools.memory.learning.submitter is \"none\" - facts are admitted to the \
                 ledger but never handed to a training pipeline until this is set to \
                 \"local\""
                    .to_string(),
            ),
        },
        "local" => DoctorCheck {
            name: "submitter",
            status: CheckStatus::Ok("\"local\"".to_string()),
        },
        other => DoctorCheck {
            name: "submitter",
            status: CheckStatus::Fail(format!(
                "unknown tools.memory.learning.submitter {other:?}: expected \"local\" or \"none\""
            )),
        },
    });

    checks.push(required_field(
        "base_weights",
        learning.base_weights.as_deref(),
        "the base checkpoint a study trains its LoRA adapter over - must be the same \
         base the served model was built from",
    ));
    checks.push(required_field(
        "anchors_file",
        learning.anchors_file.as_deref(),
        "the behavioural anchor suite every study rehearses - brain refuses a study \
         without one",
    ));
    checks.push(required_field(
        "adapter_dir",
        learning.adapter_dir.as_deref(),
        "must be the same directory this machine's `brain serve --watch-adapters DIR` polls",
    ));

    if let CheckStatus::Ok(path) = &checks
        .iter()
        .find(|c| c.name == "anchors_file")
        .expect("pushed above")
        .status
    {
        checks.push(check_readable_file("anchors_file_readable", path));
    }
    if let CheckStatus::Ok(path) = &checks
        .iter()
        .find(|c| c.name == "adapter_dir")
        .expect("pushed above")
        .status
    {
        checks.push(check_directory("adapter_dir_exists", path));
    }

    checks.push(check_binary_on_path("brain_bin", &learning.brain_bin));

    checks.push(check_pending_facts(learning.batch_size));

    DoctorReport { checks }
}

/// A required, unset-by-default config string: `Ok` naming the value if set,
/// `Fail` with `what` explaining what the field is for otherwise. Mirrors
/// `submitter_from_config`'s own three messages, but as a non-short-circuiting
/// check rather than a `?`-chained `Result`.
fn required_field(name: &'static str, value: Option<&str>, what: &str) -> DoctorCheck {
    match value {
        Some(v) if !v.trim().is_empty() => DoctorCheck {
            name,
            status: CheckStatus::Ok(v.to_string()),
        },
        _ => DoctorCheck {
            name,
            status: CheckStatus::Fail(format!("tools.memory.learning.{name} is not set - {what}")),
        },
    }
}

fn check_readable_file(name: &'static str, path: &str) -> DoctorCheck {
    let expanded = shellexpand::full(path)
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| path.to_string());
    match std::fs::metadata(&expanded) {
        Ok(m) if m.is_file() => DoctorCheck {
            name,
            status: CheckStatus::Ok(expanded),
        },
        Ok(_) => DoctorCheck {
            name,
            status: CheckStatus::Fail(format!("{expanded} exists but is not a file")),
        },
        Err(e) => DoctorCheck {
            name,
            status: CheckStatus::Fail(format!("{expanded}: {e}")),
        },
    }
}

fn check_directory(name: &'static str, path: &str) -> DoctorCheck {
    let expanded = shellexpand::full(path)
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| path.to_string());
    match std::fs::metadata(&expanded) {
        Ok(m) if m.is_dir() => DoctorCheck {
            name,
            status: CheckStatus::Ok(format!(
                "{expanded} exists - but sven cannot verify this is the directory a live \
                 `brain serve --watch-adapters` actually polls; a mismatch there trains a \
                 model nothing serves"
            )),
        },
        Ok(_) => DoctorCheck {
            name,
            status: CheckStatus::Fail(format!("{expanded} exists but is not a directory")),
        },
        Err(_) => DoctorCheck {
            name,
            status: CheckStatus::Warning(format!(
                "{expanded} does not exist yet - it will be created on first promotion, but \
                 confirm this is the directory `brain serve --watch-adapters` polls before \
                 relying on it"
            )),
        },
    }
}

/// `true` only if `bin` actually resolves and can be spawned - an
/// `io::ErrorKind::NotFound` from `Command::spawn` is exactly what "not on
/// PATH" looks like; any other outcome (including a non-zero exit, which
/// `--help` may legitimately return) means the binary was found.
fn check_binary_on_path(name: &'static str, bin: &str) -> DoctorCheck {
    match std::process::Command::new(bin)
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(_) => DoctorCheck {
            name,
            status: CheckStatus::Ok(format!("{bin} resolves and runs")),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DoctorCheck {
            name,
            status: CheckStatus::Fail(format!(
                "{bin:?} is not on PATH - install brain or set tools.memory.learning.brain_bin \
                 to its full path"
            )),
        },
        Err(e) => DoctorCheck {
            name,
            status: CheckStatus::Fail(format!("{bin:?}: {e}")),
        },
    }
}

/// How many probe-carrying facts the default ledger has ever admitted,
/// compared against the configured `batch_size`.
///
/// Deliberately not a precise "ready to flush right now" count: that needs
/// the drain cursor, which this diagnostic does not touch (draining is
/// `flush`'s job, not doctor's). This is honestly a total, not a queue
/// depth, but it is still a useful signal for "have you extracted enough
/// facts yet" on a fresh install, where the two numbers are the same anyway.
fn check_pending_facts(batch_size: usize) -> DoctorCheck {
    let ledger = PendingFactsLedger::at_default_path();
    match ledger.pending_facts() {
        Ok(facts) => {
            let scoreable = facts.iter().filter(|f| f.probe.is_some()).count();
            if scoreable == 0 {
                DoctorCheck {
                    name: "pending_facts",
                    status: CheckStatus::Warning(
                        "no probe-carrying facts recorded yet - nothing for `sven learn flush` \
                         to submit"
                            .to_string(),
                    ),
                }
            } else if scoreable < batch_size {
                DoctorCheck {
                    name: "pending_facts",
                    status: CheckStatus::Warning(format!(
                        "{scoreable} probe-carrying fact(s) recorded so far, short of the \
                         configured batch_size ({batch_size}) - brain also enforces its own \
                         floor per cycle, independent of this setting"
                    )),
                }
            } else {
                DoctorCheck {
                    name: "pending_facts",
                    status: CheckStatus::Ok(format!(
                        "{scoreable} probe-carrying fact(s) recorded, at or above batch_size ({batch_size})"
                    )),
                }
            }
        }
        Err(e) => DoctorCheck {
            name: "pending_facts",
            status: CheckStatus::Warning(format!("could not read the pending-facts ledger: {e}")),
        },
    }
}

/// Formats a report as one line per check, for `sven learn doctor`'s
/// terminal output.
#[must_use]
pub fn format_report(report: &DoctorReport) -> String {
    let mut lines = Vec::with_capacity(report.checks.len() + 1);
    for check in &report.checks {
        let (tag, detail) = match &check.status {
            CheckStatus::Ok(detail) => ("ok", detail.as_str()),
            CheckStatus::Warning(detail) => ("warn", detail.as_str()),
            CheckStatus::Fail(detail) => ("fail", detail.as_str()),
        };
        lines.push(format!("[{tag}] {}: {detail}", check.name));
    }
    if report.is_healthy() {
        lines.push("sven learn flush should at least be able to try.".to_string());
    } else {
        lines.push(
            "fix the [fail] items above before `sven learn flush` can do real work.".to_string(),
        );
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(mutate: impl FnOnce(&mut sven_config::Config)) -> sven_config::Config {
        let mut cfg = sven_config::Config::default();
        mutate(&mut cfg);
        cfg
    }

    #[test]
    fn a_fresh_config_fails_all_three_required_fields() {
        let cfg = config_with(|c| c.tools.memory.learning.submitter = "local".to_string());
        let report = diagnose(&cfg);
        for name in ["base_weights", "anchors_file", "adapter_dir"] {
            let check = report
                .checks
                .iter()
                .find(|c| c.name == name)
                .expect("check present");
            assert!(
                check.status.is_fail(),
                "{name} should fail on a fresh config: {check:?}"
            );
        }
        assert!(!report.is_healthy());
    }

    #[test]
    fn submitter_none_is_a_warning_not_a_failure() {
        let cfg = config_with(|c| c.tools.memory.learning.submitter = "none".to_string());
        let report = diagnose(&cfg);
        let check = report
            .checks
            .iter()
            .find(|c| c.name == "submitter")
            .expect("check present");
        assert_eq!(check.status, CheckStatus::Warning(
            "tools.memory.learning.submitter is \"none\" - facts are admitted to the ledger but never handed to a training pipeline until this is set to \"local\"".to_string()
        ));
    }

    #[test]
    fn an_unknown_submitter_fails() {
        let cfg = config_with(|c| c.tools.memory.learning.submitter = "sftp".to_string());
        let report = diagnose(&cfg);
        let check = report
            .checks
            .iter()
            .find(|c| c.name == "submitter")
            .expect("check present");
        assert!(check.status.is_fail());
    }

    #[test]
    fn a_brain_bin_not_on_path_fails_by_name() {
        let cfg = config_with(|c| {
            c.tools.memory.learning.submitter = "local".to_string();
            c.tools.memory.learning.brain_bin =
                "sven-doctor-test-nonexistent-binary-xyz".to_string();
        });
        let report = diagnose(&cfg);
        let check = report
            .checks
            .iter()
            .find(|c| c.name == "brain_bin")
            .expect("check present");
        assert!(check.status.is_fail());
    }

    #[test]
    fn format_report_names_every_failing_check_in_one_pass() {
        let cfg = config_with(|c| c.tools.memory.learning.submitter = "local".to_string());
        let report = diagnose(&cfg);
        let text = format_report(&report);
        assert!(text.contains("base_weights"));
        assert!(text.contains("anchors_file"));
        assert!(text.contains("adapter_dir"));
    }
}
