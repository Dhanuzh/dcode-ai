// `#[async_trait]` puts `#[must_use]` on futures that are already must-use;
// clippy 1.99+ reports that as `double_must_use`.
#![allow(clippy::double_must_use)]
pub mod agent;
pub mod agent_defs;
pub mod approval;
pub mod code_intel;
pub mod code_map;
pub mod command_safety;
pub mod cost;
pub mod harness;
pub mod hooks;
pub mod mcp_oauth;
pub mod plugin_installer;
pub mod provider;
pub mod skill_installer;
pub mod skills;
pub mod tools;
pub mod undo;
