//! The pluggable machines that drive the Sven runtime.
//!
//! Each machine implements [`sven_hsm::Machine`] and can be driven either
//! directly with [`sven_hsm::Hsm`] (pure, synchronous — perfect for tests) or
//! wrapped in a `sven_kernel::Runtime` for async production use.

pub mod loop_core;
pub mod reactive_agent;
pub mod sdlc;
pub mod ui_test;
pub mod verified_task;
