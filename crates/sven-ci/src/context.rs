// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Re-exports from `sven-runtime` for backwards compatibility.
//!
//! Environment-detection utilities now live in `sven-runtime`.  This module
//! re-exports them so existing code that imports from `sven_ci::context`
//! continues to compile without modification.

pub use sven_runtime::{
    find_project_root,
    collect_git_context,
    detect_ci_context,
    load_project_context_file,
    ci_template_vars,
    GitContext,
    CiContext,
};
