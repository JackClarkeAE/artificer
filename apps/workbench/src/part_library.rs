//! Presentation-only foundation for the local Part Library.
//!
//! This module deliberately does not know about kernel snapshots, model
//! documents, catalog storage, or assembly placement. It validates the first
//! built-in parametric card and emits immutable insertion intents which the
//! workbench can pass through its universal confirmation gate. A later
//! catalog/model adapter can consume the same intents without moving parameter
//! validation into rendering code.

use egui::{Color32, FontId, RichText, Stroke};

const LIBRARY_PANEL: Color32 = crate::theme::PANEL;
const LIBRARY_CARD: Color32 = crate::theme::CARD;
const LIBRARY_BORDER: Color32 = crate::theme::BORDER;
const LIBRARY_TEXT: Color32 = crate::theme::TEXT;
const LIBRARY_MUTED: Color32 = crate::theme::MUTED;
const LIBRARY_ACCENT: Color32 = crate::theme::ACCENT;
const LIBRARY_GOOD: Color32 = crate::theme::GOOD;
const LIBRARY_BAD: Color32 = crate::theme::BAD;

/// Stable key for the first built-in parametric library definition.
pub const ALUMINIUM_EXTRUSION_20X20_KEY: &str = "builtin.aluminium-extrusion-20x20";
/// Human-readable name of the first built-in parametric library definition.
pub const ALUMINIUM_EXTRUSION_20X20_NAME: &str = "20 × 20 Aluminium Extrusion";
/// The current immutable revision of the built-in example definition.
pub const ALUMINIUM_EXTRUSION_20X20_REVISION: u32 = 1;
/// Stable key of the exposed extrusion-length parameter.
pub const LENGTH_PARAMETER_KEY: &str = "length";

const MIN_LENGTH_MM: f64 = 0.001;
const MAX_LENGTH_MM: f64 = 100_000.0;
const MAX_COMMITTED_INTENTS: usize = 128;

/// Whether a resolved parameter came from the definition or the user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParameterValueSource {
    Default,
    Entered,
}

/// One concrete, unit-normalized parameter assignment in an insertion intent.
#[derive(Clone, Debug, PartialEq)]
pub struct PartParameterAssignment {
    pub key: String,
    pub display_name: String,
    pub value_mm: f64,
    pub source: ParameterValueSource,
}

/// Immutable request emitted by the presentation shell after validation.
///
/// `staging_id` identifies one user insertion, not one geometry variant. Two
/// equal parameter sets therefore remain separate insertion intents while a
/// downstream catalog adapter may still share their evaluated geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct PartInsertionIntent {
    pub staging_id: u64,
    pub definition_key: String,
    pub definition_revision: u32,
    /// SHA-256 address of the exact immutable package selected in the library.
    pub definition_digest: String,
    pub display_name: String,
    pub parameters: Vec<PartParameterAssignment>,
}

/// Concrete dimensions exposed to the future part-evaluation adapter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedExtrusionDimensions {
    pub width_mm: f64,
    pub height_mm: f64,
    pub length_mm: f64,
}

impl PartInsertionIntent {
    /// Returns the resolved length carried by this built-in definition.
    #[must_use]
    pub fn length_mm(&self) -> Option<f64> {
        self.parameters
            .iter()
            .find(|parameter| parameter.key == LENGTH_PARAMETER_KEY)
            .map(|parameter| parameter.value_mm)
    }

    /// Returns the pure resolved 20 × 20 × Length data for this definition.
    ///
    /// This intentionally does not create a kernel command or publish a body;
    /// execution remains the responsibility of the model/catalog adapter.
    #[must_use]
    pub fn resolved_dimensions_mm(&self) -> Option<ResolvedExtrusionDimensions> {
        if self.definition_key != ALUMINIUM_EXTRUSION_20X20_KEY {
            return None;
        }
        Some(ResolvedExtrusionDimensions {
            width_mm: 20.0,
            height_mm: 20.0,
            length_mm: self.length_mm()?,
        })
    }
}

/// Preflight result used by both semantic tests and the egui control state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PartInsertionEligibility {
    Ready {
        length_mm: f64,
        source: ParameterValueSource,
    },
    AlreadyStaged,
    MissingLength,
    InvalidLength,
    NonFiniteLength,
    LengthTooSmall,
    LengthTooLarge,
}

impl PartInsertionEligibility {
    #[must_use]
    pub const fn can_stage(self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    #[must_use]
    pub const fn visible_reason(self) -> Option<&'static str> {
        match self {
            Self::Ready { .. } => None,
            Self::AlreadyStaged => {
                Some("Confirm or cancel the current staged insertion before adding another part.")
            }
            Self::MissingLength => {
                Some("Length is required. Enter a value in millimetres before adding this part.")
            }
            Self::InvalidLength => Some("Length must be a number in millimetres."),
            Self::NonFiniteLength => Some("Length must be a finite value in millimetres."),
            Self::LengthTooSmall => Some("Length must be at least 0.001 mm."),
            Self::LengthTooLarge => Some("Length must not exceed 100000 mm."),
        }
    }
}

/// Presentation state for the first independent Part Library window.
#[derive(Clone, Debug)]
pub struct PartLibraryState {
    open: bool,
    search: String,
    length_text: String,
    length_source: ParameterValueSource,
    length_default_mm: Option<f64>,
    definition_digest: String,
    next_staging_id: u64,
    staged: Option<PartInsertionIntent>,
    committed: Vec<PartInsertionIntent>,
    status: Option<String>,
}

impl Default for PartLibraryState {
    fn default() -> Self {
        Self::with_length_default(None)
    }
}

impl PartLibraryState {
    /// Creates the built-in card with an optional definition-owned default.
    ///
    /// The production example intentionally passes `None`, making Length a
    /// required input. The constructor keeps default behavior testable and
    /// adapter-ready for future published definitions.
    #[must_use]
    pub fn with_length_default(default_mm: Option<f64>) -> Self {
        let valid_default = default_mm
            .filter(|value| value.is_finite() && (MIN_LENGTH_MM..=MAX_LENGTH_MM).contains(value));
        Self {
            open: false,
            search: String::new(),
            length_text: valid_default.map(format_millimetres).unwrap_or_default(),
            length_source: if valid_default.is_some() {
                ParameterValueSource::Default
            } else {
                ParameterValueSource::Entered
            },
            length_default_mm: valid_default,
            definition_digest: String::new(),
            next_staging_id: 1,
            staged: None,
            committed: Vec::new(),
            status: None,
        }
    }

    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn open_mut(&mut self) -> &mut bool {
        &mut self.open
    }

    #[must_use]
    pub fn length_text(&self) -> &str {
        &self.length_text
    }

    pub fn set_length_text(&mut self, text: impl Into<String>) {
        self.length_text = text.into();
        self.length_source = ParameterValueSource::Entered;
        self.status = None;
    }

    /// Pins the visible card to one exact immutable catalog package.
    pub(crate) fn set_definition_digest(&mut self, digest: impl Into<String>) {
        self.definition_digest = digest.into();
    }

    #[must_use]
    pub fn definition_digest(&self) -> &str {
        &self.definition_digest
    }

    #[must_use]
    pub fn eligibility(&self) -> PartInsertionEligibility {
        let trimmed = self.length_text.trim();
        if trimmed.is_empty() {
            return PartInsertionEligibility::MissingLength;
        }
        let Ok(length_mm) = trimmed.parse::<f64>() else {
            return PartInsertionEligibility::InvalidLength;
        };
        if !length_mm.is_finite() {
            return PartInsertionEligibility::NonFiniteLength;
        }
        if length_mm < MIN_LENGTH_MM {
            return PartInsertionEligibility::LengthTooSmall;
        }
        if length_mm > MAX_LENGTH_MM {
            return PartInsertionEligibility::LengthTooLarge;
        }
        PartInsertionEligibility::Ready {
            length_mm,
            source: self.length_source,
        }
    }

    #[must_use]
    pub fn staged_intent(&self) -> Option<&PartInsertionIntent> {
        self.staged.as_ref()
    }

    #[must_use]
    pub fn committed_intents(&self) -> &[PartInsertionIntent] {
        &self.committed
    }

    /// Drains confirmed intents for a future catalog/model insertion adapter.
    pub fn drain_committed_intents(&mut self) -> Vec<PartInsertionIntent> {
        std::mem::take(&mut self.committed)
    }

    /// Stages the currently resolved card without committing workspace state.
    pub fn stage_selected(&mut self) -> Result<u64, PartInsertionEligibility> {
        if self.staged.is_some() {
            return Err(PartInsertionEligibility::AlreadyStaged);
        }
        let eligibility = self.eligibility();
        let PartInsertionEligibility::Ready { length_mm, source } = eligibility else {
            return Err(eligibility);
        };
        let staging_id = self.next_staging_id;
        self.next_staging_id = self.next_staging_id.saturating_add(1);
        self.staged = Some(PartInsertionIntent {
            staging_id,
            definition_key: ALUMINIUM_EXTRUSION_20X20_KEY.to_owned(),
            definition_revision: ALUMINIUM_EXTRUSION_20X20_REVISION,
            definition_digest: self.definition_digest.clone(),
            display_name: ALUMINIUM_EXTRUSION_20X20_NAME.to_owned(),
            parameters: vec![PartParameterAssignment {
                key: LENGTH_PARAMETER_KEY.to_owned(),
                display_name: "Length".to_owned(),
                value_mm: length_mm,
                source,
            }],
        });
        self.status = Some(
            "Placement staged. Use the green tick or Enter to commit; use the red X or Escape to cancel."
                .to_owned(),
        );
        Ok(staging_id)
    }

    /// Confirms only the matching staged insertion.
    pub fn commit_staged(&mut self, staging_id: u64) -> bool {
        let Some(staged) = self
            .staged
            .take_if(|intent| intent.staging_id == staging_id)
        else {
            return false;
        };
        let length = staged.length_mm().unwrap_or_default();
        let name = staged.display_name.clone();
        if self.committed.len() == MAX_COMMITTED_INTENTS {
            self.committed.remove(0);
        }
        self.committed.push(staged);
        self.status = Some(format!(
            "{name} · {} mm accepted for workspace insertion.",
            format_millimetres(length)
        ));
        true
    }

    /// Cancels only the matching staged insertion and keeps entered values.
    pub fn cancel_staged(&mut self, staging_id: u64) -> bool {
        if self
            .staged
            .as_ref()
            .is_none_or(|intent| intent.staging_id != staging_id)
        {
            return false;
        }
        self.staged = None;
        self.status = Some(
            "Insertion cancelled. Parameter values were retained for another placement.".to_owned(),
        );
        true
    }

    /// Draws the independent library window and returns a newly staged ID.
    pub(crate) fn show(
        &mut self,
        context: &egui::Context,
        another_operation_pending: bool,
    ) -> Option<u64> {
        if !self.open {
            return None;
        }

        let mut requested_stage = false;
        let mut open = self.open;
        egui::Window::new("Part Library")
            .id(egui::Id::new("part_library_window"))
            .open(&mut open)
            .default_pos(egui::pos2(82.0, 92.0))
            .default_size(egui::vec2(720.0, 470.0))
            .min_size(egui::vec2(590.0, 390.0))
            .resizable(true)
            .collapsible(true)
            .frame(
                egui::Frame::window(context.style_of(context.theme()).as_ref())
                    .fill(LIBRARY_PANEL)
                    .stroke(Stroke::new(1.0, LIBRARY_BORDER)),
            )
            .show(context, |ui| {
                requested_stage = self.contents(ui, another_operation_pending);
            });
        self.open = open;

        if requested_stage {
            self.stage_selected().ok()
        } else {
            None
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui, another_operation_pending: bool) -> bool {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("LOCAL PARTS")
                    .font(FontId::proportional(10.5))
                    .color(LIBRARY_ACCENT)
                    .strong(),
            );
            ui.separator();
            ui.label(
                RichText::new("Immutable definitions · exact revision insertion")
                    .small()
                    .color(LIBRARY_MUTED),
            );
        });
        ui.add_space(5.0);
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .id(egui::Id::new("part_library_search"))
                .hint_text("Search standard and parametric parts…")
                .desired_width(f32::INFINITY),
        );
        search.ctx.accesskit_node_builder(search.id, |node| {
            node.set_label("Search part library");
            node.set_description("Filter the available local standard and parametric parts.");
        });
        ui.add_space(7.0);

        let available = ui.available_rect_before_wrap();
        let list_width = (available.width() * 0.36).clamp(196.0, 260.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(list_width, available.height()),
                egui::Layout::top_down(egui::Align::Min),
                |ui| self.part_list(ui),
            );
            ui.separator();
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), available.height()),
                egui::Layout::top_down(egui::Align::Min),
                |ui| self.part_details(ui, another_operation_pending),
            )
            .inner
        })
        .inner
    }

    fn part_list(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("STANDARD COMPONENTS")
                .small()
                .color(LIBRARY_MUTED)
                .strong(),
        );
        ui.add_space(5.0);
        let query = self.search.trim().to_ascii_lowercase();
        let searchable =
            format!("{ALUMINIUM_EXTRUSION_20X20_NAME} aluminium profile extrusion parametric")
                .to_ascii_lowercase();
        if !query.is_empty() && !searchable.contains(&query) {
            ui.label(
                RichText::new("No local parts match this search.")
                    .color(LIBRARY_MUTED)
                    .italics(),
            );
            return;
        }

        egui::Frame::new()
            .fill(LIBRARY_CARD)
            .stroke(Stroke::new(1.0, LIBRARY_ACCENT.gamma_multiply(0.65)))
            .corner_radius(4)
            .inner_margin(egui::Margin::same(7))
            .show(ui, |ui| {
                let response = ui.add_sized(
                    [ui.available_width(), 66.0],
                    egui::Button::new(
                        RichText::new(ALUMINIUM_EXTRUSION_20X20_NAME)
                            .color(LIBRARY_TEXT)
                            .strong(),
                    )
                    .frame(false)
                    .selected(true),
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        ALUMINIUM_EXTRUSION_20X20_NAME,
                    )
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("PARAMETRIC").small().color(LIBRARY_ACCENT));
                    ui.label(
                        RichText::new("Aluminium profiles")
                            .small()
                            .color(LIBRARY_MUTED),
                    );
                });
            });
    }

    fn part_details(&mut self, ui: &mut egui::Ui, another_operation_pending: bool) -> bool {
        ui.label(
            RichText::new(ALUMINIUM_EXTRUSION_20X20_NAME)
                .font(FontId::proportional(17.0))
                .color(LIBRARY_TEXT)
                .strong(),
        );
        let package_identity = if self.definition_digest.len() == 64 {
            format!(
                "Parametric part · revision {ALUMINIUM_EXTRUSION_20X20_REVISION}.0.0 · verified {}…",
                &self.definition_digest[..12]
            )
        } else {
            format!(
                "Parametric part · revision {ALUMINIUM_EXTRUSION_20X20_REVISION}.0.0 · package unavailable"
            )
        };
        ui.label(
            RichText::new(package_identity)
                .small()
                .color(LIBRARY_ACCENT),
        );
        ui.add_space(5.0);
        ui.label(
            RichText::new(
                "Exact 20 mm × 20 mm profile with a user-resolved extrusion length. Equal variants may share evaluated geometry while every insertion remains independent.",
            )
            .color(LIBRARY_MUTED),
        );
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(
            RichText::new("PARAMETERS")
                .small()
                .color(LIBRARY_MUTED)
                .strong(),
        );
        ui.add_space(5.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Length").color(LIBRARY_TEXT).strong());
            if self.length_default_mm.is_some()
                && self.length_source == ParameterValueSource::Default
            {
                ui.label(RichText::new("DEFAULT").small().color(LIBRARY_GOOD));
            } else {
                ui.label(RichText::new("REQUIRED").small().color(LIBRARY_ACCENT));
            }
        });
        let editor = ui.add(
            egui::TextEdit::singleline(&mut self.length_text)
                .id(egui::Id::new("part_library_length_mm"))
                .desired_width(190.0),
        );
        editor.ctx.accesskit_node_builder(editor.id, |node| {
            node.set_label("Length (mm)");
            node.set_description(
                "Required aluminium extrusion length in millimetres. A valid value enables Add to current workspace.",
            );
        });
        ui.label(RichText::new("millimetres").small().color(LIBRARY_MUTED));
        if editor.changed() {
            self.length_source = ParameterValueSource::Entered;
            self.status = None;
        }

        let eligibility = self.eligibility();
        if let Some(reason) = eligibility.visible_reason() {
            ui.label(RichText::new(reason).small().color(LIBRARY_BAD));
        } else if let PartInsertionEligibility::Ready { length_mm, source } = eligibility {
            let source_label = match source {
                ParameterValueSource::Default => "definition default",
                ParameterValueSource::Entered => "entered value",
            };
            ui.label(
                RichText::new(format!(
                    "Resolved · {} mm · {source_label}",
                    format_millimetres(length_mm)
                ))
                .small()
                .color(LIBRARY_GOOD),
            );
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            let blocked_reason = if another_operation_pending || self.staged.is_some() {
                Some("Confirm or cancel the current staged operation before adding another part.")
            } else {
                eligibility.visible_reason()
            };
            let add = ui.add_enabled(
                blocked_reason.is_none(),
                egui::Button::new(
                    RichText::new("Add to current workspace")
                        .color(LIBRARY_TEXT)
                        .strong(),
                )
                .fill(LIBRARY_ACCENT.gamma_multiply(0.28))
                .stroke(Stroke::new(1.0, LIBRARY_ACCENT))
                .corner_radius(3)
                .min_size(egui::vec2(ui.available_width(), 34.0)),
            );
            let add = if let Some(reason) = blocked_reason {
                add.on_disabled_hover_text(reason)
            } else {
                add.on_hover_text(
                    "Stage a separate component insertion; the green tick or Enter commits it.",
                )
            };
            if let Some(status) = &self.status {
                ui.label(RichText::new(status).small().color(LIBRARY_MUTED));
            }
            add.clicked()
        })
        .inner
    }
}

fn format_millimetres(value: f64) -> String {
    let formatted = format!("{value:.6}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_length_blocks_staging_with_precise_diagnostics() {
        let mut library = PartLibraryState::default();
        assert_eq!(
            library.eligibility(),
            PartInsertionEligibility::MissingLength
        );
        assert_eq!(
            library.stage_selected(),
            Err(PartInsertionEligibility::MissingLength)
        );

        library.set_length_text("not-a-number");
        assert_eq!(
            library.eligibility(),
            PartInsertionEligibility::InvalidLength
        );
        library.set_length_text("NaN");
        assert_eq!(
            library.eligibility(),
            PartInsertionEligibility::NonFiniteLength
        );
        library.set_length_text("0");
        assert_eq!(
            library.eligibility(),
            PartInsertionEligibility::LengthTooSmall
        );
        library.set_length_text("100001");
        assert_eq!(
            library.eligibility(),
            PartInsertionEligibility::LengthTooLarge
        );
        assert!(library.staged_intent().is_none());
    }

    #[test]
    fn optional_default_is_explicitly_preserved_in_the_intent() {
        let mut library = PartLibraryState::with_length_default(Some(500.0));
        assert_eq!(library.length_text(), "500");
        let staging_id = library
            .stage_selected()
            .expect("valid default should stage");
        let intent = library.staged_intent().expect("staged intent");
        assert_eq!(intent.staging_id, staging_id);
        assert_eq!(intent.length_mm(), Some(500.0));
        assert_eq!(
            intent.resolved_dimensions_mm(),
            Some(ResolvedExtrusionDimensions {
                width_mm: 20.0,
                height_mm: 20.0,
                length_mm: 500.0,
            })
        );
        assert_eq!(intent.parameters[0].source, ParameterValueSource::Default);
    }

    #[test]
    fn repeated_equal_additions_remain_separate_and_retain_the_field_value() {
        let mut library = PartLibraryState::default();
        library.set_length_text("455");
        let first = library
            .stage_selected()
            .expect("first insertion should stage");
        assert!(library.commit_staged(first));
        assert_eq!(library.length_text(), "455");

        let second = library
            .stage_selected()
            .expect("second insertion should stage independently");
        assert_ne!(first, second);
        assert!(library.commit_staged(second));
        assert_eq!(library.committed_intents().len(), 2);
        assert_eq!(library.committed_intents()[0].length_mm(), Some(455.0));
        assert_eq!(library.committed_intents()[1].length_mm(), Some(455.0));
        assert_ne!(
            library.committed_intents()[0].staging_id,
            library.committed_intents()[1].staging_id
        );
    }

    #[test]
    fn cancel_keeps_values_but_does_not_commit_an_intent() {
        let mut library = PartLibraryState::default();
        library.set_length_text("310");
        let staged = library
            .stage_selected()
            .expect("valid insertion should stage");
        assert!(library.cancel_staged(staged));
        assert_eq!(library.length_text(), "310");
        assert!(library.staged_intent().is_none());
        assert!(library.committed_intents().is_empty());
    }
}
