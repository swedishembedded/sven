//! `sven-executors` — effect executors for the Sven HSM runtime.
//!
//! # Purpose
//!
//! Implements [`sven_kernel::EffectExecutor`] for every [`sven_hsm::Effect`]
//! variant.  Each executor receives an `Effect`, performs the corresponding
//! I/O on a tokio task, and sends the resulting [`sven_hsm::Event`]s back to
//! the kernel queue via [`sven_kernel::EventSink`].
//!
//! # Module map
//!
//! | Module | Effect(s) handled | Events emitted |
//! |--------|-------------------|----------------|
//! | [`turn`] | `CallLlm` (kind=turn) | `LlmTurnComplete`, `LlmFailed` |
//! | [`tool`] | `CallTool` | `ToolSucceeded`, `ToolFailed` |
//! | [`user`] | `AskUser`, `RequestHumanApproval` | `UserMessage`, `HumanApproved`, `HumanRejected` |
//! | [`timer`] | `ScheduleTimeout`, `CancelTimeout` | `Timeout` |
//! | [`audit`] | `PersistAudit` | *(none — appends to log file)* |
//! | [`internal`] | `EmitInternal` | `Internal::Custom` |
//! | [`verify`] | `Verify` | `VerificationComplete` |
//! | [`composite`] | all of the above | delegates |
//!
//! # Quickstart
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use sven_executors::composite::CompositeExecutor;
//! use sven_hsm::{Hsm, Context, PermissionPolicy};
//! use sven_kernel::ErasedRuntime;
//!
//! let executor = CompositeExecutor::builder()
//!     .with_turn(turn_executor)
//!     .with_tools(registry, Default::default())
//!     .with_user(question_tx, approval_tx)
//!     .with_timers(Arc::new(sven_kernel::SystemClock::new()))
//!     .with_audit("/var/log/sven/audit.jsonl")
//!     .build();
//!
//! let runtime = ErasedRuntime::spawn(Box::new(Hsm::new(machine)), ctx, policy, executor, 64);
//! ```

mod argument_check;
#[cfg(not(target_arch = "wasm32"))]
pub mod audit;
pub mod chunks;
pub mod composite;
pub mod internal;
mod notices;
pub mod thread_store;
pub mod timer;
pub mod tool;
pub mod turn;
pub mod user;
#[cfg(not(target_arch = "wasm32"))]
pub mod verify;

// Re-exports
#[cfg(not(target_arch = "wasm32"))]
pub use audit::{
    append_chain, read_chain, verify_chain, AuditExecutor, ChainError, ChainedLine, GENESIS_HASH,
};
pub use chunks::{Assembled, CacheContext, ChunkSession, ContextAssembler};
pub use composite::{CompositeExecutor, CompositeExecutorBuilder};
pub use internal::InternalExecutor;
pub use thread_store::{GateJournal, GateVerdict, ThreadId, ThreadStore};
pub use timer::TimerExecutor;
pub use tool::ToolExecutor;
pub use turn::{CompactionConfig, CostPolicy, TurnExecutor};
pub use user::{ApprovalRequest, Preapproval, UserExecutor, UserQuestion};
#[cfg(not(target_arch = "wasm32"))]
pub use verify::VerifyExecutor;
