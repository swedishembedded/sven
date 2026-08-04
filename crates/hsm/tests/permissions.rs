//! Permission choke-point tests driven through real dispatches: the effects a
//! transition emits are validated against a policy for the resulting state.

mod common;

use common::{AgentMachine, St};
use sven_hsm::{
    validate_effects_are_allowed, Context, Event, Hsm, MachineError, PermissionPolicy,
    ToolCapability,
};

/// Drives the machine to Planning and returns the `CallTool { ExecuteShell }`
/// effect batch produced by proposing a tool call (post-state is `Running`).
fn shell_effect_batch() -> (Hsm<AgentMachine>, Context, Vec<sven_hsm::Effect>) {
    let mut hsm = Hsm::new(AgentMachine::new());
    let mut ctx = Context::new();
    hsm.init(&mut ctx);
    hsm.dispatch(&Event::user_message("go"), &mut ctx);
    hsm.dispatch(
        &Event::LlmProposedPlan {
            plan: serde_json::Value::Null,
        },
        &mut ctx,
    );
    assert_eq!(hsm.state(), St::Planning);
    let out = hsm.dispatch(
        &Event::LlmProposedToolCall {
            name: "sh".into(),
            args: serde_json::Value::Null,
        },
        &mut ctx,
    );
    assert_eq!(hsm.state(), St::Running);
    (hsm, ctx, out.effects)
}

#[test]
fn forbidden_capability_is_rejected() {
    let (hsm, ctx, effects) = shell_effect_batch();
    // Policy grants nothing in Running -> ExecuteShell is forbidden.
    let policy = PermissionPolicy::builder().build();
    let err = validate_effects_are_allowed(&policy, &hsm.state(), &effects, &ctx).unwrap_err();
    assert!(
        matches!(
            err,
            MachineError::ForbiddenToolCall {
                capability: ToolCapability::ExecuteShell,
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn allowed_but_unapproved_dangerous_capability_requires_approval() {
    let (hsm, ctx, effects) = shell_effect_batch();
    // ExecuteShell is allowed in Running but it is inherently dangerous, so it
    // still needs a granted approval.
    let policy = PermissionPolicy::builder()
        .allow_in(St::Running, [ToolCapability::ExecuteShell])
        .build();
    let err = validate_effects_are_allowed(&policy, &hsm.state(), &effects, &ctx).unwrap_err();
    assert!(
        matches!(err, MachineError::HumanApprovalRequired { .. }),
        "got {err:?}"
    );
}

#[test]
fn allowed_and_approved_capability_passes() {
    let (hsm, mut ctx, effects) = shell_effect_batch();
    let policy = PermissionPolicy::builder()
        .allow_in(St::Running, [ToolCapability::ExecuteShell])
        .build();
    // The human has approved shell execution this session.
    ctx.grant(ToolCapability::ExecuteShell);
    assert!(validate_effects_are_allowed(&policy, &hsm.state(), &effects, &ctx).is_ok());
}
