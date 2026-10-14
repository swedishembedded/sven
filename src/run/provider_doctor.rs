// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for making agent setups that fail
// early and explain why for its clients. If your team needs expertise in
// reliable LLM provider integration then you can procure our services by
// sending an email to info@swedishembedded.com.
//! `sven doctor provider`.

use std::fmt::Write as _;

use sven_bootstrap::Config;
use sven_model_drivers::{check_provider, ProviderReport, Verdict};

/// Prints whether the configured model can be used; an error (non-zero exit)
/// when it cannot.
pub(crate) async fn run_doctor_provider(config: &Config, json: bool) -> anyhow::Result<()> {
    let report = check_provider(&config.model).await;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render(&report));
    }
    anyhow::ensure!(report.ready(), "the configured model cannot be used");
    Ok(())
}

fn render(report: &ProviderReport) -> String {
    let mut out = String::new();
    for check in &report.checks {
        let mark = match check.verdict {
            Verdict::Ok => "ok  ",
            Verdict::Warn => "warn",
            Verdict::Fail => "FAIL",
        };
        let _ = writeln!(out, "{mark} {}: {}", check.name, check.detail);
        if let Some(fix) = &check.fix {
            let _ = writeln!(out, "     fix: {fix}");
        }
    }
    out.push_str(if report.ready() {
        "ready: sven can use this model\n"
    } else {
        "not ready: fix the failed checks above\n"
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model_drivers::ProviderCheck;

    fn check(verdict: Verdict, fix: Option<&str>) -> ProviderCheck {
        ProviderCheck {
            name: "endpoint",
            verdict,
            detail: "http://127.0.0.1:1/v1 did not answer".into(),
            fix: fix.map(str::to_string),
        }
    }

    #[test]
    fn a_failed_check_prints_its_fix_and_the_report_is_not_ready() {
        let text = render(&ProviderReport {
            checks: vec![check(Verdict::Fail, Some("start the server"))],
        });
        assert!(text.contains("FAIL endpoint") && text.contains("fix: start the server"));
        assert!(text.ends_with("not ready: fix the failed checks above\n"));
    }

    #[test]
    fn warnings_do_not_stop_the_report_from_being_ready() {
        let text = render(&ProviderReport {
            checks: vec![check(Verdict::Warn, Some("pick a model with tools"))],
        });
        assert!(text.contains("warn endpoint"));
        assert!(text.ends_with("ready: sven can use this model\n"));
    }
}
