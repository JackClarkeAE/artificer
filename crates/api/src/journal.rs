//! Journaling and command serialization for deterministic recording and replay.

use serde::{Deserialize, Serialize};

use crate::commands::ApiCommand;

pub const JOURNAL_SCHEMA_VERSION: u32 = 1;

/// A single recorded operation in the session journal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub label: String,
    pub command: ApiCommand,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_ms: Option<u64>,
}

impl JournalEntry {
    #[must_use]
    pub fn new(command: ApiCommand) -> Self {
        let label = command.label().to_owned();
        Self {
            label,
            command,
            timestamp_ms: None,
        }
    }
}

/// An ordered sequence of commands defining a model document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    pub schema_version: u32,
    pub entries: Vec<JournalEntry>,
}

impl Default for Journal {
    fn default() -> Self {
        Self::new()
    }
}

impl Journal {
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }

    pub fn push(&mut self, entry: JournalEntry) {
        self.entries.push(entry);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}
