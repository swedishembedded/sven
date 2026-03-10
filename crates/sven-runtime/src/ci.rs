// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! CI environment detection and template variable extraction.

// ─── CI context ───────────────────────────────────────────────────────────────

/// Snapshot of the CI environment read from well-known environment variables.
#[derive(Debug, Default)]
pub struct CiContext {
    pub provider: Option<String>,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub pr_number: Option<String>,
    pub run_id: Option<String>,
}

/// Detect the current CI provider and relevant metadata from well-known
/// environment variables.
pub fn detect_ci_context() -> CiContext {
    let mut ctx = CiContext::default();

    if std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true") {
        ctx.provider = Some("GitHub Actions".to_string());
        ctx.repo = std::env::var("GITHUB_REPOSITORY").ok();
        ctx.branch = std::env::var("GITHUB_REF_NAME").ok();
        ctx.commit = std::env::var("GITHUB_SHA").ok();
        ctx.run_id = std::env::var("GITHUB_RUN_ID").ok();
        ctx.pr_number = std::env::var("GITHUB_EVENT_NUMBER")
            .ok()
            .or_else(|| std::env::var("PR_NUMBER").ok());
    } else if std::env::var("GITLAB_CI").as_deref() == Ok("true") {
        ctx.provider = Some("GitLab CI".to_string());
        ctx.repo = std::env::var("CI_PROJECT_PATH").ok();
        ctx.branch = std::env::var("CI_COMMIT_REF_NAME").ok();
        ctx.commit = std::env::var("CI_COMMIT_SHA").ok();
        ctx.run_id = std::env::var("CI_PIPELINE_ID").ok();
        ctx.pr_number = std::env::var("CI_MERGE_REQUEST_IID").ok();
    } else if std::env::var("CIRCLECI").as_deref() == Ok("true") {
        ctx.provider = Some("CircleCI".to_string());
        ctx.repo = std::env::var("CIRCLE_REPOSITORY_URL").ok();
        ctx.branch = std::env::var("CIRCLE_BRANCH").ok();
        ctx.commit = std::env::var("CIRCLE_SHA1").ok();
        ctx.run_id = std::env::var("CIRCLE_BUILD_NUM").ok();
        ctx.pr_number = std::env::var("CIRCLE_PR_NUMBER").ok();
    } else if std::env::var("TRAVIS").as_deref() == Ok("true") {
        ctx.provider = Some("Travis CI".to_string());
        ctx.repo = std::env::var("TRAVIS_REPO_SLUG").ok();
        ctx.branch = std::env::var("TRAVIS_BRANCH").ok();
        ctx.commit = std::env::var("TRAVIS_COMMIT").ok();
        ctx.run_id = std::env::var("TRAVIS_BUILD_ID").ok();
        ctx.pr_number = std::env::var("TRAVIS_PULL_REQUEST")
            .ok()
            .filter(|v| v != "false");
    } else if std::env::var("JENKINS_URL").is_ok() || std::env::var("BUILD_URL").is_ok() {
        ctx.provider = Some("Jenkins".to_string());
        ctx.branch = std::env::var("BRANCH_NAME")
            .ok()
            .or_else(|| std::env::var("GIT_BRANCH").ok());
        ctx.commit = std::env::var("GIT_COMMIT").ok();
        ctx.run_id = std::env::var("BUILD_NUMBER").ok();
    } else if std::env::var("TF_BUILD").as_deref() == Ok("True") {
        ctx.provider = Some("Azure Pipelines".to_string());
        ctx.repo = std::env::var("BUILD_REPOSITORY_NAME").ok();
        ctx.branch = std::env::var("BUILD_SOURCEBRANCH")
            .ok()
            .map(|b| b.trim_start_matches("refs/heads/").to_string());
        ctx.commit = std::env::var("BUILD_SOURCEVERSION").ok();
        ctx.run_id = std::env::var("BUILD_BUILDID").ok();
        ctx.pr_number = std::env::var("SYSTEM_PULLREQUEST_PULLREQUESTNUMBER").ok();
    } else if std::env::var("BITBUCKET_BUILD_NUMBER").is_ok() {
        ctx.provider = Some("Bitbucket Pipelines".to_string());
        ctx.repo = std::env::var("BITBUCKET_REPO_FULL_NAME").ok();
        ctx.branch = std::env::var("BITBUCKET_BRANCH").ok();
        ctx.commit = std::env::var("BITBUCKET_COMMIT").ok();
        ctx.run_id = std::env::var("BITBUCKET_BUILD_NUMBER").ok();
        ctx.pr_number = std::env::var("BITBUCKET_PR_ID").ok();
    } else if std::env::var("CI").as_deref() == Ok("true") {
        ctx.provider = Some("CI".to_string());
        ctx.branch = std::env::var("BRANCH_NAME")
            .ok()
            .or_else(|| std::env::var("GIT_BRANCH").ok());
        ctx.commit = std::env::var("GIT_COMMIT").ok();
    }

    ctx
}

impl CiContext {
    /// Returns true if any CI provider was detected.
    pub fn is_ci(&self) -> bool {
        self.provider.is_some()
    }

    /// Format as a system-prompt section.  Returns `None` when not in CI.
    pub fn to_prompt_section(&self) -> Option<String> {
        let provider = self.provider.as_deref()?;

        let mut lines = vec![
            "## CI Environment".to_string(),
            format!("Running in: {}", provider),
        ];

        if let Some(repo) = &self.repo {
            lines.push(format!("Repository: {}", repo));
        }
        if let Some(branch) = &self.branch {
            lines.push(format!("Branch: {}", branch));
        }
        if let Some(commit) = &self.commit {
            let short = &commit[..commit.len().min(12)];
            lines.push(format!("Commit: {}", short));
        }
        if let Some(pr) = &self.pr_number {
            lines.push(format!("PR/MR: #{}", pr));
        }

        Some(lines.join("\n"))
    }
}

// ─── CI env vars as template variables ───────────────────────────────────────

/// Build a map of well-known CI environment variables for use as template
/// variables in workflow files.
pub fn ci_template_vars(ci: &CiContext) -> std::collections::HashMap<String, String> {
    let mut vars = std::collections::HashMap::new();

    let raw_vars: &[&str] = &[
        "GITHUB_SHA",
        "GITHUB_REF_NAME",
        "GITHUB_REPOSITORY",
        "GITHUB_RUN_ID",
        "GITHUB_EVENT_NUMBER",
        "GITHUB_ACTOR",
        "GITHUB_WORKFLOW",
        "CI_COMMIT_SHA",
        "CI_COMMIT_REF_NAME",
        "CI_PROJECT_PATH",
        "CI_PIPELINE_ID",
        "CI_MERGE_REQUEST_IID",
        "CIRCLE_SHA1",
        "CIRCLE_BRANCH",
        "CIRCLE_BUILD_NUM",
        "TRAVIS_COMMIT",
        "TRAVIS_BRANCH",
        "TRAVIS_REPO_SLUG",
        "BUILD_SOURCEVERSION",
        "BUILD_SOURCEBRANCH",
        "BUILD_REPOSITORY_NAME",
        "BITBUCKET_COMMIT",
        "BITBUCKET_BRANCH",
        "GIT_COMMIT",
        "GIT_BRANCH",
        "BUILD_NUMBER",
        "BRANCH_NAME",
    ];
    for name in raw_vars {
        if let Ok(val) = std::env::var(name) {
            vars.insert(format!("CI_{}", name.to_lowercase()), val);
        }
    }

    if let Some(v) = &ci.branch {
        vars.entry("branch".into()).or_insert_with(|| v.clone());
    }
    if let Some(v) = &ci.commit {
        vars.entry("commit".into()).or_insert_with(|| v.clone());
    }
    if let Some(v) = &ci.repo {
        vars.entry("repo".into()).or_insert_with(|| v.clone());
    }
    if let Some(v) = &ci.pr_number {
        vars.entry("pr".into()).or_insert_with(|| v.clone());
    }

    vars
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_context_no_provider_returns_empty_section() {
        let ctx = CiContext::default();
        assert!(ctx.to_prompt_section().is_none());
        assert!(!ctx.is_ci());
    }

    #[test]
    fn ci_context_with_provider_formats_section() {
        let ctx = CiContext {
            provider: Some("GitHub Actions".to_string()),
            repo: Some("acme/repo".to_string()),
            branch: Some("main".to_string()),
            commit: Some("abc123def456".to_string()),
            pr_number: Some("42".to_string()),
            run_id: None,
        };
        let section = ctx.to_prompt_section().unwrap();
        assert!(section.contains("GitHub Actions"));
        assert!(section.contains("acme/repo"));
        assert!(section.contains("main"));
        assert!(section.contains("abc123"));
        assert!(section.contains("#42"));
    }

    #[test]
    fn ci_template_vars_provides_shortcuts() {
        let ctx = CiContext {
            provider: Some("Test CI".to_string()),
            branch: Some("feat/test".to_string()),
            commit: Some("abc1234".to_string()),
            repo: Some("acme/repo".to_string()),
            pr_number: Some("7".to_string()),
            run_id: None,
        };
        let vars = ci_template_vars(&ctx);
        assert_eq!(vars.get("branch").map(|s| s.as_str()), Some("feat/test"));
        assert_eq!(vars.get("commit").map(|s| s.as_str()), Some("abc1234"));
        assert_eq!(vars.get("repo").map(|s| s.as_str()), Some("acme/repo"));
        assert_eq!(vars.get("pr").map(|s| s.as_str()), Some("7"));
    }
}
