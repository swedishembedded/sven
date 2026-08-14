// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `/tenant` command - select the tenant scope of the operator console.
//!
//! `/tenant <id>` narrows the cross-session view to one tenant;
//! `/tenant all` (or no argument) restores the cross-tenant view. The
//! resulting [`ImmediateAction::SelectTenant`] is forwarded by the frontend
//! to its operator console as an
//! `OperatorRequest::SelectTenant` (`sven_frontend::operator::OperatorRequest`).
//!
//! Not registered by [`CommandRegistry::with_builtins`](crate::CommandRegistry::with_builtins):
//! frontends opt in through
//! [`CommandRegistry::register_operator_commands`](crate::CommandRegistry::register_operator_commands)
//! when they wire an operator console — without one the command would be a
//! visible no-op.

use crate::{
    CommandContext, CommandResult, CompletionItem, ImmediateAction, SlashCommand,
};

pub struct TenantCommand;

impl SlashCommand for TenantCommand {
    fn name(&self) -> &str {
        "tenant"
    }

    fn description(&self) -> &str {
        "Select the operator console's tenant scope (/tenant <id> or /tenant all)"
    }

    fn complete(
        &self,
        arg_index: usize,
        partial: &str,
        _ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        if arg_index != 0 {
            return vec![];
        }
        // Tenant ids are only known to the running operator console; the
        // static registry can just offer the cross-tenant view.
        let items = vec![CompletionItem::with_desc(
            "all",
            "all",
            "Show sessions of every tenant",
        )];
        crate::completion::filter_and_rank(items, partial)
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        let arg = args.into_iter().next().unwrap_or_default();
        let tenant = match arg.as_str() {
            "" | "all" => None,
            id => Some(id.to_string()),
        };
        CommandResult {
            immediate_action: Some(ImmediateAction::SelectTenant { tenant }),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected_tenant(result: CommandResult) -> Option<String> {
        match result.immediate_action {
            Some(ImmediateAction::SelectTenant { tenant }) => tenant,
            other => panic!("expected SelectTenant, got {other:?}"),
        }
    }

    #[test]
    fn execute_with_id_selects_that_tenant() {
        let tenant = selected_tenant(TenantCommand.execute(vec!["acme".into()]));
        assert_eq!(tenant.as_deref(), Some("acme"));
    }

    #[test]
    fn execute_all_selects_the_cross_tenant_view() {
        assert_eq!(
            selected_tenant(TenantCommand.execute(vec!["all".into()])),
            None
        );
    }

    #[test]
    fn execute_without_args_selects_the_cross_tenant_view() {
        assert_eq!(selected_tenant(TenantCommand.execute(vec![])), None);
    }

    #[test]
    fn complete_offers_the_all_scope() {
        use std::sync::Arc;

        use sven_config::Config;

        use crate::CommandContext;
        let ctx = CommandContext {
            config: Arc::new(Config::default()),
            current_model_provider: "openai".into(),
            current_model_name: "gpt-4o".into(),
        };
        let items = TenantCommand.complete(0, "a", &ctx);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].value, "all");
        assert!(TenantCommand.complete(1, "", &ctx).is_empty());
    }
}
