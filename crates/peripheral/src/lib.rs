//! Library interface for reloopy-peripheral.
//!
//! Exposes core modules so external crates (e.g. integration tests) can reuse
//! [`Agent`], the [`LlmClient`] trait, and [`ScriptedLlmClient`] without
//! depending on the binary entry-point.

pub mod agent;
pub mod deepseek;
pub mod ipc_client;
pub mod memory;
pub mod migration;
pub mod scripted_llm;
pub mod source;
pub mod tools;
