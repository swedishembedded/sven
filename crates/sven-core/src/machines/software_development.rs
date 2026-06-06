//! The full SDLC hierarchical state machine.
//!
//! [`SoftwareDevelopmentMachine`] drives a complete software-development
//! lifecycle: from understanding what the user wants all the way through
//! delivery and human sign-off. The machine can also be instantiated as a
//! submachine inside [`super::conversation::ConversationMachine`].
//!
//! # State hierarchy (flat enum with `superstate` encoding)
//!
//! ```text
//! Top
//! ├── Intake
//! │   ├── InterpretUserIntent
//! │   ├── ExtractProblemStatement
//! │   ├── ExtractConstraints
//! │   ├── AssessInformationCompleteness
//! │   └── ConfirmScope
//! ├── Discovery
//! │   ├── RequestArtifacts … ProduceDiscoverySummary
//! ├── Planning
//! │   ├── GenerateCandidatePlan … ApproveExecutionPlan
//! ├── Execution
//! │   ├── SelectNextTask … DecideTaskOutcome
//! ├── Verification
//! │   ├── VerifyRequirements … HumanAcceptanceGate
//! ├── Delivery
//! │   ├── ProduceTechnicalSummary … FinalApproval
//! ├── AwaitUser          ← cross-cutting: any state may suspend here
//! ├── AwaitHumanApproval ← cross-cutting approval gate
//! ├── AwaitTool          ← cross-cutting tool-in-flight
//! ├── Recovery
//! │   ├── ClassifyFailure … EscalateToHuman
//! ├── RollingBack
//! ├── Done               ← terminal
//! ├── Failed             ← terminal
//! └── Cancelled          ← terminal
//! ```

use serde_json::{json, Value};
use sven_hsm::{
    context::{Context, PendingApproval},
    effect::Effect,
    event::{Event, InternalEvent},
    ids::{ApprovalId, MachineId, ToolCallId},
    machine::Machine,
    permissions::{PermissionPolicy, ToolCapability},
    status::Reaction,
};

/// Every state in the SDLC machine (composite + leaf).
///
/// Composite states (Intake, Discovery, …) can appear in `superstate` return
/// values but are never the active *leaf* state; they are transiently the
/// active state only during initial-transition drilling.
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SdmState {
    // Root
    Top,

    // ── Intake ──────────────────────────────────────────────────────────
    Intake,
    /// Waits for the first `UserMessage` before starting the intake process.
    /// The SDLC machine must not fire LLM calls before the user has spoken.
    Idle,
    InterpretUserIntent,
    ExtractProblemStatement,
    ExtractConstraints,
    AssessInformationCompleteness,
    ConfirmScope,

    // ── Discovery ───────────────────────────────────────────────────────
    Discovery,
    RequestArtifacts,
    ClassifyArtifacts,
    InspectRepository,
    BuildBaseline,
    ExtractTechnicalContext,
    ProduceDiscoverySummary,

    // ── Planning ────────────────────────────────────────────────────────
    Planning,
    GenerateCandidatePlan,
    DecomposeIntoTasks,
    ValidatePlan,
    EstimateRisk,
    SelectPlan,
    ApproveExecutionPlan,

    // ── Execution ───────────────────────────────────────────────────────
    Execution,
    SelectNextTask,
    PrepareTaskContext,
    ProposePatch,
    ApplyPatch,
    Build,
    RunTests,
    StaticAnalysis,
    ObserveResult,
    DecideTaskOutcome,

    // ── Verification ────────────────────────────────────────────────────
    Verification,
    VerifyRequirements,
    VerifyTests,
    VerifySecurityBoundaries,
    VerifyRegressionRisk,
    HumanAcceptanceGate,

    // ── Delivery ────────────────────────────────────────────────────────
    Delivery,
    ProduceTechnicalSummary,
    ProduceUserInstructions,
    PackageArtifacts,
    FinalApproval,

    // ── Cross-cutting waiting states (direct children of Top) ───────────
    AwaitUser,
    AwaitHumanApproval,
    AwaitTool,

    // ── Recovery ────────────────────────────────────────────────────────
    Recovery,
    ClassifyFailure,
    ProposeRecoveryOptions,
    SelectRecoveryAction,
    Retry,
    Rollback,
    AskUserForDecision,
    EscalateToHuman,

    // ── Terminal ────────────────────────────────────────────────────────
    RollingBack,
    Done,
    Failed,
    Cancelled,
}

/// Returns the parent state in the hierarchy (static; no `self` needed).
pub(crate) fn super_of(state: SdmState) -> SdmState {
    use SdmState::*;
    match state {
        Top => Top,
        // Direct children of Top
        Intake | Discovery | Planning | Execution | Verification | Delivery | AwaitUser
        | AwaitHumanApproval | AwaitTool | Recovery | RollingBack | Done | Failed | Cancelled => {
            Top
        }
        // Children of Intake
        Idle
        | InterpretUserIntent
        | ExtractProblemStatement
        | ExtractConstraints
        | AssessInformationCompleteness
        | ConfirmScope => Intake,
        // Children of Discovery
        RequestArtifacts
        | ClassifyArtifacts
        | InspectRepository
        | BuildBaseline
        | ExtractTechnicalContext
        | ProduceDiscoverySummary => Discovery,
        // Children of Planning
        GenerateCandidatePlan
        | DecomposeIntoTasks
        | ValidatePlan
        | EstimateRisk
        | SelectPlan
        | ApproveExecutionPlan => Planning,
        // Children of Execution
        SelectNextTask | PrepareTaskContext | ProposePatch | ApplyPatch | Build | RunTests
        | StaticAnalysis | ObserveResult | DecideTaskOutcome => Execution,
        // Children of Verification
        VerifyRequirements
        | VerifyTests
        | VerifySecurityBoundaries
        | VerifyRegressionRisk
        | HumanAcceptanceGate => Verification,
        // Children of Delivery
        ProduceTechnicalSummary | ProduceUserInstructions | PackageArtifacts | FinalApproval => {
            Delivery
        }
        // Children of Recovery
        ClassifyFailure
        | ProposeRecoveryOptions
        | SelectRecoveryAction
        | Retry
        | Rollback
        | AskUserForDecision
        | EscalateToHuman => Recovery,
    }
}

/// Resolves the continuation state stored in `ctx.facts["continuation"]`.
/// Falls back to `SdmState::SelectNextTask` when the key is absent or unknown.
fn resolve_continuation(ctx: &Context) -> SdmState {
    ctx.facts
        .get("continuation")
        .and_then(Value::as_str)
        .and_then(state_from_str)
        .unwrap_or(SdmState::SelectNextTask)
}

/// Maps the `Debug` label of a state back to its enum variant.
fn state_from_str(s: &str) -> Option<SdmState> {
    use SdmState::*;
    Some(match s {
        "Top" => Top,
        "Intake" => Intake,
        "Idle" => Idle,
        "InterpretUserIntent" => InterpretUserIntent,
        "ExtractProblemStatement" => ExtractProblemStatement,
        "ExtractConstraints" => ExtractConstraints,
        "AssessInformationCompleteness" => AssessInformationCompleteness,
        "ConfirmScope" => ConfirmScope,
        "Discovery" => Discovery,
        "RequestArtifacts" => RequestArtifacts,
        "ClassifyArtifacts" => ClassifyArtifacts,
        "InspectRepository" => InspectRepository,
        "BuildBaseline" => BuildBaseline,
        "ExtractTechnicalContext" => ExtractTechnicalContext,
        "ProduceDiscoverySummary" => ProduceDiscoverySummary,
        "Planning" => Planning,
        "GenerateCandidatePlan" => GenerateCandidatePlan,
        "DecomposeIntoTasks" => DecomposeIntoTasks,
        "ValidatePlan" => ValidatePlan,
        "EstimateRisk" => EstimateRisk,
        "SelectPlan" => SelectPlan,
        "ApproveExecutionPlan" => ApproveExecutionPlan,
        "Execution" => Execution,
        "SelectNextTask" => SelectNextTask,
        "PrepareTaskContext" => PrepareTaskContext,
        "ProposePatch" => ProposePatch,
        "ApplyPatch" => ApplyPatch,
        "Build" => Build,
        "RunTests" => RunTests,
        "StaticAnalysis" => StaticAnalysis,
        "ObserveResult" => ObserveResult,
        "DecideTaskOutcome" => DecideTaskOutcome,
        "Verification" => Verification,
        "VerifyRequirements" => VerifyRequirements,
        "VerifyTests" => VerifyTests,
        "VerifySecurityBoundaries" => VerifySecurityBoundaries,
        "VerifyRegressionRisk" => VerifyRegressionRisk,
        "HumanAcceptanceGate" => HumanAcceptanceGate,
        "Delivery" => Delivery,
        "ProduceTechnicalSummary" => ProduceTechnicalSummary,
        "ProduceUserInstructions" => ProduceUserInstructions,
        "PackageArtifacts" => PackageArtifacts,
        "FinalApproval" => FinalApproval,
        "AwaitUser" => AwaitUser,
        "AwaitHumanApproval" => AwaitHumanApproval,
        "AwaitTool" => AwaitTool,
        "Recovery" => Recovery,
        "ClassifyFailure" => ClassifyFailure,
        "ProposeRecoveryOptions" => ProposeRecoveryOptions,
        "SelectRecoveryAction" => SelectRecoveryAction,
        "Retry" => Retry,
        "Rollback" => Rollback,
        "AskUserForDecision" => AskUserForDecision,
        "EscalateToHuman" => EscalateToHuman,
        "RollingBack" => RollingBack,
        "Done" => Done,
        "Failed" => Failed,
        "Cancelled" => Cancelled,
        _ => return None,
    })
}

/// Store a continuation state so that `AwaitUser`/`AwaitHumanApproval`/
/// `AwaitTool` know where to resume.
fn set_continuation(ctx: &mut Context, target: SdmState) {
    ctx.set_fact("continuation", json!(format!("{target:?}")));
}

/// The full SDLC machine.
pub struct SoftwareDevelopmentMachine {
    id: MachineId,
}

impl Default for SoftwareDevelopmentMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl SoftwareDevelopmentMachine {
    /// Creates a new instance.
    pub fn new() -> Self {
        Self {
            id: MachineId::new(),
        }
    }

    /// The permission policy for the SDLC machine.
    ///
    /// - Globally: `ReadFile` (all states may read)
    /// - Discovery: `GitOperation` (repository inspection)
    /// - Execution / ApplyPatch: `WriteFile`, `GitOperation`
    /// - Build / RunTests / StaticAnalysis / PackageArtifacts: `ExecuteShell`
    ///   (inherently dangerous; requires prior `HumanApproved`)
    /// - FinalApproval: `ExecuteShell` (deploy, additionally approval-gated)
    pub fn permission_policy() -> PermissionPolicy {
        use SdmState::*;
        use ToolCapability::*;

        PermissionPolicy::builder()
            .allow_globally([ReadFile])
            .allow_in(InspectRepository, [GitOperation])
            .allow_in(BuildBaseline, [GitOperation])
            .allow_in(ApplyPatch, [WriteFile, GitOperation])
            .allow_in(Build, [ExecuteShell])
            .allow_in(RunTests, [ExecuteShell])
            .allow_in(StaticAnalysis, [ExecuteShell])
            .allow_in(PackageArtifacts, [ExecuteShell])
            .allow_in(FinalApproval, [ExecuteShell, GitOperation])
            // The Rollback state exercises the Rollback capability.
            .allow_in(SdmState::Rollback, [ToolCapability::Rollback])
            .build()
    }
}

impl Machine for SoftwareDevelopmentMachine {
    type State = SdmState;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> SdmState {
        SdmState::Top
    }

    /// First real leaf reached after `init()`: `Intake → InterpretUserIntent`.
    fn initial(&self) -> SdmState {
        SdmState::Intake
    }

    fn superstate(&self, state: SdmState) -> SdmState {
        super_of(state)
    }

    #[allow(clippy::too_many_lines)]
    fn dispatch_state(
        &mut self,
        state: SdmState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<SdmState> {
        use InternalEvent::{Entry, Exit, Init};
        use SdmState::*;

        match state {
            // =================================================================
            // Root
            // =================================================================
            Top => match event {
                Event::UserCancelled => Reaction::transition(
                    RollingBack,
                    vec![Effect::CreateCheckpoint {
                        label: "pre-cancel".into(),
                    }],
                    "user cancelled; checkpointing before rollback",
                ),
                _ => Reaction::Ignored,
            },

            // =================================================================
            // Composite state: Init transitions
            // =================================================================
            Intake => match event {
                // Drill into Idle first so the machine waits for a UserMessage
                // before issuing any LLM call.  This prevents the machine from
                // firing CallLlm on startup before the user has typed anything.
                Event::Internal(Init) => Reaction::goto(Idle),
                // Track exit for cross-superstate ordering tests.
                Event::Internal(Exit) => Reaction::effects(vec![Effect::PersistAudit]),
                _ => Reaction::Super(Top),
            },

            // Wait for the first user message, then begin intake.
            Idle => match event {
                Event::UserMessage { text } => {
                    // Store the original request so LLM states can access it.
                    ctx.set_fact("user_request", json!(text));
                    Reaction::goto(InterpretUserIntent)
                }
                _ => Reaction::Super(Intake),
            },
            Discovery => match event {
                Event::Internal(Init) => Reaction::goto(RequestArtifacts),
                // Track entry for cross-superstate ordering tests.
                Event::Internal(Entry) => Reaction::effects(vec![Effect::PersistAudit]),
                _ => Reaction::Super(Top),
            },
            Planning => match event {
                Event::Internal(Init) => Reaction::goto(GenerateCandidatePlan),
                _ => Reaction::Super(Top),
            },
            Execution => match event {
                Event::Internal(Init) => Reaction::goto(SelectNextTask),
                // Track exit for cross-superstate ordering tests.
                Event::Internal(Exit) => Reaction::effects(vec![Effect::PersistAudit]),
                _ => Reaction::Super(Top),
            },
            Verification => match event {
                Event::Internal(Init) => Reaction::goto(VerifyRequirements),
                _ => Reaction::Super(Top),
            },
            Delivery => match event {
                Event::Internal(Init) => Reaction::goto(ProduceTechnicalSummary),
                _ => Reaction::Super(Top),
            },
            Recovery => match event {
                Event::Internal(Init) => Reaction::goto(ClassifyFailure),
                // Track entry for cross-superstate ordering tests.
                Event::Internal(Entry) => Reaction::effects(vec![Effect::PersistAudit]),
                _ => Reaction::Super(Top),
            },

            // =================================================================
            // Cross-cutting: AwaitUser
            // =================================================================
            AwaitUser => match event {
                Event::UserMessage { text } => {
                    // Ask the LLM to interpret the answer; stay in AwaitUser
                    // until the LlmProposedAssessment comes back.
                    let question = ctx
                        .facts
                        .get("await_user_prompt")
                        .and_then(Value::as_str)
                        .unwrap_or("(no question)")
                        .to_string();
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "interpret_user_answer",
                            "question": question,
                            "answer": text,
                            "expected_answer_shape": "structured data relevant to the question",
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("last_assessment", assessment.clone());
                    Reaction::goto(resolve_continuation(ctx))
                }
                Event::LlmFailed { .. } => {
                    // If interpretation fails, resume with empty assessment.
                    Reaction::goto(resolve_continuation(ctx))
                }
                Event::Timeout { .. } => {
                    let attempts = ctx.bump_retry("await_user_timeout");
                    if attempts < 3 {
                        let prompt = ctx
                            .facts
                            .get("await_user_prompt")
                            .and_then(Value::as_str)
                            .unwrap_or("Please respond to continue.")
                            .to_string();
                        Reaction::effects(vec![Effect::AskUser { prompt }])
                    } else {
                        Reaction::goto(AskUserForDecision)
                    }
                }
                _ => Reaction::Super(Top),
            },

            // =================================================================
            // Cross-cutting: AwaitHumanApproval
            // =================================================================
            AwaitHumanApproval => match event {
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    Reaction::goto(resolve_continuation(ctx))
                }
                Event::HumanRejected { .. } => Reaction::goto(ProposeRecoveryOptions),
                _ => Reaction::Super(Top),
            },

            // =================================================================
            // Cross-cutting: AwaitTool
            // =================================================================
            AwaitTool => match event {
                Event::ToolSucceeded { .. } | Event::ToolFailed { .. } => {
                    Reaction::goto(resolve_continuation(ctx))
                }
                _ => Reaction::Super(Top),
            },

            // =================================================================
            // Intake leaf states
            // =================================================================
            InterpretUserIntent => match event {
                Event::Internal(Entry) => {
                    let text = ctx
                        .facts
                        .get("user_request")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "extract_intent",
                            "text": text,
                            "allowed_intents": [
                                "new_feature", "bug_fix", "refactor",
                                "documentation", "test", "infrastructure",
                                "question", "other"
                            ],
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("intent", assessment.clone());
                    Reaction::goto(ExtractProblemStatement)
                }
                Event::LlmFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Intake),
            },

            ExtractProblemStatement => match event {
                Event::Internal(Entry) => {
                    let intent = ctx
                        .facts
                        .get("intent")
                        .cloned()
                        .and_then(|v| v.get("intent").and_then(Value::as_str).map(str::to_string))
                        .unwrap_or_else(|| "unknown".to_string());
                    let known = ctx.facts.get("intent").cloned().unwrap_or(Value::Null);
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "extract_problem_statement",
                            "intent": intent,
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("problem_statement", assessment.clone());
                    Reaction::goto(ExtractConstraints)
                }
                Event::LlmFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Intake),
            },

            ExtractConstraints => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "intent": ctx.facts.get("intent"),
                        "problem_statement": ctx.facts.get("problem_statement"),
                        "user_request": ctx.facts.get("user_request"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "extract_constraints",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("constraints", assessment.clone());
                    Reaction::goto(AssessInformationCompleteness)
                }
                Event::LlmFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Intake),
            },

            AssessInformationCompleteness => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "intent": ctx.facts.get("intent"),
                        "problem_statement": ctx.facts.get("problem_statement"),
                        "constraints": ctx.facts.get("constraints"),
                        "user_request": ctx.facts.get("user_request"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "assess_completeness",
                            "known_context": known,
                            "required_fields": ["intent", "problem_statement"],
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    // CompletenessAssessment serialises as:
                    //   {"status": {"status": "enough"}} or
                    //   {"status": {"status": "missing", "fields": [...]}}
                    // Read the discriminant from the nested status object.
                    let status_str = assessment
                        .get("status")
                        .and_then(|s| s.get("status"))
                        .and_then(Value::as_str)
                        .unwrap_or("missing");

                    if status_str == "enough" {
                        // Proceed to scope confirmation with a human sign-off.
                        Reaction::goto(ConfirmScope)
                    } else {
                        // Not enough info: ask the user for clarification.
                        let missing_fields: Vec<String> = assessment
                            .get("status")
                            .and_then(|s| s.get("fields"))
                            .and_then(Value::as_array)
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|f| {
                                        f.get("field").and_then(Value::as_str).map(str::to_string)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        let prompt = if missing_fields.is_empty() {
                            "Could you provide more details about what you need?".to_string()
                        } else {
                            format!(
                                "I need more information about: {}. Could you elaborate?",
                                missing_fields.join(", ")
                            )
                        };
                        ctx.set_fact("await_user_prompt", json!(prompt));
                        set_continuation(ctx, AssessInformationCompleteness);
                        Reaction::transition(
                            AwaitUser,
                            vec![Effect::AskUser { prompt }],
                            "missing information; asking user for clarification",
                        )
                    }
                }
                Event::LlmFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Intake),
            },

            ConfirmScope => match event {
                Event::Internal(Entry) => {
                    let approval_id = ApprovalId::new();
                    ctx.set_pending_approval(PendingApproval {
                        approval_id,
                        capability: ToolCapability::GitOperation,
                        description: "Confirm problem scope before proceeding to discovery".into(),
                    });
                    ctx.set_fact(
                        "confirm_scope_approval_id",
                        json!(approval_id.as_uuid().to_string()),
                    );
                    set_continuation(ctx, Discovery);
                    Reaction::effects(vec![Effect::RequestHumanApproval {
                        approval_id,
                        capability: ToolCapability::GitOperation,
                        description: "Confirm problem scope before proceeding to discovery".into(),
                    }])
                }
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    Reaction::goto(Discovery)
                }
                Event::HumanRejected { .. } => Reaction::goto(ProposeRecoveryOptions),
                _ => Reaction::Super(Intake),
            },

            // =================================================================
            // Discovery leaf states (simplified: entry → LLM; assessment → next)
            // =================================================================
            RequestArtifacts => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "problem_statement": ctx.facts.get("problem_statement"),
                        "constraints": ctx.facts.get("constraints"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Identify what artifacts (files, specs, docs) are needed to understand this problem. Return {\"artifacts\": [{\"name\": \"...\", \"reason\": \"...\"}]}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("artifacts_requested", assessment.clone());
                    Reaction::goto(ClassifyArtifacts)
                }
                Event::LlmFailed { .. } => Reaction::goto(ClassifyArtifacts),
                Event::UserProvidedArtifact { artifact } => {
                    ctx.set_fact("artifacts", artifact.clone());
                    Reaction::goto(ClassifyArtifacts)
                }
                _ => Reaction::Super(Discovery),
            },

            ClassifyArtifacts => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "artifacts": ctx.facts.get("artifacts"),
                        "artifacts_requested": ctx.facts.get("artifacts_requested"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Classify the provided artifacts by type and relevance. Return {\"classified\": [{\"name\": \"...\", \"type\": \"...\", \"relevance\": \"high|medium|low\"}]}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("classified_artifacts", assessment.clone());
                    Reaction::goto(InspectRepository)
                }
                Event::LlmFailed { .. } => Reaction::goto(InspectRepository),
                _ => Reaction::Super(Discovery),
            },

            InspectRepository => match event {
                Event::Internal(Entry) => {
                    let call_id = ToolCallId::new();
                    ctx.set_fact("inspect_tool_call_id", json!(call_id.as_uuid().to_string()));
                    set_continuation(ctx, BuildBaseline);
                    Reaction::effects(vec![Effect::CallTool {
                        call_id,
                        name: "read_repository".into(),
                        capability: ToolCapability::ReadFile,
                        args: json!({}),
                    }])
                }
                Event::ToolSucceeded { observation, .. } => {
                    ctx.set_fact("repository_snapshot", observation.clone());
                    Reaction::goto(BuildBaseline)
                }
                Event::ToolFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Discovery),
            },

            BuildBaseline => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "repository_snapshot": ctx.facts.get("repository_snapshot"),
                        "classified_artifacts": ctx.facts.get("classified_artifacts"),
                        "problem_statement": ctx.facts.get("problem_statement"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Build a baseline understanding of the codebase state. Return {\"baseline\": {\"languages\": [...], \"structure\": \"...\", \"key_components\": [...], \"relevant_files\": [...]}}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("baseline", assessment.clone());
                    Reaction::goto(ExtractTechnicalContext)
                }
                Event::LlmFailed { .. } => Reaction::goto(ExtractTechnicalContext),
                _ => Reaction::Super(Discovery),
            },

            ExtractTechnicalContext => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "baseline": ctx.facts.get("baseline"),
                        "problem_statement": ctx.facts.get("problem_statement"),
                        "constraints": ctx.facts.get("constraints"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Extract technical context needed to implement the solution. Return {\"dependencies\": [...], \"patterns\": [...], \"risks\": [...], \"entry_points\": [...]}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("technical_context", assessment.clone());
                    Reaction::goto(ProduceDiscoverySummary)
                }
                Event::LlmFailed { .. } => Reaction::goto(ProduceDiscoverySummary),
                _ => Reaction::Super(Discovery),
            },

            ProduceDiscoverySummary => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "baseline": ctx.facts.get("baseline"),
                        "technical_context": ctx.facts.get("technical_context"),
                        "classified_artifacts": ctx.facts.get("classified_artifacts"),
                        "problem_statement": ctx.facts.get("problem_statement"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Produce a concise discovery summary ready for planning. Return {\"summary\": \"...\", \"ready_to_plan\": true|false, \"blockers\": [...]}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("discovery_summary", assessment.clone());
                    Reaction::goto(Planning)
                }
                Event::LlmFailed { .. } => Reaction::goto(Planning),
                _ => Reaction::Super(Discovery),
            },

            // =================================================================
            // Planning leaf states
            // =================================================================
            GenerateCandidatePlan => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "discovery_summary": ctx.facts.get("discovery_summary"),
                        "problem_statement": ctx.facts.get("problem_statement"),
                        "constraints": ctx.facts.get("constraints"),
                        "technical_context": ctx.facts.get("technical_context"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "generate_candidate_plan",
                            "known_context": known,
                            "planning_policy": "prefer incremental, testable steps; low risk over speed",
                        }),
                    }])
                }
                Event::LlmProposedPlan { plan } => {
                    ctx.set_fact("candidate_plan", plan.clone());
                    Reaction::goto(DecomposeIntoTasks)
                }
                Event::LlmFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Planning),
            },

            DecomposeIntoTasks => match event {
                Event::Internal(Entry) => {
                    let selected_plan = ctx
                        .facts
                        .get("candidate_plan")
                        .cloned()
                        .unwrap_or(Value::Null);
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "decompose_into_tasks",
                            "selected_plan": selected_plan,
                            "task_policy": "atomic tasks, each verifiable with a single tool call",
                        }),
                    }])
                }
                Event::LlmProposedPlan { plan } => {
                    ctx.set_fact("backlog", plan.clone());
                    Reaction::goto(ValidatePlan)
                }
                Event::LlmFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Planning),
            },

            ValidatePlan => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "backlog": ctx.facts.get("backlog"),
                        "constraints": ctx.facts.get("constraints"),
                        "technical_context": ctx.facts.get("technical_context"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Validate the task backlog against constraints and technical context. Return {\"valid\": true|false, \"issues\": [...], \"suggestions\": [...]}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("plan_validation", assessment.clone());
                    Reaction::goto(EstimateRisk)
                }
                Event::LlmFailed { .. } => Reaction::goto(EstimateRisk),
                _ => Reaction::Super(Planning),
            },

            EstimateRisk => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "backlog": ctx.facts.get("backlog"),
                        "plan_validation": ctx.facts.get("plan_validation"),
                        "technical_context": ctx.facts.get("technical_context"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Estimate risks for the execution plan. Return {\"overall_risk\": \"low|medium|high\", \"risks\": [{\"description\": \"...\", \"mitigation\": \"...\"}]}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("risk_estimate", assessment.clone());
                    Reaction::goto(SelectPlan)
                }
                Event::LlmFailed { .. } => Reaction::goto(SelectPlan),
                _ => Reaction::Super(Planning),
            },

            SelectPlan => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "backlog": ctx.facts.get("backlog"),
                        "risk_estimate": ctx.facts.get("risk_estimate"),
                        "plan_validation": ctx.facts.get("plan_validation"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Select the best execution plan. Return {\"selected\": {\"summary\": \"...\", \"first_task_id\": \"...\"}, \"rationale\": \"...\"}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("selected_plan", assessment.clone());
                    Reaction::goto(ApproveExecutionPlan)
                }
                Event::LlmFailed { .. } => Reaction::goto(ApproveExecutionPlan),
                _ => Reaction::Super(Planning),
            },

            ApproveExecutionPlan => match event {
                Event::Internal(Entry) => {
                    let approval_id = ApprovalId::new();
                    ctx.set_pending_approval(PendingApproval {
                        approval_id,
                        capability: ToolCapability::GitOperation,
                        description: "Approve the execution plan before implementation begins"
                            .into(),
                    });
                    set_continuation(ctx, Execution);
                    Reaction::effects(vec![Effect::RequestHumanApproval {
                        approval_id,
                        capability: ToolCapability::GitOperation,
                        description: "Approve the execution plan before implementation begins"
                            .into(),
                    }])
                }
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    Reaction::goto(Execution)
                }
                Event::HumanRejected { .. } => Reaction::goto(ProposeRecoveryOptions),
                _ => Reaction::Super(Planning),
            },

            // =================================================================
            // Execution leaf states
            // =================================================================
            SelectNextTask => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "backlog": ctx.facts.get("backlog"),
                        "completed_tasks": ctx.facts.get("completed_tasks"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Select the next task to execute from the backlog. Return {\"backlog_empty\": true|false, \"task_id\": \"...\", \"description\": \"...\", \"tool\": \"...\", \"args\": {}}. If all tasks are done, set backlog_empty=true.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    let backlog_empty = assessment
                        .get("backlog_empty")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);

                    if backlog_empty || is_backlog_empty(ctx) {
                        ctx.set_fact("all_tasks_done", json!(true));
                        Reaction::goto(Verification)
                    } else {
                        ctx.set_fact("current_task", assessment.clone());
                        Reaction::goto(PrepareTaskContext)
                    }
                }
                Event::LlmFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Execution),
            },

            PrepareTaskContext => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "current_task": ctx.facts.get("current_task"),
                        "baseline": ctx.facts.get("baseline"),
                        "technical_context": ctx.facts.get("technical_context"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Prepare the code context needed to implement the current task. Return {\"relevant_files\": [...], \"existing_code\": \"...\", \"implementation_notes\": \"...\"}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("task_context", assessment.clone());
                    Reaction::goto(ProposePatch)
                }
                Event::LlmFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Execution),
            },

            ProposePatch => match event {
                Event::Internal(Entry) => {
                    let task = ctx
                        .facts
                        .get("current_task")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let code_context = ctx
                        .facts
                        .get("task_context")
                        .cloned()
                        .unwrap_or(Value::Null);
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "propose_patch",
                            "task": task,
                            "code_context": code_context,
                            "patch_policy": "produce a minimal, correct unified diff; explain each change",
                        }),
                    }])
                }
                Event::LlmProposedPlan { plan } => {
                    ctx.set_fact("proposed_patch", plan.clone());
                    let call_id = ToolCallId::new();
                    ctx.set_fact("apply_patch_call_id", json!(call_id.as_uuid().to_string()));
                    Reaction::transition(
                        ApplyPatch,
                        vec![Effect::CallTool {
                            call_id,
                            name: "apply_patch".into(),
                            capability: ToolCapability::WriteFile,
                            args: plan.clone(),
                        }],
                        "patch proposed; applying to workspace",
                    )
                }
                Event::LlmFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Execution),
            },

            ApplyPatch => match event {
                Event::ToolSucceeded { .. } => Reaction::goto(Build),
                Event::ToolFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Execution),
            },

            Build => match event {
                Event::Internal(Entry) => {
                    let call_id = ToolCallId::new();
                    ctx.set_fact("build_call_id", json!(call_id.as_uuid().to_string()));
                    Reaction::effects(vec![Effect::CallTool {
                        call_id,
                        name: "build".into(),
                        capability: ToolCapability::ExecuteShell,
                        args: json!({}),
                    }])
                }
                Event::ToolSucceeded { .. } => Reaction::goto(RunTests),
                Event::ToolFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Execution),
            },

            RunTests => match event {
                Event::Internal(Entry) => {
                    let call_id = ToolCallId::new();
                    ctx.set_fact("test_call_id", json!(call_id.as_uuid().to_string()));
                    Reaction::effects(vec![Effect::CallTool {
                        call_id,
                        name: "test".into(),
                        capability: ToolCapability::ExecuteShell,
                        args: json!({}),
                    }])
                }
                Event::ToolSucceeded { .. } => {
                    ctx.set_fact("tests_passed", json!(true));
                    Reaction::goto(StaticAnalysis)
                }
                Event::ToolFailed { .. } => {
                    ctx.set_fact("tests_passed", json!(false));
                    Reaction::goto(ClassifyFailure)
                }
                _ => Reaction::Super(Execution),
            },

            StaticAnalysis => match event {
                Event::Internal(Entry) => {
                    let call_id = ToolCallId::new();
                    ctx.set_fact(
                        "static_analysis_call_id",
                        json!(call_id.as_uuid().to_string()),
                    );
                    Reaction::effects(vec![Effect::CallTool {
                        call_id,
                        name: "static_analysis".into(),
                        capability: ToolCapability::ExecuteShell,
                        args: json!({}),
                    }])
                }
                Event::ToolSucceeded { .. } => Reaction::goto(ObserveResult),
                Event::ToolFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Execution),
            },

            ObserveResult => match event {
                Event::Internal(Entry) => {
                    let raw = ctx
                        .facts
                        .get("last_tool_output")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let task = ctx
                        .facts
                        .get("current_task")
                        .cloned()
                        .unwrap_or(Value::Null);
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "structure_tool_observation",
                            "task": task,
                            "raw_output": raw,
                            "expected_observation": "{\"success\": true|false, \"summary\": \"...\", \"details\": {...}}",
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("observation", assessment.clone());
                    Reaction::goto(DecideTaskOutcome)
                }
                Event::LlmFailed { .. } => Reaction::goto(DecideTaskOutcome),
                _ => Reaction::Super(Execution),
            },

            DecideTaskOutcome => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "current_task": ctx.facts.get("current_task"),
                        "observation": ctx.facts.get("observation"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Decide if the current task is complete based on the observation. Return {\"task_complete\": true|false, \"reason\": \"...\"}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    let task_complete = assessment
                        .get("task_complete")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);

                    if task_complete {
                        mark_task_complete(ctx);
                        Reaction::goto(SelectNextTask)
                    } else {
                        ctx.bump_retry("task_retry");
                        Reaction::goto(ClassifyFailure)
                    }
                }
                Event::LlmFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Execution),
            },

            // =================================================================
            // Verification leaf states
            // =================================================================
            VerifyRequirements => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "problem_statement": ctx.facts.get("problem_statement"),
                        "constraints": ctx.facts.get("constraints"),
                        "completed_tasks": ctx.facts.get("completed_tasks"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Verify all requirements have been satisfied. Return {\"requirements_met\": true|false, \"unmet\": [...], \"notes\": \"...\"}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("requirements_verification", assessment.clone());
                    Reaction::goto(VerifyTests)
                }
                Event::LlmFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Verification),
            },

            VerifyTests => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "requirements_verification": ctx.facts.get("requirements_verification"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Verify the test suite adequately covers the changes. Return {\"tests_ok\": true|false, \"coverage_notes\": \"...\", \"missing_tests\": [...]}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    let ok = assessment
                        .get("tests_ok")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if ok {
                        Reaction::goto(VerifySecurityBoundaries)
                    } else {
                        Reaction::goto(ClassifyFailure)
                    }
                }
                Event::LlmFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Verification),
            },

            VerifySecurityBoundaries => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "baseline": ctx.facts.get("baseline"),
                        "completed_tasks": ctx.facts.get("completed_tasks"),
                        "technical_context": ctx.facts.get("technical_context"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Verify no security boundaries were violated by the changes. Return {\"secure\": true|false, \"issues\": [...], \"recommendations\": [...]}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("security_verification", assessment.clone());
                    Reaction::goto(VerifyRegressionRisk)
                }
                Event::LlmFailed { .. } => Reaction::goto(HumanAcceptanceGate),
                _ => Reaction::Super(Verification),
            },

            VerifyRegressionRisk => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "security_verification": ctx.facts.get("security_verification"),
                        "completed_tasks": ctx.facts.get("completed_tasks"),
                        "baseline": ctx.facts.get("baseline"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Assess regression risk from the changes. Return {\"regression_risk\": \"low|medium|high\", \"affected_areas\": [...], \"mitigation\": \"...\"}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("regression_risk", assessment.clone());
                    Reaction::goto(HumanAcceptanceGate)
                }
                Event::LlmFailed { .. } => Reaction::goto(HumanAcceptanceGate),
                _ => Reaction::Super(Verification),
            },

            HumanAcceptanceGate => match event {
                Event::Internal(Entry) => {
                    let approval_id = ApprovalId::new();
                    ctx.set_pending_approval(PendingApproval {
                        approval_id,
                        capability: ToolCapability::GitOperation,
                        description: "Human acceptance: review implementation results".into(),
                    });
                    set_continuation(ctx, Delivery);
                    Reaction::effects(vec![Effect::RequestHumanApproval {
                        approval_id,
                        capability: ToolCapability::GitOperation,
                        description: "Human acceptance: review implementation results".into(),
                    }])
                }
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    ctx.set_fact("human_acceptance_obtained", json!(true));
                    Reaction::goto(Delivery)
                }
                Event::HumanRejected { .. } => Reaction::goto(ProposeRecoveryOptions),
                _ => Reaction::Super(Verification),
            },

            // =================================================================
            // Delivery leaf states
            // =================================================================
            ProduceTechnicalSummary => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "problem_statement": ctx.facts.get("problem_statement"),
                        "completed_tasks": ctx.facts.get("completed_tasks"),
                        "requirements_verification": ctx.facts.get("requirements_verification"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Produce a technical delivery summary for engineers. Return {\"summary\": \"...\", \"changes\": [...], \"test_coverage\": \"...\", \"known_limitations\": [...]}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("technical_summary", assessment.clone());
                    Reaction::goto(ProduceUserInstructions)
                }
                Event::LlmFailed { .. } => Reaction::goto(ProduceUserInstructions),
                _ => Reaction::Super(Delivery),
            },

            ProduceUserInstructions => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "technical_summary": ctx.facts.get("technical_summary"),
                        "problem_statement": ctx.facts.get("problem_statement"),
                        "user_request": ctx.facts.get("user_request"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Produce user-facing instructions for the delivered changes. Return {\"instructions\": \"...\", \"steps\": [...], \"notes\": \"...\"}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("user_instructions", assessment.clone());
                    Reaction::goto(PackageArtifacts)
                }
                Event::LlmFailed { .. } => Reaction::goto(PackageArtifacts),
                _ => Reaction::Super(Delivery),
            },

            PackageArtifacts => match event {
                Event::Internal(Entry) => {
                    let call_id = ToolCallId::new();
                    ctx.set_fact("package_call_id", json!(call_id.as_uuid().to_string()));
                    Reaction::effects(vec![Effect::CallTool {
                        call_id,
                        name: "package".into(),
                        capability: ToolCapability::ExecuteShell,
                        args: json!({}),
                    }])
                }
                Event::ToolSucceeded { .. } => Reaction::goto(FinalApproval),
                Event::ToolFailed { .. } => Reaction::goto(ClassifyFailure),
                _ => Reaction::Super(Delivery),
            },

            FinalApproval => match event {
                Event::Internal(Entry) => {
                    let approval_id = ApprovalId::new();
                    ctx.set_pending_approval(PendingApproval {
                        approval_id,
                        capability: ToolCapability::ExecuteShell,
                        description: "Final approval: deploy to production".into(),
                    });
                    set_continuation(ctx, Done);
                    Reaction::effects(vec![Effect::RequestHumanApproval {
                        approval_id,
                        capability: ToolCapability::ExecuteShell,
                        description: "Final approval: deploy to production".into(),
                    }])
                }
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    ctx.set_fact("human_acceptance_obtained", json!(true));
                    Reaction::goto(Done)
                }
                Event::HumanRejected { .. } => Reaction::goto(ProposeRecoveryOptions),
                _ => Reaction::Super(Delivery),
            },

            // =================================================================
            // Recovery leaf states
            // =================================================================
            ClassifyFailure => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "failure_classification": ctx.facts.get("failure_classification"),
                        "last_tool_output": ctx.facts.get("last_tool_output"),
                        "current_task": ctx.facts.get("current_task"),
                        "retry_count": ctx.retry_counters.get("task_retry"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Classify the failure to determine recovery strategy. Return {\"failure_type\": \"tool_error|llm_error|logic_error|user_error\", \"severity\": \"low|medium|high\", \"recoverable\": true|false, \"description\": \"...\"}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("failure_classification", assessment.clone());
                    Reaction::goto(ProposeRecoveryOptions)
                }
                Event::LlmFailed { .. } => Reaction::goto(ProposeRecoveryOptions),
                _ => Reaction::Super(Recovery),
            },

            ProposeRecoveryOptions => match event {
                Event::Internal(Entry) => {
                    let failure = ctx
                        .facts
                        .get("failure_classification")
                        .and_then(|v| v.get("description"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown failure")
                        .to_string();
                    let known = serde_json::json!({
                        "failure_classification": ctx.facts.get("failure_classification"),
                        "retry_count": ctx.retry_counters.get("recovery_retry"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "propose_recovery_options",
                            "failure": failure,
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("recovery_options", assessment.clone());
                    Reaction::goto(SelectRecoveryAction)
                }
                Event::LlmFailed { .. } => Reaction::goto(AskUserForDecision),
                _ => Reaction::Super(Recovery),
            },

            SelectRecoveryAction => match event {
                Event::Internal(Entry) => {
                    let known = serde_json::json!({
                        "recovery_options": ctx.facts.get("recovery_options"),
                        "failure_classification": ctx.facts.get("failure_classification"),
                        "retry_count": ctx.retry_counters.get("recovery_retry"),
                    });
                    Reaction::effects(vec![Effect::CallLlm {
                        request: json!({
                            "kind": "evaluate_context",
                            "goal": "Select the best recovery action. Return {\"action\": \"retry|rollback|ask_user|escalate\", \"rationale\": \"...\"}.",
                            "known_context": known,
                        }),
                    }])
                }
                Event::LlmProposedAssessment { assessment } => {
                    let action = assessment
                        .get("action")
                        .and_then(Value::as_str)
                        .unwrap_or("retry");
                    match action {
                        "retry" => Reaction::goto(Retry),
                        "rollback" => Reaction::goto(Rollback),
                        "ask_user" => Reaction::goto(AskUserForDecision),
                        "escalate" => Reaction::goto(EscalateToHuman),
                        _ => Reaction::goto(Failed),
                    }
                }
                Event::LlmFailed { .. } => Reaction::goto(AskUserForDecision),
                _ => Reaction::Super(Recovery),
            },

            Retry => match event {
                Event::Internal(Entry) => {
                    ctx.bump_retry("recovery_retry");
                    Reaction::effects(vec![Effect::EmitInternal {
                        name: "do_retry".into(),
                        payload: json!({}),
                    }])
                }
                Event::Internal(InternalEvent::Custom { name, .. }) if name == "do_retry" => {
                    Reaction::goto(resolve_continuation(ctx))
                }
                _ => Reaction::Super(Recovery),
            },

            Rollback => match event {
                Event::Internal(Entry) => {
                    let label = ctx
                        .checkpoints
                        .last()
                        .cloned()
                        .unwrap_or_else(|| "pre-cancel".into());
                    Reaction::effects(vec![
                        Effect::RollbackToCheckpoint { label },
                        Effect::EmitInternal {
                            name: "rollback_done".into(),
                            payload: json!({}),
                        },
                    ])
                }
                Event::Internal(InternalEvent::Custom { name, .. }) if name == "rollback_done" => {
                    Reaction::goto(Cancelled)
                }
                _ => Reaction::Super(Recovery),
            },

            AskUserForDecision => match event {
                Event::Internal(Entry) => Reaction::effects(vec![Effect::AskUser {
                    prompt: "Recovery stalled. Please decide how to proceed.".into(),
                }]),
                Event::UserMessage { text } => {
                    ctx.set_fact("user_recovery_decision", json!(text));
                    Reaction::goto(ProposeRecoveryOptions)
                }
                _ => Reaction::Super(Recovery),
            },

            EscalateToHuman => match event {
                Event::Internal(Entry) => {
                    let approval_id = ApprovalId::new();
                    ctx.set_pending_approval(PendingApproval {
                        approval_id,
                        capability: ToolCapability::GitOperation,
                        description: "Escalation: human intervention required".into(),
                    });
                    Reaction::effects(vec![Effect::RequestHumanApproval {
                        approval_id,
                        capability: ToolCapability::GitOperation,
                        description: "Escalation: human intervention required".into(),
                    }])
                }
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    Reaction::goto(Failed)
                }
                Event::HumanRejected { .. } => Reaction::goto(Failed),
                _ => Reaction::Super(Recovery),
            },

            // =================================================================
            // Semi-terminal: RollingBack
            // =================================================================
            RollingBack => match event {
                Event::Internal(Entry) => Reaction::effects(vec![Effect::EmitInternal {
                    name: "rollback_complete".into(),
                    payload: json!({}),
                }]),
                Event::Internal(InternalEvent::Custom { name, .. })
                    if name == "rollback_complete" =>
                {
                    Reaction::goto(Cancelled)
                }
                _ => Reaction::Super(Top),
            },

            // =================================================================
            // Terminal states – absorb all events
            // =================================================================
            Done | Failed | Cancelled => Reaction::Ignored,
        }
    }

    fn is_terminal(&self, state: SdmState) -> bool {
        matches!(
            state,
            SdmState::Done | SdmState::Failed | SdmState::Cancelled
        )
    }

    fn all_states(&self) -> Vec<SdmState> {
        use SdmState::*;
        vec![
            Top,
            Intake,
            Idle,
            InterpretUserIntent,
            ExtractProblemStatement,
            ExtractConstraints,
            AssessInformationCompleteness,
            ConfirmScope,
            Discovery,
            RequestArtifacts,
            ClassifyArtifacts,
            InspectRepository,
            BuildBaseline,
            ExtractTechnicalContext,
            ProduceDiscoverySummary,
            Planning,
            GenerateCandidatePlan,
            DecomposeIntoTasks,
            ValidatePlan,
            EstimateRisk,
            SelectPlan,
            ApproveExecutionPlan,
            Execution,
            SelectNextTask,
            PrepareTaskContext,
            ProposePatch,
            ApplyPatch,
            Build,
            RunTests,
            StaticAnalysis,
            ObserveResult,
            DecideTaskOutcome,
            Verification,
            VerifyRequirements,
            VerifyTests,
            VerifySecurityBoundaries,
            VerifyRegressionRisk,
            HumanAcceptanceGate,
            Delivery,
            ProduceTechnicalSummary,
            ProduceUserInstructions,
            PackageArtifacts,
            FinalApproval,
            AwaitUser,
            AwaitHumanApproval,
            AwaitTool,
            Recovery,
            ClassifyFailure,
            ProposeRecoveryOptions,
            SelectRecoveryAction,
            Retry,
            Rollback,
            AskUserForDecision,
            EscalateToHuman,
            RollingBack,
            Done,
            Failed,
            Cancelled,
        ]
    }
}

// =============================================================================
// Context helpers
// =============================================================================

fn is_backlog_empty(ctx: &Context) -> bool {
    match ctx.facts.get("backlog") {
        None => true,
        Some(v) => v.as_array().map(Vec::is_empty).unwrap_or(true),
    }
}

fn mark_task_complete(ctx: &mut Context) {
    let backlog = ctx.facts.get_mut("backlog").and_then(Value::as_array_mut);

    if let Some(arr) = backlog {
        if !arr.is_empty() {
            arr.remove(0);
        }
    }

    let remaining = ctx
        .facts
        .get("backlog")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);

    if remaining == 0 {
        ctx.set_fact("all_tasks_done", json!(true));
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::{dispatch::Hsm, effect::EffectKind, ids::ToolCallId};

    fn make_hsm() -> (Hsm<SoftwareDevelopmentMachine>, Context) {
        let mut hsm = Hsm::new(SoftwareDevelopmentMachine::new());
        let mut ctx = Context::new();
        hsm.init(&mut ctx);
        (hsm, ctx)
    }

    // ------------------------------------------------------------------
    // Initial state
    // ------------------------------------------------------------------

    #[test]
    fn initial_state_is_idle() {
        let (hsm, _) = make_hsm();
        // The machine now waits in Idle for the first UserMessage before
        // beginning the intake process.  This prevents LLM calls on startup.
        assert_eq!(hsm.state(), SdmState::Idle);
    }

    #[test]
    fn user_message_transitions_idle_to_interpret_user_intent() {
        let (mut hsm, mut ctx) = make_hsm();
        assert_eq!(hsm.state(), SdmState::Idle);

        let out = hsm.dispatch(&Event::UserMessage { text: "add feature X".into() }, &mut ctx);
        assert_eq!(hsm.state(), SdmState::InterpretUserIntent);
        // The transition to InterpretUserIntent must emit a CallLlm effect.
        assert!(out.effects.iter().any(|e| e.kind() == EffectKind::CallLlm),
            "expected CallLlm after transition to InterpretUserIntent: {:?}", out.effects);
    }

    // ------------------------------------------------------------------
    // UserCancelled from deep leaf → RollingBack (cross-superstate)
    // ------------------------------------------------------------------

    #[test]
    fn user_cancelled_from_interpret_user_intent_reaches_rolling_back() {
        let (mut hsm, mut ctx) = make_hsm();
        // Transition through Idle first.
        hsm.dispatch(&Event::UserMessage { text: "some request".into() }, &mut ctx);
        assert_eq!(hsm.state(), SdmState::InterpretUserIntent);

        let out = hsm.dispatch(&Event::UserCancelled, &mut ctx);
        assert_eq!(hsm.state(), SdmState::RollingBack);
        assert!(out.transitioned);
        // The transition action must include a checkpoint.
        assert!(out
            .effects
            .iter()
            .any(|e| e.kind() == EffectKind::CreateCheckpoint));
    }

    // ------------------------------------------------------------------
    // Cross-superstate exit/entry ordering
    //
    // Drive to ConfirmScope (under Intake) then reject → ProposeRecoveryOptions
    // (under Recovery, direct child of Top).
    //
    // LCA of ConfirmScope and ProposeRecoveryOptions is Top.
    // Expected effect ordering:
    //   exit:  ConfirmScope (nothing), Intake (PersistAudit)
    //   enter: Recovery (PersistAudit), ProposeRecoveryOptions (CallLlm)
    // ------------------------------------------------------------------

    /// Drive the Hsm through the four Intake states and stop at ConfirmScope.
    fn drive_to_confirm_scope(hsm: &mut Hsm<SoftwareDevelopmentMachine>, ctx: &mut Context) {
        // Machine starts in Idle; send a user message to begin intake.
        hsm.dispatch(&Event::UserMessage { text: "implement feature X".into() }, ctx);
        assert_eq!(hsm.state(), SdmState::InterpretUserIntent);
        for assessment in [
            json!({ "intent": "implement feature X" }),
            json!({ "problem": "users need feature X" }),
            json!({ "constraints": [] }),
            json!({ "status": { "status": "enough" } }),
        ] {
            hsm.dispatch(&Event::LlmProposedAssessment { assessment }, ctx);
        }
        assert_eq!(hsm.state(), SdmState::ConfirmScope);
    }

    #[test]
    fn cross_superstate_confirm_scope_rejection_fires_intake_exit_and_recovery_entry() {
        let (mut hsm, mut ctx) = make_hsm();
        drive_to_confirm_scope(&mut hsm, &mut ctx);

        let approval_id = ctx
            .pending_approval
            .as_ref()
            .map(|p| p.approval_id)
            .unwrap();

        // HumanRejected on ConfirmScope → ProposeRecoveryOptions directly
        // (Intake → Recovery, crossing Top as LCA).
        let out = hsm.dispatch(&Event::HumanRejected { approval_id }, &mut ctx);

        // ConfirmScope's handler goes directly to ProposeRecoveryOptions (not
        // ClassifyFailure), so drilling stops there since it is a leaf state.
        assert_eq!(hsm.state(), SdmState::ProposeRecoveryOptions);

        // The effects must contain:
        //   PersistAudit (Intake exit) + PersistAudit (Recovery entry)
        //   + CallLlm (ProposeRecoveryOptions entry)
        let audit_count = out
            .effects
            .iter()
            .filter(|e| e.kind() == EffectKind::PersistAudit)
            .count();
        assert_eq!(
            audit_count, 2,
            "expected 2 PersistAudit (Intake exit + Recovery entry); got {audit_count}: {:?}",
            out.effects
        );

        let llm_count = out
            .effects
            .iter()
            .filter(|e| e.kind() == EffectKind::CallLlm)
            .count();
        assert!(
            llm_count >= 1,
            "expected ProposeRecoveryOptions entry CallLlm"
        );

        // Verify ordering: PersistAudit from Intake exit appears BEFORE the
        // PersistAudit from Recovery entry (exits precede entries in the effect vec).
        let audit_indices: Vec<_> = out
            .effects
            .iter()
            .enumerate()
            .filter(|(_, e)| e.kind() == EffectKind::PersistAudit)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(audit_indices.len(), 2);
        assert!(
            audit_indices[0] < audit_indices[1],
            "first PersistAudit (Intake exit) must precede second (Recovery entry)"
        );
    }

    #[test]
    fn cross_superstate_direct_build_to_classify_failure() {
        // Verify the transition reaction for Build → ClassifyFailure directly.
        let mut m = SoftwareDevelopmentMachine::new();
        let mut ctx = Context::new();

        let reaction = m.dispatch_state(
            SdmState::Build,
            &Event::ToolFailed {
                call_id: ToolCallId::new(),
                error: "undefined symbol".into(),
            },
            &mut ctx,
        );
        assert!(
            matches!(
                reaction,
                Reaction::Transition {
                    target: SdmState::ClassifyFailure,
                    ..
                }
            ),
            "Build + ToolFailed must transition to ClassifyFailure"
        );
    }

    // ------------------------------------------------------------------
    // Intake path: full happy-path smoke test
    // ------------------------------------------------------------------

    #[test]
    fn intake_happy_path() {
        let (mut hsm, mut ctx) = make_hsm();

        // Machine starts in Idle; send UserMessage to begin intake.
        hsm.dispatch(&Event::UserMessage { text: "add feature X".into() }, &mut ctx);
        assert_eq!(hsm.state(), SdmState::InterpretUserIntent);

        // InterpretUserIntent → assessment → ExtractProblemStatement
        hsm.dispatch(
            &Event::LlmProposedAssessment {
                assessment: json!({ "intent": "add feature X" }),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), SdmState::ExtractProblemStatement);

        // ExtractProblemStatement → ExtractConstraints
        hsm.dispatch(
            &Event::LlmProposedAssessment {
                assessment: json!({ "problem": "users need feature X" }),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), SdmState::ExtractConstraints);

        // ExtractConstraints → AssessInformationCompleteness
        hsm.dispatch(
            &Event::LlmProposedAssessment {
                assessment: json!({ "constraints": ["no breaking changes"] }),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), SdmState::AssessInformationCompleteness);

        // AssessInformationCompleteness(enough) → ConfirmScope
        let out = hsm.dispatch(
            &Event::LlmProposedAssessment {
                assessment: json!({ "status": { "status": "enough" } }),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), SdmState::ConfirmScope);
        // ConfirmScope entry emits RequestHumanApproval
        assert!(out
            .effects
            .iter()
            .any(|e| e.kind() == EffectKind::RequestHumanApproval));

        // Human approves scope → Discovery (then drills to RequestArtifacts)
        let approval_id = ctx
            .pending_approval
            .as_ref()
            .map(|p| p.approval_id)
            .unwrap();
        hsm.dispatch(&Event::HumanApproved { approval_id }, &mut ctx);
        assert_eq!(hsm.state(), SdmState::RequestArtifacts);
    }

    // ------------------------------------------------------------------
    // Intake: missing info → AwaitUser
    // ------------------------------------------------------------------

    #[test]
    fn assess_completeness_missing_enters_await_user() {
        let (mut hsm, mut ctx) = make_hsm();

        // Machine starts in Idle; send UserMessage to begin intake.
        hsm.dispatch(&Event::UserMessage { text: "?".into() }, &mut ctx);

        // Drive to AssessInformationCompleteness
        hsm.dispatch(
            &Event::LlmProposedAssessment {
                assessment: json!({ "intent": "?" }),
            },
            &mut ctx,
        );
        hsm.dispatch(
            &Event::LlmProposedAssessment {
                assessment: json!({ "problem": "?" }),
            },
            &mut ctx,
        );
        hsm.dispatch(
            &Event::LlmProposedAssessment {
                assessment: json!({ "constraints": [] }),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), SdmState::AssessInformationCompleteness);

        let out = hsm.dispatch(
            &Event::LlmProposedAssessment {
                assessment: json!({
                    "status": {
                        "status": "missing",
                        "fields": [{"field": "environment", "reason": "Which environment?"}],
                    },
                }),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), SdmState::AwaitUser);
        assert!(out.effects.iter().any(|e| e.kind() == EffectKind::AskUser));
        // Continuation was stored
        assert_eq!(
            ctx.facts.get("continuation").and_then(|v| v.as_str()),
            Some("AssessInformationCompleteness"),
        );
    }

    // ------------------------------------------------------------------
    // ConfirmScope human rejection → Recovery
    // ------------------------------------------------------------------

    #[test]
    fn confirm_scope_rejection_enters_recovery() {
        let (mut hsm, mut ctx) = make_hsm();
        // Machine starts in Idle; send UserMessage first.
        hsm.dispatch(&Event::UserMessage { text: "x".into() }, &mut ctx);
        // Drive to ConfirmScope
        for assessment in [
            json!({ "intent": "x" }),
            json!({ "problem": "x" }),
            json!({ "constraints": [] }),
            json!({ "status": { "status": "enough" } }),
        ] {
            hsm.dispatch(&Event::LlmProposedAssessment { assessment }, &mut ctx);
        }
        assert_eq!(hsm.state(), SdmState::ConfirmScope);

        let approval_id = ctx
            .pending_approval
            .as_ref()
            .map(|p| p.approval_id)
            .unwrap();
        hsm.dispatch(&Event::HumanRejected { approval_id }, &mut ctx);
        // ConfirmScope's HumanRejected handler goes directly to ProposeRecoveryOptions.
        assert_eq!(hsm.state(), SdmState::ProposeRecoveryOptions);
    }

    // ------------------------------------------------------------------
    // Execution: Build → RunTests / ClassifyFailure
    // ------------------------------------------------------------------

    #[test]
    fn build_tool_succeeded_enters_run_tests() {
        let mut m = SoftwareDevelopmentMachine::new();
        let mut ctx = Context::new();

        let r = m.dispatch_state(
            SdmState::Build,
            &Event::ToolSucceeded {
                call_id: ToolCallId::new(),
                observation: json!({ "exit_code": 0 }),
            },
            &mut ctx,
        );
        assert!(matches!(
            r,
            Reaction::Transition {
                target: SdmState::RunTests,
                ..
            }
        ));
    }

    #[test]
    fn build_tool_failed_enters_classify_failure() {
        let mut m = SoftwareDevelopmentMachine::new();
        let mut ctx = Context::new();

        let r = m.dispatch_state(
            SdmState::Build,
            &Event::ToolFailed {
                call_id: ToolCallId::new(),
                error: "undefined symbol".into(),
            },
            &mut ctx,
        );
        assert!(matches!(
            r,
            Reaction::Transition {
                target: SdmState::ClassifyFailure,
                ..
            }
        ));
    }

    // ------------------------------------------------------------------
    // Execution: SelectNextTask + backlog empty → Verification
    // ------------------------------------------------------------------

    #[test]
    fn select_next_task_backlog_empty_assessment_enters_verification() {
        let mut m = SoftwareDevelopmentMachine::new();
        let mut ctx = Context::new();

        let r = m.dispatch_state(
            SdmState::SelectNextTask,
            &Event::LlmProposedAssessment {
                assessment: json!({ "backlog_empty": true }),
            },
            &mut ctx,
        );
        assert!(matches!(
            r,
            Reaction::Transition {
                target: SdmState::Verification,
                ..
            }
        ));
    }

    #[test]
    fn select_next_task_empty_backlog_in_ctx_enters_verification() {
        let mut m = SoftwareDevelopmentMachine::new();
        let mut ctx = Context::new();
        // No backlog key at all → treated as empty
        let r = m.dispatch_state(
            SdmState::SelectNextTask,
            &Event::LlmProposedAssessment {
                assessment: json!({ "selected_task": null }),
            },
            &mut ctx,
        );
        assert!(matches!(
            r,
            Reaction::Transition {
                target: SdmState::Verification,
                ..
            }
        ));
    }

    // ------------------------------------------------------------------
    // AwaitUser timeout retry loop
    // ------------------------------------------------------------------

    #[test]
    fn await_user_timeout_re_asks_until_limit() {
        let mut m = SoftwareDevelopmentMachine::new();
        let mut ctx = Context::new();
        ctx.set_fact("await_user_prompt", json!("Please answer."));
        ctx.set_fact("continuation", json!("SelectNextTask"));

        use sven_hsm::ids::TimerId;

        // First two timeouts → re-ask (Handled, no transition)
        for i in 1..=2_u32 {
            let r = m.dispatch_state(
                SdmState::AwaitUser,
                &Event::Timeout {
                    timer_id: TimerId::new(),
                },
                &mut ctx,
            );
            assert!(
                matches!(r, Reaction::Handled(ref effs) if effs.iter().any(|e| e.kind() == EffectKind::AskUser)),
                "attempt {i} should re-ask"
            );
        }

        // Third timeout → AskUserForDecision
        let r = m.dispatch_state(
            SdmState::AwaitUser,
            &Event::Timeout {
                timer_id: TimerId::new(),
            },
            &mut ctx,
        );
        assert!(
            matches!(
                r,
                Reaction::Transition {
                    target: SdmState::AskUserForDecision,
                    ..
                }
            ),
            "third timeout should escalate to AskUserForDecision"
        );
    }

    // ------------------------------------------------------------------
    // AwaitUser resume continuation
    // ------------------------------------------------------------------

    #[test]
    fn await_user_llm_assessment_resumes_continuation() {
        let mut m = SoftwareDevelopmentMachine::new();
        let mut ctx = Context::new();
        ctx.set_fact("continuation", json!("AssessInformationCompleteness"));

        let r = m.dispatch_state(
            SdmState::AwaitUser,
            &Event::LlmProposedAssessment {
                assessment: json!({ "answer": "production" }),
            },
            &mut ctx,
        );
        assert!(matches!(
            r,
            Reaction::Transition {
                target: SdmState::AssessInformationCompleteness,
                ..
            }
        ));
    }

    // ------------------------------------------------------------------
    // Terminal states
    // ------------------------------------------------------------------

    #[test]
    fn done_failed_cancelled_are_terminal() {
        let m = SoftwareDevelopmentMachine::new();
        assert!(m.is_terminal(SdmState::Done));
        assert!(m.is_terminal(SdmState::Failed));
        assert!(m.is_terminal(SdmState::Cancelled));
        assert!(!m.is_terminal(SdmState::Build));
        assert!(!m.is_terminal(SdmState::RollingBack));
    }

    // ------------------------------------------------------------------
    // Superstate hierarchy sanity
    // ------------------------------------------------------------------

    #[test]
    fn superstate_chain_reaches_top_for_all_leaves() {
        let m = SoftwareDevelopmentMachine::new();
        for &state in &[
            SdmState::InterpretUserIntent,
            SdmState::Build,
            SdmState::RunTests,
            SdmState::ClassifyFailure,
            SdmState::FinalApproval,
            SdmState::AwaitUser,
        ] {
            let mut cur = state;
            let mut steps = 0;
            loop {
                let parent = m.superstate(cur);
                if parent == cur {
                    break;
                }
                cur = parent;
                steps += 1;
                assert!(steps < 10, "superstate chain too deep for {state:?}");
            }
            assert_eq!(cur, SdmState::Top);
        }
    }
}
