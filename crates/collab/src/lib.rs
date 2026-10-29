//! WordCraft co-editing. Several people edit one document: each replica applies its own edits
//! at once and the relay's order for everyone's, re-running its unconfirmed edits on top
//! (`replica`). Positions travel as story offsets and move through each change (`mapping`).
//! This crate does no I/O and draws nothing: the host carries `proto` messages to the relay.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod hash;
pub mod mapping;
pub mod proto;
pub mod replica;
pub mod undo;
