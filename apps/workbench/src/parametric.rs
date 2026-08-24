//! The Parametric Design tab's variables panel.
//!
//! Variables are the document's typed parameter table with a face: named
//! values — lengths, angles, dimensionless factors — created here, renamed
//! here, given literal values or expressions over one another, and consumed
//! anywhere a dimension or feature value accepts an entry. Every change that
//! can move geometry stages through the universal confirmation gate exactly
//! as a modeling operation does; renames and metadata edits, which move
//! nothing, apply directly.

use artificer_model::{
    ParameterBinding, ParameterExposure, ParameterId, ParameterOverrides, ParameterType,
    ParameterUnit, ParameterValue, ParsedParameterEntry, QuantityKind, QuantityValue,
    format_parameter_binding, parameter_unit_suffix, parse_parameter_entry,
};
use eframe::egui;
use egui::{Frame, Margin, RichText, Stroke};

use crate::{KernelLabApp, PendingOperation, theme};

impl KernelLabApp {
    /// Stages the creation of one new named variable of `kind`. The ordinal
    /// keeps generated keys unique without ever reusing one.
    pub(crate) fn stage_new_variable(&mut self, kind: QuantityKind) {
        if self.pending_operation.is_some() {
            return;
        }
        let prefix = match kind {
            QuantityKind::Length => "Length",
            QuantityKind::Angle => "Angle",
            QuantityKind::Scalar => "Factor",
        };
        let mut ordinal = self.document.parameters().len() as u32 + 1;
        while self
            .document
            .parameters()
            .get_by_key(&format!("{prefix}{ordinal}"))
            .is_some()
        {
            ordinal = ordinal.saturating_add(1);
        }
        self.variables_window_open = true;
        self.pending_operation = Some(PendingOperation::AddUserParameter { ordinal, kind });
    }

    /// Resolves a variable name to its id for the expression parser,
    /// excluding `except` so a variable cannot silently reference itself.
    fn variable_resolver(
        &self,
        except: Option<ParameterId>,
    ) -> impl Fn(&str) -> Option<ParameterId> + 'static {
        let names = self
            .document
            .parameters()
            .records()
            .iter()
            .filter(|record| Some(record.id) != except)
            .map(|record| (record.spec.key.clone(), record.id))
            .collect::<std::collections::BTreeMap<_, _>>();
        move |name: &str| names.get(name).copied()
    }

    /// Parses one typed entry against the current table and stages it for
    /// confirmation. Returns the error text to show beside the field.
    fn stage_variable_entry(&mut self, parameter: ParameterId, text: &str) -> Option<String> {
        let record = self.document.parameter(parameter)?;
        let default_unit = record
            .spec
            .display_unit
            .unwrap_or(ParameterUnit::Millimeter);
        let resolve = self.variable_resolver(Some(parameter));
        match parse_parameter_entry(text, default_unit, &resolve) {
            Ok(ParsedParameterEntry::Literal(value)) => {
                self.staged_parameter_binding = Some((parameter, ParameterBinding::literal(value)));
                self.pending_operation =
                    Some(PendingOperation::SetParameterBindingEntry { parameter });
                None
            }
            Ok(ParsedParameterEntry::Expression(expression)) => {
                self.staged_parameter_binding =
                    Some((parameter, ParameterBinding::expression(expression)));
                self.pending_operation =
                    Some(PendingOperation::SetParameterBindingEntry { parameter });
                None
            }
            Err(error) => Some(error.to_string()),
        }
    }

    /// A name to show and edit for one variable: the key is the identity
    /// expressions use, so the panel edits the key and mirrors it into the
    /// label.
    fn commit_variable_rename(&mut self, parameter: ParameterId, name: &str) {
        let Some(record) = self.document.parameter(parameter).cloned() else {
            return;
        };
        let trimmed = name.trim();
        if trimmed == record.spec.key {
            return;
        }
        let mut spec = record.spec.clone();
        spec.key = trimmed.to_owned();
        spec.label = trimmed.to_owned();
        match self.document.replace_parameter_spec(parameter, spec) {
            Ok(_) => {
                self.document_status = Some(format!("Variable renamed to {trimmed}"));
            }
            Err(error) => {
                self.document_status = Some(format!("Variable rename rejected: {error}"));
            }
        }
    }

    /// Every variable name with its evaluated canonical value in millimetres,
    /// radians, or a bare scalar — the lookup sketch dimension fields consume.
    #[must_use]
    pub fn evaluated_variable_values(&self) -> std::collections::BTreeMap<String, f64> {
        let Ok(evaluated) = self
            .document
            .evaluate_parameters(&ParameterOverrides::default())
        else {
            return std::collections::BTreeMap::new();
        };
        self.document
            .parameters()
            .records()
            .iter()
            .filter_map(|record| {
                let value = evaluated.get(record.id)?;
                let ParameterValue::Quantity { value } = value else {
                    return None;
                };
                Some((record.spec.key.clone(), value.magnitude))
            })
            .collect()
    }

    /// The variables panel: one row per parameter with its name, its value or
    /// expression, its evaluated result, and a delete control; the add
    /// commands live in the ribbon's Parametric tab beside the toggle.
    pub(crate) fn variables_window(&mut self, context: &egui::Context) {
        if !self.variables_window_open {
            return;
        }
        let mut open = self.variables_window_open;
        let evaluated = self
            .document
            .evaluate_parameters(&ParameterOverrides::default())
            .ok();
        let records = self
            .document
            .parameters()
            .records()
            .iter()
            .map(|record| {
                (
                    record.id,
                    record.spec.key.clone(),
                    record.spec.display_unit,
                    record.binding.clone(),
                    record.spec.metadata.exposure,
                    matches!(record.spec.value_type, ParameterType::Quantity(_)),
                )
            })
            .collect::<Vec<_>>();
        let names = self
            .document
            .parameters()
            .records()
            .iter()
            .map(|record| (record.id, record.spec.key.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let busy = self.pending_operation.is_some();
        let mut rename: Option<(ParameterId, String)> = None;
        let mut stage: Option<(ParameterId, String)> = None;
        let mut remove: Option<ParameterId> = None;
        egui::Window::new("VARIABLES")
            .id(egui::Id::new("variables_window"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-18.0, 150.0))
            .default_width(430.0)
            .max_height(560.0)
            .resizable(true)
            .open(&mut open)
            .frame(
                Frame::new()
                    .fill(theme::panel().gamma_multiply(0.98))
                    .stroke(Stroke::new(1.0, theme::border()))
                    .corner_radius(6)
                    .inner_margin(Margin::same(10)),
            )
            .show(context, |ui| {
                ui.label(
                    RichText::new(
                        "Name a value once, then type its name in any dimension or value \
                         field. Values accept expressions over other variables: width * 2 + 5mm.",
                    )
                    .small()
                    .color(theme::muted()),
                );
                ui.add_space(6.0);
                if records.is_empty() {
                    ui.label(
                        RichText::new(
                            "No variables yet · create one with the Length, Angle, or Factor \
                             commands in the Parametric tab",
                        )
                        .color(theme::muted()),
                    );
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("variables_grid")
                        .num_columns(4)
                        .spacing(egui::vec2(8.0, 6.0))
                        .striped(true)
                        .show(ui, |ui| {
                            ui.label(RichText::new("Name").small().color(theme::muted()));
                            ui.label(
                                RichText::new("Value or expression")
                                    .small()
                                    .color(theme::muted()),
                            );
                            ui.label(RichText::new("Evaluates to").small().color(theme::muted()));
                            ui.label("");
                            ui.end_row();
                            for (id, key, display_unit, binding, exposure, is_quantity) in &records
                            {
                                let serial = id.get();
                                // Name field: edits commit when focus leaves.
                                let mut name_draft = self
                                    .variable_name_drafts
                                    .get(&serial)
                                    .cloned()
                                    .unwrap_or_else(|| key.clone());
                                let name_response = ui.add_enabled(
                                    !busy,
                                    egui::TextEdit::singleline(&mut name_draft)
                                        .desired_width(96.0)
                                        .hint_text("name"),
                                );
                                name_response.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::TextEdit,
                                        true,
                                        format!("Variable name {key}"),
                                    )
                                });
                                if name_response.changed() {
                                    self.variable_name_drafts.insert(serial, name_draft.clone());
                                }
                                if name_response.lost_focus()
                                    && let Some(draft) = self.variable_name_drafts.remove(&serial)
                                {
                                    rename = Some((*id, draft));
                                }

                                // Value field: Enter (or focus loss with a
                                // change) parses and stages.
                                let stored = format_parameter_binding(binding, &|reference| {
                                    names.get(&reference).cloned()
                                });
                                let mut value_draft = self
                                    .variable_value_drafts
                                    .get(&serial)
                                    .cloned()
                                    .unwrap_or_else(|| stored.clone());
                                let hint = if *exposure == ParameterExposure::UserInput
                                    && stored.is_empty()
                                {
                                    "required input"
                                } else {
                                    "value or expression"
                                };
                                let value_response = ui.add_enabled(
                                    !busy && *is_quantity,
                                    egui::TextEdit::singleline(&mut value_draft)
                                        .desired_width(150.0)
                                        .hint_text(hint),
                                );
                                value_response.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::TextEdit,
                                        true,
                                        format!("Variable value {key}"),
                                    )
                                });
                                if value_response.changed() {
                                    self.variable_value_drafts
                                        .insert(serial, value_draft.clone());
                                }
                                let commit = value_response.lost_focus()
                                    && self
                                        .variable_value_drafts
                                        .get(&serial)
                                        .is_some_and(|draft| draft.trim() != stored.trim());
                                if commit
                                    && let Some(draft) =
                                        self.variable_value_drafts.get(&serial).cloned()
                                {
                                    stage = Some((*id, draft));
                                }

                                // Evaluated result, in the display unit.
                                let display = evaluated
                                    .as_ref()
                                    .and_then(|values| values.get(*id))
                                    .and_then(|value| match value {
                                        ParameterValue::Quantity { value } => Some(*value),
                                        _ => None,
                                    })
                                    .map_or_else(
                                        || "—".to_owned(),
                                        |value| format_evaluated_quantity(value, *display_unit),
                                    );
                                ui.label(RichText::new(display).monospace());

                                let delete = ui.add_enabled(!busy, egui::Button::new("✕").small());
                                delete.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::Button,
                                        true,
                                        format!("Delete variable {key}"),
                                    )
                                });
                                if delete
                                    .on_hover_text(
                                        "Delete this variable. A variable still driving a \
                                         feature or expression is refused.",
                                    )
                                    .clicked()
                                {
                                    remove = Some(*id);
                                }
                                ui.end_row();
                            }
                        });
                });
                if busy {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Staged · confirm with the green tick or Enter")
                            .small()
                            .color(theme::good()),
                    );
                }
            });
        if let Some((parameter, name)) = rename {
            self.commit_variable_rename(parameter, &name);
        }
        if let Some((parameter, text)) = stage
            && let Some(error) = self.stage_variable_entry(parameter, &text)
        {
            self.document_status = Some(format!("Variable entry rejected: {error}"));
        }
        if let Some(parameter) = remove {
            self.pending_operation = Some(PendingOperation::RemoveParameter { parameter });
        }
        self.variables_window_open = open;
    }
}

/// Formats an evaluated canonical quantity in the spec's display unit.
fn format_evaluated_quantity(value: QuantityValue, display_unit: Option<ParameterUnit>) -> String {
    let unit = display_unit.unwrap_or(value.unit);
    let converted = convert_canonical(value, unit);
    let suffix = parameter_unit_suffix(unit);
    if suffix.is_empty() {
        format!("{converted:.4}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    } else {
        format!(
            "{} {suffix}",
            format!("{converted:.4}")
                .trim_end_matches('0')
                .trim_end_matches('.')
        )
    }
}

/// Converts a canonical magnitude (mm, rad, or scalar) into `unit`.
fn convert_canonical(value: QuantityValue, unit: ParameterUnit) -> f64 {
    let scale = match unit {
        ParameterUnit::Micrometer => 0.001,
        ParameterUnit::Millimeter | ParameterUnit::Radian | ParameterUnit::Scalar => 1.0,
        ParameterUnit::Centimeter => 10.0,
        ParameterUnit::Meter => 1_000.0,
        ParameterUnit::Inch => 25.4,
        ParameterUnit::Foot => 304.8,
        ParameterUnit::Degree => std::f64::consts::PI / 180.0,
    };
    value.magnitude / scale
}
