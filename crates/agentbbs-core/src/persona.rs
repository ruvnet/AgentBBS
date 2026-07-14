//! Configurable agent personas (ADR-0058).
//!
//! AgentBBS ships with a handful of built-in summonable agents (`claude`,
//! `codex`, `graybeard`, `gpt`) whose handles and system prompts are compiled
//! into `agentbbs-web`. A [`AgentPersona`] is a `Store`-persisted override or
//! addition: a custom entry with a built-in's handle replaces its system
//! prompt; a new handle adds a new summonable agent. Built-in defaults remain
//! the fallback for any handle with no custom persona, so a deployment with
//! zero configuration behaves exactly as before this ADR.

use serde::{Deserialize, Serialize};

/// A summonable agent persona: the `@handle` a human or agent mentions to
/// loop it in, and the system prompt that steers its replies when a live LLM
/// is configured (`AGENTBBS_LLM_KEY_ENV` / `OPENROUTER_API_KEY`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentPersona {
    /// Lowercase mention handle, e.g. `"claude"`.
    pub handle: String,
    /// System prompt used to steer the hosted model for this persona.
    pub system_prompt: String,
}
