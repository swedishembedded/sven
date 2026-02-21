//! Neovim integration: grid data structures, RPC handler, grid renderer, and
//! the bridge that ties them all together around an embedded `nvim --embed`
//! process.

pub mod bridge;
pub mod grid;
pub mod handler;
pub mod render;

pub use bridge::NvimBridge;
