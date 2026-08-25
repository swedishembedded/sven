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
//! | [`checkpoint`] | `CreateCheckpoint`, `RollbackToCheckpoint` | `Internal::Custom` |
//! | [`audit`] | `PersistAudit` | *(none — appends to log file)* |
//! | [`internal`] | `EmitInternal` | `Internal::Custom` |
//! | [`composite`] | all of the above | delegates |
//!
//! # Quickstart
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use sven_executors::composite::CompositeExecutor;
//! use sven_hsm::{Hsm, Context, PermissionPolicy};
//! use sven_kernel::Runtime;
//!
//! let executor = CompositeExecutor::builder()
//!     .with_turn(turn_executor)
//!     .with_tools(registry, Default::default())
//!     .with_user(question_tx, approval_tx)
//!     .with_timers(Arc::new(sven_kernel::SystemClock::new()))
//!     .with_checkpoints("/path/to/repo")
//!     .with_audit("/var/log/sven/audit.jsonl")
//!     .build();
//!
//! let runtime = Runtime::spawn(Hsm::new(machine), ctx, policy, executor, 64);
//! ```

pub mod audit;
pub mod checkpoint;
pub mod composite;
pub mod internal;
pub mod timer;
pub mod tool;
pub mod turn;
pub mod user;

// Re-exports
pub use audit::{
    append_chain, read_chain, verify_chain, AuditExecutor, ChainError, ChainedLine, GENESIS_HASH,
};
pub use checkpoint::CheckpointExecutor;
pub use composite::{CompositeExecutor, CompositeExecutorBuilder};
pub use internal::InternalExecutor;
pub use timer::TimerExecutor;
pub use tool::ToolExecutor;
pub use turn::{CompactionConfig, TurnExecutor};
pub use user::{ApprovalRequest, UserExecutor, UserQuestion};
