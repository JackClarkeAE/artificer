//! Presentation for the local Part Library.
//!
//! This module deliberately does not know about kernel snapshots, model
//! documents, catalog storage, or assembly placement. The workbench hands it
//! the parts the library holds — the built-in extrusion and every part a
//! person has saved — as plain descriptions with their parameters, and their
//! saved pictures as plain data. It validates the values typed for the
//! selected part and emits immutable insertion intents, which the workbench
//! passes through its universal confirmation gate. Every insertion is its
//! own intent with its own values, so one part can be placed as many times
//! as wanted, each at different values.

use std::collections::BTreeMap;

use artificer_catalog::{PartPreview, PartPreviewFacts};
use artificer_sketch::expression::{FieldUnit, evaluate_entry};
use egui::{FontId, RichText, Stroke};

use crate::part_preview::decode_png;
use crate::units::LengthUnit;

// Aliases rather than a second palette: the library styles itself from the
// active theme like every other panel, and these names only exist so the
// module reads in its own vocabulary.
use crate::theme::{
    accent as library_accent, bad as library_bad, border as library_border, card as library_card,
    good as library_good, muted as library_muted, panel as library_panel, text as library_text,
};

/// Stable key for the first built-in parametric library definition.
pub const ALUMINIUM_EXTRUSION_20X20_KEY: &str = "builtin.aluminium-extrusion-20x20";
/// Human-readable name of the first built-in parametric library definition.
pub const ALUMINIUM_EXTRUSION_20X20_NAME: &str = "20 × 20 Aluminium Extrusion";
/// The authored (major) revision of the built-in example definition. The
/// package's full revision adds a minor that follows the native document
/// schema it embeds, because the embedded document — and so the package's
/// content address — changes with the schema even when the part does not
/// (see `library_catalog::builtin_part_revision`).
pub const ALUMINIUM_EXTRUSION_20X20_REVISION: u32 = 1;
/// Stable key of the exposed extrusion-length parameter.
pub const LENGTH_PARAMETER_KEY: &str = "length";

const MIN_LENGTH_MM: f64 = 0.001;
const MAX_LENGTH_MM: f64 = 100_000.0;
const MAX_COMMITTED_INTENTS: usize = 128;
/// The side of a part's picture in the list, in points.
const THUMBNAIL_SIDE: f32 = 52.0;

/// Whether a resolved parameter came from the definition or the user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParameterValueSource {
    Default,
    Entered,
}

/// What a parameter measures, which decides how its field reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParameterQuantity {
    /// Read in the document's length unit, or the unit typed; millimetres.
    Length,
    /// Read in degrees, or the unit typed; radians.
    Angle,
    /// A plain number.
    Number,
}

/// One parameter a library part takes. Values are canonical: millimetres,
/// radians, or the number itself.
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryParameter {
    pub key: String,
    pub label: String,
    pub quantity: ParameterQuantity,
    pub default: Option<f64>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
}

/// One part the library offers, pinned to one exact immutable package.
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryPart {
    pub key: String,
    /// The exact revision, `[major, minor, patch]`.
    pub revision: [u32; 3],
    /// SHA-256 address of the package.
    pub digest: String,
    pub name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub parametric: bool,
    pub parameters: Vec<LibraryParameter>,
    /// Extra words the search matches, beyond the name and category.
    pub keywords: Vec<String>,
}

impl LibraryPart {
    /// The built-in extrusion, pinned to `digest` at `revision`, with
    /// `length_default` as its Length default when it has one.
    #[must_use]
    pub fn builtin(digest: String, revision: [u32; 3], length_default: Option<f64>) -> Self {
        Self {
            key: ALUMINIUM_EXTRUSION_20X20_KEY.to_owned(),
            revision,
            digest,
            name: ALUMINIUM_EXTRUSION_20X20_NAME.to_owned(),
            description: Some(
                "Exact 20 mm × 20 mm profile with a user-resolved extrusion length. Equal variants may share evaluated geometry while every insertion remains independent."
                    .to_owned(),
            ),
            category: Some("Aluminium profiles".to_owned()),
            parametric: true,
            parameters: vec![LibraryParameter {
                key: LENGTH_PARAMETER_KEY.to_owned(),
                label: "Length".to_owned(),
                quantity: ParameterQuantity::Length,
                default: length_default,
                minimum: Some(MIN_LENGTH_MM),
                maximum: Some(MAX_LENGTH_MM),
            }],
            keywords: vec![
                "aluminium".into(),
                "profile".into(),
                "extrusion".into(),
                "parametric".into(),
            ],
        }
    }

    /// Whether this is one of the parts Artificer ships.
    #[must_use]
    pub fn is_builtin(&self) -> bool {
        self.key.starts_with("builtin.")
    }

    fn revision_label(&self) -> String {
        let [major, minor, patch] = self.revision;
        format!("{major}.{minor}.{patch}")
    }

    fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let mut haystack = self.name.to_lowercase();
        for extra in self
            .category
            .iter()
            .chain(self.description.iter())
            .chain(self.keywords.iter())
        {
            haystack.push(' ');
            haystack.push_str(&extra.to_lowercase());
        }
        haystack.contains(query)
    }
}

/// One concrete, canonical parameter assignment in an insertion intent.
#[derive(Clone, Debug, PartialEq)]
pub struct PartParameterAssignment {
    pub key: String,
    pub display_name: String,
    /// Canonical: millimetres for a length, radians for an angle, the number
    /// itself otherwise.
    pub value: f64,
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
    /// The exact revision, `[major, minor, patch]`, of the package selected.
    pub definition_revision: [u32; 3],
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
        self.value(LENGTH_PARAMETER_KEY)
    }

    /// The canonical value given for one parameter.
    #[must_use]
    pub fn value(&self, key: &str) -> Option<f64> {
        self.parameters
            .iter()
            .find(|parameter| parameter.key == key)
            .map(|parameter| parameter.value)
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

/// Whether the selected part can be added with the values typed for it, and
/// if not, which value is wrong.
#[derive(Clone, Debug, PartialEq)]
pub enum PartInsertionEligibility {
    Ready,
    NoPart,
    AlreadyStaged,
    Missing {
        parameter: String,
    },
    Invalid {
        parameter: String,
        quantity: ParameterQuantity,
    },
    NonFinite {
        parameter: String,
    },
    TooSmall {
        parameter: String,
        minimum: String,
    },
    TooLarge {
        parameter: String,
        maximum: String,
    },
}

impl PartInsertionEligibility {
    #[must_use]
    pub const fn can_stage(&self) -> bool {
        matches!(self, Self::Ready)
    }

    #[must_use]
    pub fn visible_reason(&self) -> Option<String> {
        Some(match self {
            Self::Ready => return None,
            Self::NoPart => "Pick a part in the list.".to_owned(),
            Self::AlreadyStaged => {
                "Confirm or cancel the current staged insertion before adding another part."
                    .to_owned()
            }
            Self::Missing { parameter } => {
                format!("{parameter} is required. Enter a value before adding this part.")
            }
            Self::Invalid {
                parameter,
                quantity,
            } => match quantity {
                ParameterQuantity::Length => format!(
                    "{parameter} must be a number, in the document unit or with its own (10mm, 1in)."
                ),
                ParameterQuantity::Angle => {
                    format!(
                        "{parameter} must be a number of degrees, or carry its own unit (1rad)."
                    )
                }
                ParameterQuantity::Number => format!("{parameter} must be a number."),
            },
            Self::NonFinite { parameter } => format!("{parameter} must be a finite value."),
            Self::TooSmall { parameter, minimum } => {
                format!("{parameter} must be at least {minimum}.")
            }
            Self::TooLarge { parameter, maximum } => {
                format!("{parameter} must not exceed {maximum}.")
            }
        })
    }
}

/// What a person typed for one parameter of one part.
#[derive(Clone, Debug, PartialEq)]
struct ParameterEntry {
    text: String,
    source: ParameterValueSource,
}

/// Presentation state for the Part Library window.
#[derive(Clone, Debug)]
pub struct PartLibraryState {
    open: bool,
    search: String,
    /// The document's length unit, which length fields show and read.
    length_unit: LengthUnit,
    parts: Vec<LibraryPart>,
    selected: usize,
    /// Typed values, by part key and then parameter key, kept while the
    /// library is open so each part remembers its own.
    entries: BTreeMap<String, BTreeMap<String, ParameterEntry>>,
    /// Saved pictures, by package digest.
    previews: BTreeMap<String, ShownPreview>,
    next_staging_id: u64,
    staged: Option<PartInsertionIntent>,
    committed: Vec<PartInsertionIntent>,
    status: Option<String>,
    /// Set when "Save current part" is pressed, until the workbench takes it.
    save_requested: bool,
}

/// A saved preview as the list shows it: its facts, the decoded picture,
/// and the texture made from that picture the first time it is drawn.
#[derive(Clone)]
struct ShownPreview {
    facts: PartPreviewFacts,
    image: Option<egui::ColorImage>,
    texture: Option<egui::TextureHandle>,
}

impl std::fmt::Debug for ShownPreview {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShownPreview")
            .field("facts", &self.facts)
            .field("image", &self.image.as_ref().map(|image| image.size))
            .finish_non_exhaustive()
    }
}

/// A part's rough size, as the list shows it: each extent in `unit`, and an
/// extent a parameter sets named as that parameter — `20 × 20 × 100 mm`, or
/// `20 × 20 mm × Length`.
#[must_use]
pub fn rough_dimensions(facts: &PartPreviewFacts, unit: LengthUnit) -> String {
    let fixed = facts
        .extents_mm
        .iter()
        .zip(&facts.driven_by)
        .filter(|(_, driver)| driver.is_none())
        .map(|(extent, _)| unit.format_value(*extent))
        .collect::<Vec<_>>();
    let mut text = if fixed.is_empty() {
        String::new()
    } else {
        format!("{} {}", fixed.join(" × "), unit.suffix())
    };
    for driver in facts.driven_by.iter().flatten() {
        if !text.is_empty() {
            text.push_str(" × ");
        }
        text.push_str(driver);
    }
    text
}

impl Default for PartLibraryState {
    fn default() -> Self {
        Self::with_length_default(None)
    }
}

impl PartLibraryState {
    /// A library holding only the built-in part, with an optional Length
    /// default.
    ///
    /// The production part passes `None`, making Length a required input.
    /// The constructor keeps default behavior testable.
    #[must_use]
    pub fn with_length_default(default_mm: Option<f64>) -> Self {
        let valid_default = default_mm
            .filter(|value| value.is_finite() && (MIN_LENGTH_MM..=MAX_LENGTH_MM).contains(value));
        let mut library = Self {
            open: false,
            search: String::new(),
            length_unit: LengthUnit::Millimetre,
            parts: Vec::new(),
            selected: 0,
            entries: BTreeMap::new(),
            previews: BTreeMap::new(),
            next_staging_id: 1,
            staged: None,
            committed: Vec::new(),
            status: None,
            save_requested: false,
        };
        library.set_parts(vec![LibraryPart::builtin(
            String::new(),
            [ALUMINIUM_EXTRUSION_20X20_REVISION, 0, 0],
            valid_default,
        )]);
        library
    }

    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn open_mut(&mut self) -> &mut bool {
        &mut self.open
    }

    /// Replaces the parts the library offers. The selection and typed values
    /// follow each part by key, so saving a new version of the part being
    /// looked at keeps it selected.
    pub fn set_parts(&mut self, parts: Vec<LibraryPart>) {
        let selected_key = self.selected_part().map(|part| part.key.clone());
        self.parts = parts;
        self.selected = selected_key
            .and_then(|key| self.parts.iter().position(|part| part.key == key))
            .unwrap_or(0);
        let unit = self.length_unit;
        for part in &self.parts {
            let entries = self.entries.entry(part.key.clone()).or_default();
            entries.retain(|key, _| {
                part.parameters
                    .iter()
                    .any(|parameter| parameter.key == *key)
            });
            for parameter in &part.parameters {
                entries
                    .entry(parameter.key.clone())
                    .or_insert_with(|| match parameter.default {
                        Some(default) => ParameterEntry {
                            text: format_value(parameter.quantity, default, unit),
                            source: ParameterValueSource::Default,
                        },
                        None => ParameterEntry {
                            text: String::new(),
                            source: ParameterValueSource::Entered,
                        },
                    });
            }
        }
    }

    /// The parts the library offers.
    #[must_use]
    pub fn parts(&self) -> &[LibraryPart] {
        &self.parts
    }

    /// The part whose card is showing.
    #[must_use]
    pub fn selected_part(&self) -> Option<&LibraryPart> {
        self.parts.get(self.selected)
    }

    /// Shows `key`'s card. `false` when the library has no such part.
    pub fn select_part(&mut self, key: &str) -> bool {
        match self.parts.iter().position(|part| part.key == key) {
            Some(index) => {
                self.selected = index;
                self.status = None;
                true
            }
            None => false,
        }
    }

    /// The typed text for one of the selected part's parameters.
    #[must_use]
    pub fn parameter_text(&self, key: &str) -> Option<&str> {
        let part = self.selected_part()?;
        self.entries
            .get(&part.key)
            .and_then(|entries| entries.get(key))
            .map(|entry| entry.text.as_str())
    }

    /// Types a value for one of the selected part's parameters.
    pub fn set_parameter_text(&mut self, key: &str, text: impl Into<String>) {
        let Some(part) = self.selected_part().map(|part| part.key.clone()) else {
            return;
        };
        if let Some(entry) = self
            .entries
            .get_mut(&part)
            .and_then(|entries| entries.get_mut(key))
        {
            entry.text = text.into();
            entry.source = ParameterValueSource::Entered;
            self.status = None;
        }
    }

    /// The selected part's Length field, which the built-in extrusion and
    /// most saved parts have.
    #[must_use]
    pub fn length_text(&self) -> &str {
        self.parameter_text(LENGTH_PARAMETER_KEY).unwrap_or("")
    }

    pub fn set_length_text(&mut self, text: impl Into<String>) {
        self.set_parameter_text(LENGTH_PARAMETER_KEY, text);
    }

    /// The unit length fields show and read.
    #[must_use]
    pub const fn length_unit(&self) -> LengthUnit {
        self.length_unit
    }

    /// Follows the document's unit. A length already in a field is
    /// re-rendered in the new unit, so `80` typed as millimetres does not
    /// sit there reading as eighty inches.
    pub fn set_length_unit(&mut self, unit: LengthUnit) {
        if self.length_unit == unit {
            return;
        }
        let old = self.length_unit;
        for part in &self.parts {
            let Some(entries) = self.entries.get_mut(&part.key) else {
                continue;
            };
            for parameter in &part.parameters {
                if parameter.quantity != ParameterQuantity::Length {
                    continue;
                }
                if let Some(entry) = entries.get_mut(&parameter.key)
                    && let Ok(millimetres) = old.parse(&entry.text)
                {
                    entry.text = unit.format_value(millimetres);
                }
            }
        }
        self.length_unit = unit;
    }

    /// Pins the built-in part to one exact immutable catalog package.
    pub(crate) fn set_definition(&mut self, digest: impl Into<String>, revision: [u32; 3]) {
        let digest = digest.into();
        if let Some(part) = self
            .parts
            .iter_mut()
            .find(|part| part.key == ALUMINIUM_EXTRUSION_20X20_KEY)
        {
            part.digest = digest;
            part.revision = revision;
        }
    }

    /// The exact revision of the package the selected card is pinned to.
    #[must_use]
    pub fn definition_revision(&self) -> [u32; 3] {
        self.selected_part().map_or([0, 0, 0], |part| part.revision)
    }

    #[must_use]
    pub fn definition_digest(&self) -> &str {
        self.selected_part().map_or("", |part| part.digest.as_str())
    }

    /// Shows the preview saved with the package `digest`, or none. A picture
    /// that does not decode leaves the list's placeholder, with the size
    /// still shown.
    pub(crate) fn set_preview(&mut self, digest: &str, preview: Option<&PartPreview>) {
        match preview {
            Some(preview) => {
                self.previews.insert(
                    digest.to_owned(),
                    ShownPreview {
                        facts: preview.facts.clone(),
                        image: decode_png(&preview.image_png),
                        texture: None,
                    },
                );
            }
            None => {
                self.previews.remove(digest);
            }
        }
    }

    fn preview_of(&self, part: &LibraryPart) -> Option<&ShownPreview> {
        self.previews.get(&part.digest)
    }

    /// The measurements saved with the selected part's preview.
    #[must_use]
    pub fn preview_facts(&self) -> Option<&PartPreviewFacts> {
        self.selected_part()
            .and_then(|part| self.preview_of(part))
            .map(|preview| &preview.facts)
    }

    /// The size of the picture the list shows for the selected part, in
    /// pixels, if it has one.
    #[must_use]
    pub fn preview_image_size(&self) -> Option<[usize; 2]> {
        self.selected_part()
            .and_then(|part| self.preview_of(part))
            .and_then(|preview| preview.image.as_ref())
            .map(|image| image.size)
    }

    /// The selected part's rough size in the document unit.
    #[must_use]
    pub fn rough_dimensions_text(&self) -> Option<String> {
        self.preview_facts()
            .map(|facts| rough_dimensions(facts, self.length_unit))
    }

    /// Whether "Save current part" was pressed since the last call.
    pub(crate) fn take_save_request(&mut self) -> bool {
        std::mem::take(&mut self.save_requested)
    }

    /// Reads every value typed for the selected part.
    pub fn resolved_values(
        &self,
    ) -> Result<Vec<PartParameterAssignment>, PartInsertionEligibility> {
        let Some(part) = self.selected_part() else {
            return Err(PartInsertionEligibility::NoPart);
        };
        let entries = self.entries.get(&part.key);
        part.parameters
            .iter()
            .map(|parameter| {
                let entry = entries.and_then(|entries| entries.get(&parameter.key));
                let text = entry.map_or("", |entry| entry.text.trim());
                let source = entry.map_or(ParameterValueSource::Entered, |entry| entry.source);
                let value = read_value(parameter, text, self.length_unit)?;
                Ok(PartParameterAssignment {
                    key: parameter.key.clone(),
                    display_name: parameter.label.clone(),
                    value,
                    source,
                })
            })
            .collect()
    }

    /// Whether the selected part can be staged with what is typed.
    #[must_use]
    pub fn eligibility(&self) -> PartInsertionEligibility {
        match self.resolved_values() {
            Ok(_) => PartInsertionEligibility::Ready,
            Err(reason) => reason,
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

    /// Stages the selected part at the typed values without committing
    /// workspace state.
    pub fn stage_selected(&mut self) -> Result<u64, PartInsertionEligibility> {
        if self.staged.is_some() {
            return Err(PartInsertionEligibility::AlreadyStaged);
        }
        let parameters = self.resolved_values()?;
        let part = self
            .selected_part()
            .ok_or(PartInsertionEligibility::NoPart)?;
        let staging_id = self.next_staging_id;
        let intent = PartInsertionIntent {
            staging_id,
            definition_key: part.key.clone(),
            definition_revision: part.revision,
            definition_digest: part.digest.clone(),
            display_name: part.name.clone(),
            parameters,
        };
        self.next_staging_id = self.next_staging_id.saturating_add(1);
        self.staged = Some(intent);
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
        let values = self.describe_values(&staged);
        let name = staged.display_name.clone();
        if self.committed.len() == MAX_COMMITTED_INTENTS {
            self.committed.remove(0);
        }
        self.committed.push(staged);
        self.status = Some(if values.is_empty() {
            format!("{name} accepted for workspace insertion.")
        } else {
            format!("{name} · {values} accepted for workspace insertion.")
        });
        true
    }

    fn describe_values(&self, intent: &PartInsertionIntent) -> String {
        let part = self
            .parts
            .iter()
            .find(|part| part.key == intent.definition_key);
        intent
            .parameters
            .iter()
            .map(|assignment| {
                let quantity = part
                    .and_then(|part| {
                        part.parameters
                            .iter()
                            .find(|parameter| parameter.key == assignment.key)
                    })
                    .map_or(ParameterQuantity::Number, |parameter| parameter.quantity);
                // A part with one parameter says only its value, as the
                // built-in extrusion always has.
                let value = format_value_with_unit(quantity, assignment.value, self.length_unit);
                if intent.parameters.len() == 1 {
                    value
                } else {
                    format!("{} {value}", assignment.display_name)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
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

    /// Shows a line under the library's card: what was saved, or why not.
    pub(crate) fn set_status(&mut self, status: impl Into<String>) {
        self.status = Some(status.into());
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
                    .fill(library_panel())
                    .stroke(Stroke::new(1.0, library_border())),
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
                    .color(library_accent())
                    .strong(),
            );
            ui.separator();
            ui.label(
                RichText::new("Immutable definitions · exact revision insertion")
                    .small()
                    .color(library_muted()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let save = ui
                    .add_enabled(
                        !another_operation_pending,
                        egui::Button::new(RichText::new("Save current part…").small()),
                    )
                    .on_hover_text(
                        "Save the part you are working on into the library, with its variables as the values it takes.",
                    );
                if save.clicked() {
                    self.save_requested = true;
                }
            });
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
        let query = self.search.trim().to_lowercase();
        let visible = self
            .parts
            .iter()
            .enumerate()
            .filter(|(_, part)| part.matches(&query))
            .map(|(index, part)| (index, part.is_builtin()))
            .collect::<Vec<_>>();
        if visible.is_empty() {
            ui.label(
                RichText::new("No local parts match this search.")
                    .color(library_muted())
                    .italics(),
            );
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("part_library_list")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (heading, builtin) in [("STANDARD COMPONENTS", true), ("MY PARTS", false)] {
                    let group = visible
                        .iter()
                        .filter(|(_, is_builtin)| *is_builtin == builtin)
                        .map(|(index, _)| *index)
                        .collect::<Vec<_>>();
                    if group.is_empty() {
                        continue;
                    }
                    ui.label(
                        RichText::new(heading)
                            .small()
                            .color(library_muted())
                            .strong(),
                    );
                    ui.add_space(5.0);
                    for index in group {
                        self.part_row(ui, index);
                        ui.add_space(6.0);
                    }
                    ui.add_space(4.0);
                }
            });
    }

    fn part_row(&mut self, ui: &mut egui::Ui, index: usize) {
        let selected = index == self.selected;
        let part = self.parts[index].clone();
        let version = format!("v{}", part.revision_label());
        let size = self
            .preview_of(&part)
            .map(|preview| rough_dimensions(&preview.facts, self.length_unit));
        let sample = self
            .preview_of(&part)
            .and_then(|preview| preview.facts.sample.clone());
        let stroke = if selected {
            Stroke::new(1.0, library_accent().gamma_multiply(0.65))
        } else {
            Stroke::new(1.0, library_border())
        };
        egui::Frame::new()
            .fill(library_card())
            .stroke(stroke)
            .corner_radius(4)
            .inner_margin(egui::Margin::same(7))
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    self.thumbnail(ui, &part);
                    ui.vertical(|ui| {
                        let response = ui.add(
                            egui::Button::new(
                                RichText::new(&part.name).color(library_text()).strong(),
                            )
                            .wrap_mode(egui::TextWrapMode::Wrap)
                            .frame(false)
                            .selected(selected),
                        );
                        let name = part.name.clone();
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name)
                        });
                        if response.clicked() {
                            self.selected = index;
                            self.status = None;
                        }
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&version).small().color(library_text()));
                            ui.label(
                                RichText::new(if part.parametric {
                                    "PARAMETRIC"
                                } else {
                                    "FIXED"
                                })
                                .small()
                                .color(library_accent()),
                            );
                        });
                        if let Some(size) = &size {
                            let label =
                                ui.label(RichText::new(size).small().color(library_muted()));
                            if let Some(sample) = &sample {
                                label.on_hover_text(format!(
                                    "Rough size; the picture shows it at {sample}"
                                ));
                            }
                        }
                        if let Some(category) = &part.category {
                            ui.label(RichText::new(category).small().color(library_muted()));
                        }
                    });
                });
            });
    }

    /// The part's saved picture, or a quiet placeholder of the same size so
    /// the row does not jump when a picture is missing.
    fn thumbnail(&mut self, ui: &mut egui::Ui, part: &LibraryPart) {
        let side = egui::vec2(THUMBNAIL_SIDE, THUMBNAIL_SIDE);
        let texture = self.previews.get_mut(&part.digest).and_then(|preview| {
            if preview.texture.is_none()
                && let Some(image) = preview.image.clone()
            {
                preview.texture = Some(ui.ctx().load_texture(
                    format!("part_library_preview_{}", part.digest),
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
            preview.texture.clone()
        });
        let label = format!("Picture of {}", part.name);
        let (rect, response) = ui.allocate_exact_size(side, egui::Sense::hover());
        ui.painter().rect(
            rect,
            4,
            library_panel(),
            Stroke::new(1.0, library_border()),
            egui::StrokeKind::Inside,
        );
        if let Some(texture) = &texture {
            egui::Image::new((texture.id(), side)).paint_at(ui, rect.shrink(2.0));
        }
        let has_picture = texture.is_some();
        response.widget_info(|| {
            let mut info = egui::WidgetInfo::labeled(egui::WidgetType::Image, true, &label);
            if !has_picture {
                info.label = Some(format!("{label} (not drawn yet)"));
            }
            info
        });
    }

    fn part_details(&mut self, ui: &mut egui::Ui, another_operation_pending: bool) -> bool {
        let Some(part) = self.selected_part().cloned() else {
            ui.label(
                RichText::new("The library is empty.")
                    .color(library_muted())
                    .italics(),
            );
            return false;
        };
        ui.label(
            RichText::new(&part.name)
                .font(FontId::proportional(17.0))
                .color(library_text())
                .strong(),
        );
        let revision = part.revision_label();
        let kind = if part.parametric {
            "Parametric part"
        } else {
            "Fixed part"
        };
        let package_identity = if part.digest.len() == 64 {
            format!(
                "{kind} · revision {revision} · verified {}…",
                &part.digest[..12]
            )
        } else {
            format!("{kind} · revision {revision} · package unavailable")
        };
        ui.label(
            RichText::new(package_identity)
                .small()
                .color(library_accent()),
        );
        if let Some(size) = self.rough_dimensions_text() {
            ui.label(
                RichText::new(format!("Size · {size}"))
                    .small()
                    .color(library_text()),
            );
        }
        if let Some(description) = &part.description {
            ui.add_space(5.0);
            ui.label(RichText::new(description).color(library_muted()));
        }
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);
        if !part.parameters.is_empty() {
            ui.label(
                RichText::new("PARAMETERS")
                    .small()
                    .color(library_muted())
                    .strong(),
            );
            ui.add_space(5.0);
        }
        let unit = self.length_unit;
        for parameter in &part.parameters {
            let Some(entry) = self
                .entries
                .get_mut(&part.key)
                .and_then(|entries| entries.get_mut(&parameter.key))
            else {
                continue;
            };
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&parameter.label)
                        .color(library_text())
                        .strong(),
                );
                if parameter.default.is_some() && entry.source == ParameterValueSource::Default {
                    ui.label(RichText::new("DEFAULT").small().color(library_good()));
                } else if parameter.default.is_none() {
                    ui.label(RichText::new("REQUIRED").small().color(library_accent()));
                }
            });
            let editor = ui.add(
                egui::TextEdit::singleline(&mut entry.text)
                    .id(egui::Id::new((
                        "part_library_parameter",
                        &part.key,
                        &parameter.key,
                    )))
                    .desired_width(190.0),
            );
            let (suffix, unit_name) = match parameter.quantity {
                ParameterQuantity::Length => (unit.suffix().to_owned(), unit.name().to_owned()),
                ParameterQuantity::Angle => ("deg".to_owned(), "degrees".to_owned()),
                ParameterQuantity::Number => (String::new(), String::new()),
            };
            let field_label = if suffix.is_empty() {
                parameter.label.clone()
            } else {
                format!("{} ({suffix})", parameter.label)
            };
            let description = format!(
                "{} for this insertion{}. A valid value enables Add to current workspace.",
                parameter.label,
                if unit_name.is_empty() {
                    String::new()
                } else {
                    format!(", in {unit_name} or with its own unit suffix")
                }
            );
            editor.ctx.accesskit_node_builder(editor.id, |node| {
                node.set_label(field_label.clone());
                node.set_description(description.clone());
            });
            if !unit_name.is_empty() {
                ui.label(RichText::new(&unit_name).small().color(library_muted()));
            }
            if editor.changed() {
                entry.source = ParameterValueSource::Entered;
                self.status = None;
            }
            ui.add_space(4.0);
        }

        let eligibility = self.eligibility();
        if let Some(reason) = eligibility.visible_reason() {
            ui.label(RichText::new(reason).small().color(library_bad()));
        } else if let Ok(values) = self.resolved_values() {
            let resolved = values
                .iter()
                .map(|assignment| {
                    let quantity = part
                        .parameters
                        .iter()
                        .find(|parameter| parameter.key == assignment.key)
                        .map_or(ParameterQuantity::Number, |parameter| parameter.quantity);
                    let source = match assignment.source {
                        ParameterValueSource::Default => "definition default",
                        ParameterValueSource::Entered => "entered value",
                    };
                    format!(
                        "{} · {source}",
                        format_value_with_unit(quantity, assignment.value, unit)
                    )
                })
                .collect::<Vec<_>>()
                .join("  ·  ");
            if !resolved.is_empty() {
                ui.label(
                    RichText::new(format!("Resolved · {resolved}"))
                        .small()
                        .color(library_good()),
                );
            }
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            let blocked_reason = if another_operation_pending || self.staged.is_some() {
                Some(
                    "Confirm or cancel the current staged operation before adding another part."
                        .to_owned(),
                )
            } else {
                eligibility.visible_reason()
            };
            let add = ui.add_enabled(
                blocked_reason.is_none(),
                egui::Button::new(
                    RichText::new("Add to current workspace")
                        .color(library_text())
                        .strong(),
                )
                .fill(library_accent().gamma_multiply(0.28))
                .stroke(Stroke::new(1.0, library_accent()))
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
                ui.label(RichText::new(status).small().color(library_muted()));
            }
            add.clicked()
        })
        .inner
    }
}

/// A canonical value as its field shows it, without a unit.
fn format_value(quantity: ParameterQuantity, value: f64, unit: LengthUnit) -> String {
    match quantity {
        ParameterQuantity::Length => unit.format_value(value),
        ParameterQuantity::Angle => trim_number(value.to_degrees()),
        ParameterQuantity::Number => trim_number(value),
    }
}

/// A canonical value with its unit, as a person reads it.
fn format_value_with_unit(quantity: ParameterQuantity, value: f64, unit: LengthUnit) -> String {
    match quantity {
        ParameterQuantity::Length => unit.format(value),
        ParameterQuantity::Angle => format!("{}°", trim_number(value.to_degrees())),
        ParameterQuantity::Number => trim_number(value),
    }
}

fn trim_number(value: f64) -> String {
    let text = format!("{value:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

/// Reads one parameter's typed text into its canonical value, or says why it
/// cannot be read. An empty field takes the default when there is one.
fn read_value(
    parameter: &LibraryParameter,
    text: &str,
    unit: LengthUnit,
) -> Result<f64, PartInsertionEligibility> {
    let name = || parameter.label.clone();
    if text.is_empty() {
        return parameter
            .default
            .ok_or_else(|| PartInsertionEligibility::Missing { parameter: name() });
    }
    // The reader refuses `NaN` and `inf` as not numbers; they are numbers
    // of a kind, and the diagnostic says so.
    let non_finite = text.parse::<f64>().is_ok_and(|value| !value.is_finite());
    let parsed = match parameter.quantity {
        ParameterQuantity::Length => unit.parse(text).ok(),
        ParameterQuantity::Angle => evaluate_entry(text, FieldUnit::degrees(), &|_| None).ok(),
        ParameterQuantity::Number => evaluate_entry(text, FieldUnit::SCALAR, &|_| None).ok(),
    };
    let Some(value) = parsed.filter(|value| value.is_finite()) else {
        return Err(if non_finite {
            PartInsertionEligibility::NonFinite { parameter: name() }
        } else {
            PartInsertionEligibility::Invalid {
                parameter: name(),
                quantity: parameter.quantity,
            }
        });
    };
    if let Some(minimum) = parameter.minimum
        && value < minimum
    {
        return Err(PartInsertionEligibility::TooSmall {
            parameter: name(),
            minimum: format_bound(parameter.quantity, minimum),
        });
    }
    if let Some(maximum) = parameter.maximum
        && value > maximum
    {
        return Err(PartInsertionEligibility::TooLarge {
            parameter: name(),
            maximum: format_bound(parameter.quantity, maximum),
        });
    }
    Ok(value)
}

/// A limit in the part's own terms: lengths in millimetres, as the
/// definition states them.
fn format_bound(quantity: ParameterQuantity, value: f64) -> String {
    match quantity {
        ParameterQuantity::Length => format!("{} mm", trim_number(value)),
        ParameterQuantity::Angle => format!("{}°", trim_number(value.to_degrees())),
        ParameterQuantity::Number => trim_number(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn missing_length() -> PartInsertionEligibility {
        PartInsertionEligibility::Missing {
            parameter: "Length".into(),
        }
    }

    #[test]
    fn rough_dimensions_name_what_a_parameter_sets_and_follow_the_unit() {
        let extrusion = PartPreviewFacts {
            extents_mm: [20.0, 20.0, 100.0],
            driven_by: [None, None, Some("Length".into())],
            sample: Some("Length 100 mm".into()),
        };
        assert_eq!(
            rough_dimensions(&extrusion, LengthUnit::Millimetre),
            "20 × 20 mm × Length"
        );
        let fixed = PartPreviewFacts {
            extents_mm: [25.4, 50.8, 12.7],
            driven_by: [None, None, None],
            sample: None,
        };
        assert_eq!(rough_dimensions(&fixed, LengthUnit::Inch), "1 × 2 × 0.5 in");
        assert_eq!(
            rough_dimensions(&fixed, LengthUnit::Millimetre),
            "25.4 × 50.8 × 12.7 mm"
        );
    }

    #[test]
    fn required_length_blocks_staging_with_precise_diagnostics() {
        let mut library = PartLibraryState::default();
        assert_eq!(library.eligibility(), missing_length());
        assert_eq!(library.stage_selected(), Err(missing_length()));
        assert_eq!(
            library.eligibility().visible_reason().as_deref(),
            Some("Length is required. Enter a value before adding this part.")
        );

        library.set_length_text("not-a-number");
        assert!(matches!(
            library.eligibility(),
            PartInsertionEligibility::Invalid { .. }
        ));
        library.set_length_text("NaN");
        assert!(matches!(
            library.eligibility(),
            PartInsertionEligibility::NonFinite { .. }
        ));
        library.set_length_text("0");
        assert_eq!(
            library.eligibility().visible_reason().as_deref(),
            Some("Length must be at least 0.001 mm.")
        );
        library.set_length_text("100001");
        assert_eq!(
            library.eligibility().visible_reason().as_deref(),
            Some("Length must not exceed 100000 mm.")
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

    /// A saved part with a length and an angle is offered beside the
    /// built-in one; each part keeps its own values, and its defaults fill
    /// its fields until something is typed.
    #[test]
    fn every_part_keeps_its_own_values_and_defaults() {
        let mut library = PartLibraryState::default();
        let bracket = LibraryPart {
            key: "user.bracket".into(),
            revision: [2, 0, 0],
            digest: "b".repeat(64),
            name: "Bracket".into(),
            description: None,
            category: Some("My parts".into()),
            parametric: true,
            parameters: vec![
                LibraryParameter {
                    key: "length".into(),
                    label: "Length".into(),
                    quantity: ParameterQuantity::Length,
                    default: Some(40.0),
                    minimum: None,
                    maximum: None,
                },
                LibraryParameter {
                    key: "tilt".into(),
                    label: "Tilt".into(),
                    quantity: ParameterQuantity::Angle,
                    default: Some(30.0_f64.to_radians()),
                    minimum: None,
                    maximum: None,
                },
            ],
            keywords: Vec::new(),
        };
        let mut parts = library.parts().to_vec();
        parts.push(bracket);
        library.set_parts(parts);
        library.set_length_text("310");

        assert!(library.select_part("user.bracket"));
        assert_eq!(library.length_text(), "40");
        assert_eq!(library.parameter_text("tilt"), Some("30"));
        library.set_parameter_text("tilt", "45");
        let staged = library.stage_selected().expect("the bracket stages");
        let intent = library.staged_intent().unwrap().clone();
        assert!(library.commit_staged(staged));
        assert_eq!(intent.definition_key, "user.bracket");
        assert_eq!(intent.definition_revision, [2, 0, 0]);
        assert_eq!(intent.value("length"), Some(40.0));
        assert!((intent.value("tilt").unwrap() - 45.0_f64.to_radians()).abs() < 1.0e-12);
        assert_eq!(intent.parameters[0].source, ParameterValueSource::Default);
        assert_eq!(intent.parameters[1].source, ParameterValueSource::Entered);

        assert!(library.select_part(ALUMINIUM_EXTRUSION_20X20_KEY));
        assert_eq!(library.length_text(), "310", "the built-in kept its own");
        library.set_parameter_text("length", "1in");
        let staged = library.stage_selected().unwrap();
        assert_eq!(library.staged_intent().unwrap().length_mm(), Some(25.4));
        assert!(library.commit_staged(staged));
    }
}
