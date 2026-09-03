//! A semantic diff between two scripts: what changed in the commands they
//! compile to, their parameters and their names, rather than in their
//! text.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::api::commands::ApiCommand;
use crate::api::scripting::ScriptProgram;
use crate::api::selectors::EntitySelector;

/// One difference between two scripts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum DiffEntry {
    ParameterAdded {
        name: String,
        value: f64,
    },
    ParameterRemoved {
        name: String,
        value: f64,
    },
    ParameterChanged {
        name: String,
        old: f64,
        new: f64,
    },
    /// A step the second script has and the first does not.
    StepAdded {
        label: String,
        command: String,
        /// Its position in the second script.
        index: usize,
    },
    StepRemoved {
        label: String,
        command: String,
        index: usize,
    },
    /// The same step in both, at a different position relative to the
    /// steps both share.
    StepMoved {
        label: String,
        from: usize,
        to: usize,
    },
    /// The same label with different arguments, or a different command.
    StepChanged {
        label: String,
        command: String,
        fields: Vec<FieldChange>,
    },
    /// A face or edge name the second script has and the first does not,
    /// for a selector the first did not name.
    NameAdded {
        name: String,
    },
    NameRemoved {
        name: String,
    },
    /// The same selector under a different name.
    NameRenamed {
        old: String,
        new: String,
    },
    /// The same name pointing at a different selector.
    SelectorChanged {
        name: String,
        old: serde_json::Value,
        new: serde_json::Value,
    },
}

/// One argument of a step that differs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldChange {
    pub field: String,
    pub old: serde_json::Value,
    pub new: serde_json::Value,
}

/// Every difference between two compiled scripts, in the order a reader
/// wants them: parameters, steps, names.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScriptDiff {
    pub entries: Vec<DiffEntry>,
}

impl ScriptDiff {
    /// The differences from `old` to `new`.
    #[must_use]
    pub fn between(old: &ScriptProgram, new: &ScriptProgram) -> Self {
        let mut entries = Vec::new();
        parameters(&old.parameters, &new.parameters, &mut entries);
        steps(&old.commands, &new.commands, &mut entries);
        names(&old.names, &new.names, &mut entries);
        Self { entries }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// One line per entry, for a console.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.entries.iter().map(DiffEntry::describe).collect()
    }
}

impl DiffEntry {
    /// The entry in words.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::ParameterAdded { name, value } => format!("param {name} added = {value}"),
            Self::ParameterRemoved { name, value } => format!("param {name} removed (was {value})"),
            Self::ParameterChanged { name, old, new } => {
                format!("param {name}: {old} -> {new}")
            }
            Self::StepAdded {
                label,
                command,
                index,
            } => format!("step \"{label}\" ({command}) added at {index}"),
            Self::StepRemoved {
                label,
                command,
                index,
            } => format!("step \"{label}\" ({command}) removed from {index}"),
            Self::StepMoved { label, from, to } => {
                format!("step \"{label}\" moved from {from} to {to}")
            }
            Self::StepChanged {
                label,
                command,
                fields,
            } => format!(
                "step \"{label}\" ({command}) changed: {}",
                fields
                    .iter()
                    .map(|change| format!("{} {} -> {}", change.field, change.old, change.new))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::NameAdded { name } => format!("name {name} added"),
            Self::NameRemoved { name } => format!("name {name} removed"),
            Self::NameRenamed { old, new } => format!("name {old} renamed to {new}"),
            Self::SelectorChanged { name, .. } => format!("name {name} now selects differently"),
        }
    }
}

fn parameters(
    old: &BTreeMap<String, f64>,
    new: &BTreeMap<String, f64>,
    entries: &mut Vec<DiffEntry>,
) {
    for (name, value) in old {
        match new.get(name) {
            None => entries.push(DiffEntry::ParameterRemoved {
                name: name.clone(),
                value: *value,
            }),
            Some(changed) if changed != value => entries.push(DiffEntry::ParameterChanged {
                name: name.clone(),
                old: *value,
                new: *changed,
            }),
            Some(_) => {}
        }
    }
    for (name, value) in new {
        if !old.contains_key(name) {
            entries.push(DiffEntry::ParameterAdded {
                name: name.clone(),
                value: *value,
            });
        }
    }
}

fn steps(old: &[ApiCommand], new: &[ApiCommand], entries: &mut Vec<DiffEntry>) {
    let old_index: BTreeMap<&str, usize> = old
        .iter()
        .enumerate()
        .map(|(index, command)| (command.label(), index))
        .collect();
    let new_index: BTreeMap<&str, usize> = new
        .iter()
        .enumerate()
        .map(|(index, command)| (command.label(), index))
        .collect();

    for (index, command) in old.iter().enumerate() {
        if !new_index.contains_key(command.label()) {
            entries.push(DiffEntry::StepRemoved {
                label: command.label().to_owned(),
                command: command.kind().to_owned(),
                index,
            });
        }
    }
    for (index, command) in new.iter().enumerate() {
        if !old_index.contains_key(command.label()) {
            entries.push(DiffEntry::StepAdded {
                label: command.label().to_owned(),
                command: command.kind().to_owned(),
                index,
            });
        }
    }

    // Order among the shared steps: a step whose rank among shared steps
    // differs has moved.
    let shared_old: Vec<&str> = old
        .iter()
        .map(ApiCommand::label)
        .filter(|label| new_index.contains_key(label))
        .collect();
    let shared_new: Vec<&str> = new
        .iter()
        .map(ApiCommand::label)
        .filter(|label| old_index.contains_key(label))
        .collect();
    let mut reported = std::collections::BTreeSet::new();
    for (rank, label) in shared_old.iter().enumerate() {
        let new_rank = shared_new
            .iter()
            .position(|other| other == label)
            .unwrap_or(rank);
        if new_rank != rank && reported.insert(*label) {
            entries.push(DiffEntry::StepMoved {
                label: (*label).to_owned(),
                from: old_index[label],
                to: new_index[label],
            });
        }
    }

    for command in old {
        let Some(&index) = new_index.get(command.label()) else {
            continue;
        };
        let changed = &new[index];
        if changed == command {
            continue;
        }
        let fields = field_changes(command, changed);
        entries.push(DiffEntry::StepChanged {
            label: command.label().to_owned(),
            command: changed.kind().to_owned(),
            fields,
        });
    }
}

/// The arguments that differ between two commands, by field name; a
/// command of a different kind reports the kind itself.
fn field_changes(old: &ApiCommand, new: &ApiCommand) -> Vec<FieldChange> {
    let old_value = serde_json::to_value(old).unwrap_or_default();
    let new_value = serde_json::to_value(new).unwrap_or_default();
    let (Some(old_object), Some(new_object)) = (old_value.as_object(), new_value.as_object())
    else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    let mut keys: Vec<&String> = old_object.keys().chain(new_object.keys()).collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        let before = old_object
            .get(key)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let after = new_object
            .get(key)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if before != after {
            fields.push(FieldChange {
                field: key.clone(),
                old: before,
                new: after,
            });
        }
    }
    fields
}

fn names(
    old: &[(String, EntitySelector)],
    new: &[(String, EntitySelector)],
    entries: &mut Vec<DiffEntry>,
) {
    let old_by_name: BTreeMap<&str, &EntitySelector> = old
        .iter()
        .map(|(name, selector)| (name.as_str(), selector))
        .collect();
    let new_by_name: BTreeMap<&str, &EntitySelector> = new
        .iter()
        .map(|(name, selector)| (name.as_str(), selector))
        .collect();
    let mut renamed_from = std::collections::BTreeSet::new();
    let mut renamed_to = std::collections::BTreeSet::new();

    // A name that vanished while its selector reappeared under a new name
    // is a rename, not a removal and an addition.
    for (name, selector) in old {
        if new_by_name.contains_key(name.as_str()) {
            continue;
        }
        if let Some((new_name, _)) = new.iter().find(|(other, candidate)| {
            !old_by_name.contains_key(other.as_str()) && candidate == selector
        }) {
            renamed_from.insert(name.as_str());
            renamed_to.insert(new_name.as_str());
            entries.push(DiffEntry::NameRenamed {
                old: name.clone(),
                new: new_name.clone(),
            });
        }
    }
    for (name, _) in old {
        if !new_by_name.contains_key(name.as_str()) && !renamed_from.contains(name.as_str()) {
            entries.push(DiffEntry::NameRemoved { name: name.clone() });
        }
    }
    for (name, selector) in new {
        match old_by_name.get(name.as_str()) {
            None => {
                if !renamed_to.contains(name.as_str()) {
                    entries.push(DiffEntry::NameAdded { name: name.clone() });
                }
            }
            Some(before) if *before != selector => entries.push(DiffEntry::SelectorChanged {
                name: name.clone(),
                old: serde_json::to_value(before).unwrap_or_default(),
                new: serde_json::to_value(selector).unwrap_or_default(),
            }),
            Some(_) => {}
        }
    }
}
