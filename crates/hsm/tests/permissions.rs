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
fn a_call_the_policy_asks_about_needs_its_own_approval() {
    let (hsm, mut ctx, effects) = shell_effect_batch();
    let policy = PermissionPolicy::builder()
        .allow_in(St::Running, [ToolCapability::ExecuteShell])
        .build();
    assert!(
        validate_effects_are_allowed(&policy, &hsm.state(), &effects, &ctx).is_ok(),
        "what the state allows runs unless the policy asks for approval"
    );

    let manual = policy.with_manual_approval();
    let err = validate_effects_are_allowed(&manual, &hsm.state(), &effects, &ctx).unwrap_err();
    assert!(
        matches!(err, MachineError::HumanApprovalRequired { .. }),
        "got {err:?}"
    );

    let call_id = effects
        .iter()
        .find_map(|effect| match effect {
            sven_hsm::Effect::CallTool { call_id, .. } => Some(call_id),
            _ => None,
        })
        .expect("the batch calls a tool");
    let approval_id = sven_hsm::ApprovalId::from_uuid(call_id.as_uuid());
    ctx.set_pending_approval(sven_hsm::PendingApproval {
        approval_id,
        capability: ToolCapability::ExecuteShell,
        description: "sh".into(),
        call_id: Some(*call_id),
    });
    ctx.approve(approval_id);
    assert!(validate_effects_are_allowed(&manual, &hsm.state(), &effects, &ctx).is_ok());
}
