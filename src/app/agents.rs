//! Agent definitions — personas that shape how the LLM responds.
//!
//! Each agent carries a name, description, and persona string that gets
//! injected into the system prompt.  Users can create custom agents on the
//! fly with `/agent create <name> <description>` and switch between them
//! with `/agent use <name>`.

use serde::{Deserialize, Serialize};

/// An agent persona that shapes how the LLM responds.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Agent {
    /// Short identifier (e.g. "memini", "coder").
    pub name: String,
    /// One-line description shown in `/agent` list.
    pub description: String,
    /// The personality/instructions injected into the system prompt.
    pub persona: String,
}

impl Default for Agent {
    fn default() -> Self {
        Self {
            name: "memini".to_string(),
            description: "Your personal CLI assistant with long-term memory".to_string(),
            persona: "You are Memini, a concise CLI assistant with long-term memory. \
                      You remember past conversations and use context to give personalized, \
                      helpful answers. Be concise but thorough."
                .to_string(),
        }
    }
}
