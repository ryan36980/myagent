//! OpenClaw Rust Gateway — library crate.
//!
//! Re-exports all modules so they can be used by the binary crate
//! (`main.rs`) and by integration tests.

pub mod agent;
pub mod auth;
pub mod backup;
pub mod channel;
pub mod config;
pub mod error;
pub mod memory;
pub mod provider;
pub mod session;
pub mod tools;
