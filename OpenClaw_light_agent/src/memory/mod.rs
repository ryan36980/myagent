//! Long-term memory persistence layer.
//!
//! Shared MEMORY.md (cross-chat) + per-chat MEMORY.md (curated) + daily YYYY-MM-DD.md logs (append-only).

pub mod store;

pub use store::MemoryStore;
