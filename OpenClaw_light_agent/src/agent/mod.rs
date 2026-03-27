//! Agent core — the stable heart of the gateway.
//!
//! This module is completely channel-agnostic. It processes messages
//! through the ReAct loop using trait objects for all external interactions.

pub mod context;
pub mod react_loop;
