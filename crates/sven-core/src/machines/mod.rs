//! The two pluggable machines that drive the Sven runtime.
//!
//! Each machine implements [`sven_hsm::Machine`] and can be driven either
//! directly with [`sven_hsm::Hsm`] (pure, synchronous – perfect for tests) or
//! wrapped in a [`sven_hsm::Runtime`] for async production use.

pub mod clarification;
pub mod conversation;
pub mod loop_core;
pub mod reactive_agent;
pub mod sdlc;
pub mod software_development;
