//! Artificer Script Studio: a live `.art` visualiser on the Artificer kernel.
//!
//! The shape is the one OpenSCAD users know: the script on the left, the
//! model on the right, the console along the bottom. Every edit re-runs the
//! script against the kernel on a worker thread, a superseded run is
//! cancelled rather than waited for, and the `param` lines become a
//! customizer whose sliders re-run the script without touching the text.
//!
//! The studio depends on the kernel the way every other client does: through
//! the kernel's own API session. It executes no kernel command of its own.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use artificer_kernel::api::commands::ApiCommand;
use artificer_kernel::api::debug::{ApiError, ApiErrorCode};
use artificer_kernel::api::decompile::DecompileOptions;
use artificer_kernel::api::diff::ScriptDiff;
use artificer_kernel::api::export::{export_obj, export_stl_binary};
use artificer_kernel::api::scripting::{
    FileModules, ModuleResolver, NoModules, ScriptError, ScriptParameter, compile_program_with,
    script_parameters,
};
use artificer_kernel::api::selectors::EntitySelector;
use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, DebugScene, NativeKernel, Snapshot};
use artificer_protocol::Vector3;
use artificer_protocol::{
    Aabb3, DiagnosticSeverity, EntityKind, EntityRef, Point3, Tier, TopologyCounts,
};
use artificer_spacemouse::SpaceMouse;
use artificer_ui_core::navigation::NavigationPreset;
use artificer_ui_core::presentation::{
    ActiveTool, DisplayTransform, SectionCutPlane, SixDofFilter, SixDofMotion, SixDofSettings,
    ViewState,
};
use artificer_ui_core::theme::{self, WorkbenchTheme};
use artificer_viewport::{
    BodyInstanceKey, DocumentBodyInstance, DocumentFaceSelection, EdgeFrameMemo,
    FeaturePreviewDragState, ModelDisplayMode, show_document_with_feature_drag,
};
use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{Color32, FontId, RichText};

/// How long the editor stays quiet before an edit re-runs the script. Short
/// enough to feel live, long enough that typing a number does not run the
/// kernel once per digit.
pub const AUTO_RUN_DEBOUNCE: Duration = Duration::from_millis(300);

/// The scripts bundled with the kernel, offered from the Examples menu so a
/// first launch has something to look at and something to copy from.
pub const EXAMPLES: &[(&str, &str)] = &[
    (
        "Flanged hub",
        include_str!("../../../crates/kernel/examples/flanged_hub.art"),
    ),
    (
        "Filleted flange",
        include_str!("../../../crates/kernel/examples/filleted_flange.art"),
    ),
    (
        "Standoff plate (functions)",
        include_str!("../../../crates/kernel/examples/standoff_plate.art"),
    ),
    (
        "Bearing mount",
        include_str!("../../../crates/kernel/examples/bearing_mount.art"),
    ),
    (
        "Three holes and a cut",
        include_str!("../../../crates/kernel/examples/three_holes_and_cut.art"),
    ),
    (
        "Filleted cube",
        include_str!("../../../crates/kernel/examples/filleted_cube.art"),
    ),
];

/// The script a fresh window opens on.
pub const WELCOME_SCRIPT: &str = EXAMPLES[0].1;

/// What File ▸ New script puts in the editor.
const NEW_SCRIPT: &str = "// A new script. Every step needs a label.\nlet body = box(size: [40, 30, 10], label: \"body\");\n";

const BODY: BodyInstanceKey = BodyInstanceKey::new(1);
const EDITOR_ID: &str = "script-studio-editor";

// ---------------------------------------------------------------------------
// Running a script
// ---------------------------------------------------------------------------

/// One executed step as the console reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct StepReport {
    pub label: String,
    pub topology: TopologyCounts,
    pub elapsed_ms: u64,
    /// Warnings and notes the kernel attached, such as the faceted tier's
    /// approximation warning.
    pub notes: Vec<String>,
}

/// A face of the finished body and the names it answers to.
///
/// The first name is the one the script gave it with a `let` bound to a
/// selector, when there is one; otherwise it is the step and role that
/// made the face, which `step.face("role")` selects. The description is
/// read off the geometry so a person can tell which face a name means.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceName {
    pub entity: EntityRef,
    /// The script's own name for the face, from `let name = <selector>`.
    pub script_name: Option<String>,
    /// `step.role`, from the step that produced the face.
    pub history_name: String,
    /// Planar or curved, which way it faces, and where its centre is.
    pub description: String,
}

impl FaceName {
    /// The name to show: the script's, or the history's.
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.script_name.as_deref().unwrap_or(&self.history_name)
    }
}

/// The face roles one step reported, by `(role, ordinal)`, keyed by the
/// step's label: the raw material of history names.
type ReportedRoles = Vec<(String, Vec<(String, Option<u32>)>)>;

/// One entity the kernel offered when a selector matched too many or too
/// few: the thing to click, and how the kernel describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct RunCandidate {
    pub entity: EntityRef,
    /// "face 12 · planar, facing up (role top)": what a person needs to
    /// tell the candidates apart.
    pub description: String,
}

/// Why a run stopped short of the end of the script.
///
/// The kernel refuses a step with more than a message: a suggestion of what
/// to try, the entities a selector could have meant, and a code that names
/// the refusal exactly. The console shows all of it, message first, code
/// last, because the message is what a person acts on and the code is what
/// a bug report needs.
#[derive(Clone, Debug, PartialEq)]
pub struct RunError {
    /// "Parse error", "Evaluation error", or the failing step's label.
    pub kind: String,
    pub message: String,
    /// One-based `(line, column)` in the script when it is known.
    pub location: Option<(usize, usize)>,
    /// What the kernel proposes doing about it, when it has an idea.
    pub suggestion: Option<String>,
    /// The entities a selector could have meant, for an ambiguous or
    /// near-miss selection.
    pub candidates: Vec<RunCandidate>,
    /// The messages of the diagnostics behind the refusal, when they say
    /// more than the headline does.
    pub details: Vec<String>,
    /// The refusal's code, `selector_ambiguous` or `kernel_error ·
    /// profile_not_closed`, for a script error there is none.
    pub code: Option<String>,
}

impl RunError {
    fn from_script(error: &ScriptError) -> Self {
        Self {
            kind: error.kind().to_owned(),
            message: error.message().to_owned(),
            location: error.location(),
            suggestion: None,
            candidates: Vec::new(),
            details: Vec::new(),
            code: None,
        }
    }

    fn from_step(source: &str, command: &ApiCommand, error: &ApiError) -> Self {
        let label = command.label();
        let candidates = error
            .candidates
            .iter()
            .map(|info| {
                let mut description = format!(
                    "{} {} · {}",
                    info.kind, info.entity_ref.entity.0, info.geometry_description
                );
                if let Some(role) = &info.role {
                    match info.ordinal {
                        Some(ordinal) => {
                            description.push_str(&format!(" (role {role}[{ordinal}])"))
                        }
                        None => description.push_str(&format!(" (role {role})")),
                    }
                }
                RunCandidate {
                    entity: info.entity_ref,
                    description,
                }
            })
            .collect();
        // A diagnostic often repeats the headline; only the ones that add
        // something are worth a line.
        let details = error
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .filter(|message| !message.is_empty() && *message != error.message)
            .collect();
        let code = error.diagnostics.first().map_or_else(
            || api_error_code_label(error.code).to_owned(),
            |diagnostic| format!("{} · {}", api_error_code_label(error.code), diagnostic.code),
        );
        Self {
            kind: format!("Step \"{label}\""),
            message: error.message.clone(),
            location: line_of_label(source, label).map(|line| (line, 1)),
            suggestion: error.suggestion.clone(),
            candidates,
            details,
            code: Some(code),
        }
    }

    /// The console's first line for the error: what failed, where, and the
    /// message in plain words.
    #[must_use]
    pub fn headline(&self) -> String {
        match self.location {
            Some((line, column)) => format!(
                "✕ {} at line {line}, column {column}: {}",
                self.kind, self.message
            ),
            None => format!("✕ {}: {}", self.kind, self.message),
        }
    }

    /// The lines the console prints beneath the headline, in order: the
    /// diagnostics' own messages, what to try, the candidate entities, and
    /// the code last.
    #[must_use]
    pub fn console_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.details.clone();
        if let Some(suggestion) = &self.suggestion {
            lines.push(format!("Try: {suggestion}"));
        }
        if !self.candidates.is_empty() {
            lines.push(format!(
                "{} candidate{}:",
                self.candidates.len(),
                if self.candidates.len() == 1 { "" } else { "s" }
            ));
            lines.extend(
                self.candidates
                    .iter()
                    .map(|candidate| format!("· {}", candidate.description)),
            );
        }
        if let Some(code) = &self.code {
            lines.push(code.clone());
        }
        lines
    }
}

/// The refusal category as its wire name, the one a bug report quotes.
const fn api_error_code_label(code: ApiErrorCode) -> &'static str {
    match code {
        ApiErrorCode::InvalidInput => "invalid_input",
        ApiErrorCode::SelectorNotFound => "selector_not_found",
        ApiErrorCode::SelectorAmbiguous => "selector_ambiguous",
        ApiErrorCode::KernelError => "kernel_error",
        ApiErrorCode::ValidationFailed => "validation_failed",
        ApiErrorCode::SessionError => "session_error",
        ApiErrorCode::ScriptError => "script_error",
        ApiErrorCode::IoError => "io_error",
    }
}

/// Everything one run of a script produced.
#[derive(Debug)]
pub struct RunOutcome {
    /// The edit generation this run answers; the studio drops answers to
    /// generations it has moved past.
    pub generation: u64,
    pub steps: Vec<StepReport>,
    pub error: Option<RunError>,
    /// The snapshot the last successful step left behind, so the model
    /// stays visible up to the step that failed.
    pub snapshot: Option<Snapshot>,
    pub scene: Option<DebugScene>,
    /// Every face of the finished body with its names, script names first.
    pub faces: Vec<FaceName>,
    /// The session journal as JSON, for export and for the round trip
    /// back into a script.
    pub journal: Option<String>,
    pub elapsed: Duration,
    pub cancelled: bool,
}

impl RunOutcome {
    /// The name record of one face of the finished body.
    #[must_use]
    pub fn face_name(&self, entity: EntityRef) -> Option<&FaceName> {
        self.faces.iter().find(|face| face.entity == entity)
    }

    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.error.is_none() && !self.cancelled
    }
}

/// Compiles and runs a script to completion on the calling thread. The studio
/// calls this from a worker; tests call it directly.
#[must_use]
pub fn run_script(
    source: &str,
    overrides: &BTreeMap<String, f64>,
    token: &CancellationToken,
) -> RunOutcome {
    run_script_generation(0, source, overrides, None, token)
}

/// [`run_script`] for a script saved at `path`, whose `use` lines resolve
/// beside it.
#[must_use]
pub fn run_script_at(
    source: &str,
    overrides: &BTreeMap<String, f64>,
    path: Option<&Path>,
    token: &CancellationToken,
) -> RunOutcome {
    run_script_generation(0, source, overrides, path, token)
}

fn run_script_generation(
    generation: u64,
    source: &str,
    overrides: &BTreeMap<String, f64>,
    path: Option<&Path>,
    token: &CancellationToken,
) -> RunOutcome {
    let started = Instant::now();
    let mut outcome = RunOutcome {
        generation,
        steps: Vec::new(),
        error: None,
        snapshot: None,
        scene: None,
        faces: Vec::new(),
        journal: None,
        elapsed: Duration::ZERO,
        cancelled: false,
    };

    // A saved script's modules live beside it; an unsaved one has nowhere
    // to look, and a `use` says so.
    let modules: Box<dyn ModuleResolver> = match path {
        Some(path) => Box::new(FileModules::beside(path)),
        None => Box::new(NoModules),
    };
    let program = match compile_program_with(source, overrides, modules.as_ref()) {
        Ok(program) => program,
        Err(error) => {
            outcome.error = Some(RunError::from_script(&error));
            outcome.elapsed = started.elapsed();
            return outcome;
        }
    };

    let mut session = Session::new();
    let mut built_anything = false;
    // The roles every step reported, for naming faces afterwards.
    let mut reported: ReportedRoles = Vec::new();
    for command in program.commands {
        if token.is_cancelled() {
            outcome.cancelled = true;
            break;
        }
        // An edge finish reports every face of the body under a generic
        // role; those say nothing about which face is which, so they never
        // name one. The faces it made keep the fallback name.
        let edge_finish = matches!(
            command,
            ApiCommand::Fillet { .. } | ApiCommand::Chamfer { .. }
        );
        match session.execute(command.clone(), token) {
            Ok(result) => {
                if !matches!(command, ApiCommand::Sketch { .. }) {
                    built_anything = true;
                }
                reported.push((
                    result.step_label.clone(),
                    result
                        .entities
                        .values()
                        .filter(|info| info.kind == EntityKind::Face)
                        .filter_map(|info| info.role.clone().map(|role| (role, info.ordinal)))
                        .filter(|(role, _)| !(edge_finish && role == "face"))
                        .collect(),
                ));
                let mut notes: Vec<String> = result
                    .warnings
                    .iter()
                    .chain(&result.diagnostics)
                    .filter(|diagnostic| diagnostic.severity != DiagnosticSeverity::Error)
                    .map(|diagnostic| diagnostic.message.clone())
                    .collect();
                if let Some(rung) = &result.rung {
                    notes.push(format!(
                        "{} · {rung}",
                        match result.tier {
                            Tier::Exact => "exact",
                            Tier::Approximate => "approximate",
                        }
                    ));
                }
                outcome.steps.push(StepReport {
                    label: result.step_label,
                    topology: result.topology,
                    elapsed_ms: result.elapsed_ms,
                    notes,
                });
            }
            Err(error) => {
                if token.is_cancelled() {
                    outcome.cancelled = true;
                } else {
                    outcome.error = Some(RunError::from_step(source, &command, &error));
                }
                break;
            }
        }
    }

    if built_anything && !outcome.cancelled {
        let snapshot = session.snapshot.clone();
        let scene = NativeKernel::debug_scene(&snapshot);
        outcome.faces = name_faces(&session, &scene, &reported, &program.names);
        outcome.scene = Some(scene);
        outcome.snapshot = Some(snapshot);
        outcome.journal = session.export_journal().ok();
    }
    outcome.elapsed = started.elapsed();
    outcome
}

/// Names every face of the session's current body.
///
/// History names come from the steps in order, so a face carries the name
/// of the last step that made or reshaped it. Script names come from the
/// program's selector bindings, resolved against the finished body; a
/// binding that no longer finds a face, or finds an edge, names nothing.
fn name_faces(
    session: &Session,
    scene: &DebugScene,
    reported: &ReportedRoles,
    names: &[(String, EntitySelector)],
) -> Vec<FaceName> {
    let query = session.query();
    let mut history: BTreeMap<u64, String> = BTreeMap::new();
    for (step, roles) in reported {
        for (role, ordinal) in roles {
            // A step reports the faces it carried over as well as the
            // ones it made; only the makers name a face, and the first
            // maker wins, so a rim keeps its revolve's name through every
            // later hole.
            if role.contains("preserved") {
                continue;
            }
            let selector = match ordinal {
                Some(ordinal) => {
                    EntitySelector::history_face_ordinal(step.clone(), role.clone(), *ordinal)
                }
                None => EntitySelector::history_face(step.clone(), role.clone()),
            };
            if let Ok(info) = query.entity_info(&selector) {
                let name = match ordinal {
                    Some(ordinal) => format!("{step}.{role}[{ordinal}]"),
                    None => format!("{step}.{role}"),
                };
                history.entry(info.entity_ref.entity.0).or_insert(name);
            }
        }
    }
    let mut script: BTreeMap<u64, String> = BTreeMap::new();
    for (name, selector) in names {
        if let Ok(info) = query.entity_info(selector)
            && info.kind == EntityKind::Face
        {
            script
                .entry(info.entity_ref.entity.0)
                .or_insert_with(|| name.clone());
        }
    }
    let mut faces: Vec<FaceName> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for triangle in &scene.triangles {
        let entity = triangle.source_face;
        if !seen.insert(entity.entity.0) {
            continue;
        }
        // The kernel's own description, off the exact carrier; the facets
        // are only the fallback when the carrier cannot be evaluated.
        let description = NativeKernel::describe_face(&session.snapshot, entity)
            .map_or_else(|_| describe_face(scene, entity), |face| face.summary);
        faces.push(FaceName {
            entity,
            script_name: script.get(&entity.entity.0).cloned(),
            history_name: history.get(&entity.entity.0).cloned().unwrap_or_else(|| {
                // No step claims this face by role: say what it is and
                // where, which is what a person needs to pick it out.
                let kind = description.split(", centre").next().unwrap_or("face");
                format!("{kind} face {}", entity.entity.0)
            }),
            description,
        });
    }
    // Script names first, then history names, each in name order.
    faces.sort_by(|a, b| {
        b.script_name
            .is_some()
            .cmp(&a.script_name.is_some())
            .then_with(|| a.display_name().cmp(b.display_name()))
    });
    faces
}

/// Planar or curved, which way it faces, and where its centre is, read
/// off the face's facets.
fn describe_face(scene: &DebugScene, face: EntityRef) -> String {
    let mut count = 0.0_f64;
    let mut centre = [0.0_f64; 3];
    let mut normal = [0.0_f64; 3];
    let mut first_normal = None::<[f64; 3]>;
    let mut planar = true;
    for triangle in scene.triangles.iter().filter(|t| t.source_face == face) {
        for (vertex, n) in triangle.vertices.iter().zip(triangle.normals.iter()) {
            count += 1.0;
            centre[0] += vertex.x;
            centre[1] += vertex.y;
            centre[2] += vertex.z;
            normal[0] += n.x;
            normal[1] += n.y;
            normal[2] += n.z;
            let this = [n.x, n.y, n.z];
            match first_normal {
                None => first_normal = Some(this),
                Some(first) => {
                    let dot = first[0] * this[0] + first[1] * this[1] + first[2] * this[2];
                    if dot < 1.0 - 1.0e-6 {
                        planar = false;
                    }
                }
            }
        }
    }
    if count == 0.0 {
        return String::new();
    }
    let centre = centre.map(|c| c / count);
    let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
    let facing = if planar && length > 1.0e-9 {
        let unit = normal.map(|n| n / length);
        let axes = [
            ("+X", [1.0, 0.0, 0.0]),
            ("-X", [-1.0, 0.0, 0.0]),
            ("+Y", [0.0, 1.0, 0.0]),
            ("-Y", [0.0, -1.0, 0.0]),
            ("up", [0.0, 0.0, 1.0]),
            ("down", [0.0, 0.0, -1.0]),
        ];
        axes.iter()
            .find(|(_, axis)| unit[0] * axis[0] + unit[1] * axis[1] + unit[2] * axis[2] > 0.999)
            .map_or_else(
                || {
                    format!(
                        "planar, normal ({:.2}, {:.2}, {:.2})",
                        unit[0], unit[1], unit[2]
                    )
                },
                |(word, _)| format!("planar, facing {word}"),
            )
    } else {
        "curved".to_owned()
    };
    format!(
        "{facing}, centre ({:.1}, {:.1}, {:.1})",
        centre[0], centre[1], centre[2]
    )
}

/// The one-based line a step label is declared on, found by its `label:`
/// argument. Commands do not carry source positions, so this is how a step
/// failure points back at the script.
#[must_use]
pub fn line_of_label(source: &str, label: &str) -> Option<usize> {
    let needle = format!("\"{label}\"");
    source
        .lines()
        .position(|line| {
            line.split("//").next().is_some_and(|code| {
                code.contains("label")
                    && code.contains(&needle)
                    && !code.trim_start().starts_with("param")
            })
        })
        .map(|index| index + 1)
}

/// A run in flight on its worker thread.
struct Worker {
    token: CancellationToken,
    generation: u64,
    started: Instant,
}

// ---------------------------------------------------------------------------
// Syntax highlighting
// ---------------------------------------------------------------------------

const KEYWORDS: &[&str] = &["param", "let", "for", "in", "f64", "true", "false"];

/// The builtins the scripting module answers to, highlighted so a typo in a
/// call name shows before the run does.
pub const BUILTINS: &[&str] = &[
    "box",
    "cylinder",
    "sketch",
    "line",
    "circle",
    "arc",
    "rect",
    "extrude",
    "revolve",
    "drill",
    "push_pull",
    "fillet",
    "chamfer",
    "mirror",
    "pattern",
    "union",
    "difference",
    "intersection",
    "faces",
    "edges",
    "nearest",
    "sqrt",
    "abs",
    "floor",
    "ceil",
    "round",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "atan2",
    "pow",
    "hypot",
    "min",
    "max",
    "clamp",
    "pi",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokenKind {
    Comment,
    Keyword,
    Builtin,
    String,
    Number,
    Identifier,
    Punctuation,
    Whitespace,
}

/// Splits a script into coloured runs. Runs are contiguous and cover the
/// whole source, so the galley the editor lays out is the text it edits.
fn highlight_tokens(source: &str) -> Vec<(TokenKind, std::ops::Range<usize>)> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        let byte = bytes[index];
        let kind = if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            TokenKind::Comment
        } else if byte == b'"' {
            index += 1;
            while index < bytes.len() && bytes[index] != b'"' {
                if bytes[index] == b'\\' {
                    index += 1;
                }
                index += 1;
            }
            index = (index + 1).min(bytes.len());
            TokenKind::String
        } else if byte.is_ascii_digit() {
            while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
                index += 1;
            }
            TokenKind::Number
        } else if byte.is_ascii_alphabetic() || byte == b'_' {
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            let word = &source[start..index];
            if KEYWORDS.contains(&word) {
                TokenKind::Keyword
            } else if BUILTINS.contains(&word) {
                TokenKind::Builtin
            } else {
                TokenKind::Identifier
            }
        } else if byte.is_ascii_whitespace() {
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            TokenKind::Whitespace
        } else {
            // Punctuation and anything non-ASCII: one UTF-8 scalar at a time,
            // so a multi-byte character never splits.
            let width = source[start..].chars().next().map_or(1, char::len_utf8);
            index += width;
            TokenKind::Punctuation
        };
        tokens.push((kind, start..index));
    }
    tokens
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let lerp = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

/// Lays a script out in the theme's colours. Cached by the source's hash so
/// a frame that does not edit the text does not re-tokenise it.
#[derive(Default)]
struct Highlighter {
    key: Option<u64>,
    job: Option<LayoutJob>,
}

impl Highlighter {
    fn layout(&mut self, ui: &egui::Ui, source: &str, wrap_width: f32) -> Arc<egui::Galley> {
        let palette = theme::palette();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        source.hash(&mut hasher);
        palette.text.hash(&mut hasher);
        wrap_width.to_bits().hash(&mut hasher);
        let key = hasher.finish();
        if self.key != Some(key) || self.job.is_none() {
            let font = FontId::monospace(13.0);
            let mut job = LayoutJob::default();
            // Never wrap: a wrapped line would put the gutter's numbers out
            // of step with the rows, and the editor scrolls sideways instead.
            job.wrap.max_width = f32::INFINITY;
            let _ = wrap_width;
            for (kind, range) in highlight_tokens(source) {
                let mut format = TextFormat {
                    font_id: font.clone(),
                    color: palette.text,
                    ..Default::default()
                };
                match kind {
                    TokenKind::Comment => {
                        format.color = palette.muted;
                        format.italics = true;
                    }
                    TokenKind::Keyword => format.color = palette.accent,
                    TokenKind::Builtin => format.color = mix(palette.accent, palette.text, 0.45),
                    TokenKind::String => format.color = mix(palette.warn, palette.text, 0.25),
                    TokenKind::Number => format.color = mix(palette.good, palette.text, 0.35),
                    TokenKind::Punctuation => format.color = palette.muted,
                    TokenKind::Identifier | TokenKind::Whitespace => {}
                }
                job.append(&source[range], 0.0, format);
            }
            self.key = Some(key);
            self.job = Some(job);
        }
        let job = self.job.clone().unwrap_or_default();
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    }
}

// ---------------------------------------------------------------------------
// The studio
// ---------------------------------------------------------------------------

/// One `param` row in the customizer: the declaration and the value the
/// user has dragged it to, if any.
#[derive(Clone, Debug, PartialEq)]
pub struct CustomizerRow {
    pub parameter: ScriptParameter,
    pub value: Option<f64>,
}

/// What a path prompt is asking the path for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathPurpose {
    Open,
    SaveAs,
    ExportStl,
    ExportObj,
    ExportJournal,
    ImportJournal,
}

impl PathPurpose {
    const fn title(self) -> &'static str {
        match self {
            Self::Open => "Open script",
            Self::SaveAs => "Save script as",
            Self::ExportStl => "Export STL",
            Self::ExportObj => "Export OBJ",
            Self::ExportJournal => "Export journal",
            Self::ImportJournal => "Pull a journal into the script",
        }
    }

    const fn verb(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::SaveAs => "Save",
            Self::ExportStl | Self::ExportObj | Self::ExportJournal => "Export",
            Self::ImportJournal => "Compare",
        }
    }

    /// The file filter the native dialog offers: what the file is called
    /// and the extensions it answers to.
    const fn filter(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Open | Self::SaveAs => ("Artificer script", &["art"]),
            Self::ExportStl => ("STL mesh", &["stl"]),
            Self::ExportObj => ("Wavefront OBJ", &["obj"]),
            Self::ExportJournal | Self::ImportJournal => ("Session journal", &["json"]),
        }
    }

    /// Whether the dialog picks an existing file or names a new one.
    const fn picks_existing(self) -> bool {
        matches!(self, Self::Open | Self::ImportJournal)
    }
}

/// What the user was about to do when unsaved changes stopped them, so the
/// prompt can carry on with it once they have decided.
#[derive(Clone, Debug, PartialEq, Eq)]
enum DiscardAction {
    /// Close the window.
    CloseWindow,
    /// Put another script in the editor: a new one or an example.
    Replace(String),
    /// Open the script at a known path, dropped or given on the command line.
    Open(PathBuf),
    /// Ask for a script to open, with the native dialog or the typed prompt.
    OpenDialog,
    /// Ask for a path to open by typing it.
    OpenByPath,
}

/// The three answers the unsaved-changes prompt takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnsavedChoice {
    Save,
    Discard,
    Cancel,
}

/// A journal decompiled to a script, waiting for the user to accept it in
/// place of the open script, with what would change.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingImport {
    /// Where the journal came from.
    pub origin: String,
    /// The decompiled script.
    pub script: String,
    /// What replacing the open script with it changes, semantically.
    pub diff: ScriptDiff,
    /// Why the open script could not be compared, when it could not.
    pub note: Option<String>,
}

/// Decompiles a journal and compares it with the open script.
pub fn pull_journal(
    current_source: &str,
    current_path: Option<&Path>,
    journal_json: &str,
    origin: &str,
) -> Result<PendingImport, String> {
    let session = Session::from_journal(journal_json).map_err(|error| error.message)?;
    let script = session
        .to_art(&DecompileOptions::default())
        .map_err(|error| error.message)?;
    let modules: Box<dyn ModuleResolver> = match current_path {
        Some(path) => Box::new(FileModules::beside(path)),
        None => Box::new(NoModules),
    };
    let incoming = compile_program_with(&script, &BTreeMap::new(), &NoModules)
        .map_err(|error| format!("The decompiled script does not compile: {error}"))?;
    let (diff, note) =
        match compile_program_with(current_source, &BTreeMap::new(), modules.as_ref()) {
            Ok(current) => (ScriptDiff::between(&current, &incoming), None),
            Err(error) => (
                ScriptDiff::default(),
                Some(format!(
                    "The open script does not compile, so no comparison is possible: {error}"
                )),
            ),
        };
    Ok(PendingImport {
        origin: origin.to_owned(),
        script,
        diff,
        note,
    })
}

struct PathPrompt {
    purpose: PathPurpose,
    text: String,
}

/// The application state: one script, its last run, and the view of it.
pub struct ScriptStudio {
    source: String,
    saved_source: String,
    path: Option<PathBuf>,
    auto_run: bool,
    customizer: Vec<CustomizerRow>,
    customizer_error: Option<String>,
    generation: u64,
    worker: Option<Worker>,
    outcome: Option<RunOutcome>,
    sender: Sender<RunOutcome>,
    receiver: Receiver<RunOutcome>,
    last_edit: Option<Instant>,
    run_requested: bool,
    view: ViewState,
    transform: DisplayTransform,
    drag: FeaturePreviewDragState,
    edge_frame_memo: Option<EdgeFrameMemo>,
    selected_face: Option<DocumentFaceSelection>,
    framed_bounds: Option<Aabb3>,
    highlighter: Highlighter,
    status: Option<String>,
    prompt: Option<PathPrompt>,
    pending_import: Option<PendingImport>,
    jump_to_line: Option<usize>,
    show_customizer: bool,
    theme_choice: WorkbenchTheme,
    display_mode: ModelDisplayMode,
    section: SectionPlane,
    /// The 3D mouse, when the process has one; `None` in tests, which never
    /// open it.
    spacemouse: Option<SpaceMouse>,
    /// The puck's motion, shaped and smoothed before it steers.
    spacemouse_filter: SixDofFilter,
    /// Whether Open, Save as and the exports use the desktop's own file
    /// dialog. Off, they fall back to the typed-path prompt, which is also
    /// what a headless test drives.
    native_file_dialogs: bool,
    /// The folder the last dialog ended in, so the next one opens there.
    last_dialog_directory: Option<PathBuf>,
    /// The action waiting on the unsaved-changes prompt, while it shows.
    discard_prompt: Option<DiscardAction>,
    /// The action to carry on with once a save the prompt asked for lands.
    resume_after_save: Option<DiscardAction>,
    /// The user has decided the window may close: the next close request
    /// goes through rather than back to the prompt.
    close_confirmed: bool,
    /// Ask the window to close on the next frame.
    close_window: bool,
}

/// The world axis a section plane is normal to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SectionAxis {
    X,
    #[default]
    Y,
    Z,
}

impl SectionAxis {
    pub const ALL: [Self; 3] = [Self::X, Self::Y, Self::Z];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::X => "YZ",
            Self::Y => "XZ",
            Self::Z => "XY",
        }
    }

    const fn normal(self) -> Vector3 {
        match self {
            Self::X => Vector3::new(1.0, 0.0, 0.0),
            Self::Y => Vector3::new(0.0, 1.0, 0.0),
            Self::Z => Vector3::new(0.0, 0.0, 1.0),
        }
    }
}

/// The section analysis plane: the model is clipped to one side of it and
/// the cut faces are capped, so the inside of a part can be inspected.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SectionPlane {
    pub active: bool,
    pub axis: SectionAxis,
    /// Where the plane sits along its axis.
    pub offset: f64,
    /// Keep the other side instead.
    pub flipped: bool,
}

impl SectionPlane {
    /// The renderer's clipping plane, or `None` while the section is off.
    /// The kept side satisfies `normal · p + offset >= 0`.
    #[must_use]
    pub fn cut_plane(self) -> Option<SectionCutPlane> {
        if !self.active {
            return None;
        }
        let sign = if self.flipped { -1.0 } else { 1.0 };
        let normal = self.axis.normal();
        Some(SectionCutPlane::new(
            Vector3::new(normal.x * sign, normal.y * sign, normal.z * sign),
            -sign * self.offset,
        ))
    }
}

impl ScriptStudio {
    /// Opens the studio on `script`, or on the welcome script when there is
    /// none or it cannot be read.
    pub fn new(creation_context: &eframe::CreationContext<'_>, script: Option<PathBuf>) -> Self {
        let mut studio = Self::with_source(creation_context, WELCOME_SCRIPT);
        // The 3D mouse wakes the window when the cap moves, so the viewport
        // follows it without polling.
        let wake_context = creation_context.egui_ctx.clone();
        studio.spacemouse = SpaceMouse::open_with_wake(move || wake_context.request_repaint());
        if let Some(path) = script {
            studio.open_path(&path);
        }
        studio
    }

    /// Opens the studio on a script held in memory, with no file behind it.
    pub fn with_source(creation_context: &eframe::CreationContext<'_>, source: &str) -> Self {
        theme::install_style(&creation_context.egui_ctx);
        let (sender, receiver) = channel();
        let mut studio = Self {
            source: source.to_owned(),
            saved_source: source.to_owned(),
            path: None,
            auto_run: true,
            customizer: Vec::new(),
            customizer_error: None,
            generation: 0,
            worker: None,
            outcome: None,
            sender,
            receiver,
            last_edit: None,
            run_requested: true,
            view: ViewState::default(),
            transform: DisplayTransform::default(),
            drag: FeaturePreviewDragState::default(),
            edge_frame_memo: None,
            selected_face: None,
            framed_bounds: None,
            highlighter: Highlighter::default(),
            status: None,
            prompt: None,
            pending_import: None,
            jump_to_line: None,
            show_customizer: true,
            theme_choice: theme::active_theme(),
            display_mode: ModelDisplayMode::ShadedEdges,
            section: SectionPlane::default(),
            spacemouse: None,
            spacemouse_filter: SixDofFilter::default(),
            native_file_dialogs: true,
            last_dialog_directory: None,
            discard_prompt: None,
            resume_after_save: None,
            close_confirmed: false,
            close_window: false,
        };
        studio.refresh_customizer();
        studio
    }

    /// Whether the studio opens the desktop's file dialogs. A headless
    /// test turns them off and drives the typed-path prompt instead.
    pub const fn set_native_file_dialogs(&mut self, enabled: bool) {
        self.native_file_dialogs = enabled;
    }

    /// Whether the unsaved-changes prompt is showing.
    #[must_use]
    pub const fn unsaved_prompt_open(&self) -> bool {
        self.discard_prompt.is_some()
    }

    /// Whether the user has answered the window's close: from then on a
    /// close request goes through rather than back to the prompt.
    #[must_use]
    pub const fn window_close_confirmed(&self) -> bool {
        self.close_confirmed
    }

    // -- state the tests read -------------------------------------------

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Replaces the script text as an edit would, re-running it.
    pub fn set_source(&mut self, source: &str) {
        self.source = source.to_owned();
        self.note_edit();
    }

    #[must_use]
    pub fn last_outcome(&self) -> Option<&RunOutcome> {
        self.outcome.as_ref()
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.worker.is_some()
    }

    #[must_use]
    pub fn customizer_rows(&self) -> &[CustomizerRow] {
        &self.customizer
    }

    /// Drags one customizer value, as the slider would.
    pub fn set_parameter(&mut self, name: &str, value: f64) {
        if let Some(row) = self
            .customizer
            .iter_mut()
            .find(|row| row.parameter.name == name)
        {
            row.value = Some(value);
            self.run_requested = true;
        }
    }

    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The camera, for tests and for hosts that place the view.
    pub fn view_mut(&mut self) -> &mut ViewState {
        &mut self.view
    }

    #[must_use]
    pub const fn section(&self) -> SectionPlane {
        self.section
    }

    /// Sets the section plane, as the View menu and the section panel do.
    pub fn set_section(&mut self, section: SectionPlane) {
        self.section = section;
    }

    #[must_use]
    pub const fn display_mode(&self) -> ModelDisplayMode {
        self.display_mode
    }

    pub fn set_display_mode(&mut self, mode: ModelDisplayMode) {
        self.display_mode = mode;
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.source != self.saved_source
    }

    // -- editing and running --------------------------------------------

    fn note_edit(&mut self) {
        self.last_edit = Some(Instant::now());
        self.refresh_customizer();
    }

    /// Re-reads the `param` lines, keeping the values the user has set on
    /// parameters that still exist.
    fn refresh_customizer(&mut self) {
        match script_parameters(&self.source) {
            Ok(parameters) => {
                let previous: BTreeMap<String, Option<f64>> = self
                    .customizer
                    .drain(..)
                    .map(|row| (row.parameter.name, row.value))
                    .collect();
                self.customizer = parameters
                    .into_iter()
                    .map(|parameter| {
                        let value = previous.get(&parameter.name).copied().flatten();
                        CustomizerRow { parameter, value }
                    })
                    .collect();
                self.customizer_error = None;
            }
            Err(error) => {
                // The rows stay as they were: a half-typed line should not
                // empty the customizer under the user's pointer.
                self.customizer_error = Some(error.to_string());
            }
        }
    }

    fn overrides(&self) -> BTreeMap<String, f64> {
        self.customizer
            .iter()
            .filter_map(|row| row.value.map(|value| (row.parameter.name.clone(), value)))
            .collect()
    }

    fn start_run(&mut self, ctx: &egui::Context) {
        if let Some(worker) = self.worker.take() {
            worker.token.cancel();
        }
        self.generation += 1;
        self.run_requested = false;
        self.last_edit = None;
        let generation = self.generation;
        let token = CancellationToken::new();
        let worker_token = token.clone();
        let source = self.source.clone();
        let overrides = self.overrides();
        let path = self.path.clone();
        let sender = self.sender.clone();
        let repaint = ctx.clone();
        std::thread::Builder::new()
            .name(format!("art-run-{generation}"))
            .spawn(move || {
                let outcome = run_script_generation(
                    generation,
                    &source,
                    &overrides,
                    path.as_deref(),
                    &worker_token,
                );
                // The receiver is gone only when the window has closed.
                let _ = sender.send(outcome);
                repaint.request_repaint();
            })
            .expect("spawn the script worker thread");
        self.worker = Some(Worker {
            token,
            generation,
            started: Instant::now(),
        });
    }

    fn poll_worker(&mut self) {
        while let Ok(outcome) = self.receiver.try_recv() {
            let current = self.worker.as_ref().map(|worker| worker.generation);
            if Some(outcome.generation) != current {
                continue;
            }
            self.worker = None;
            if outcome.cancelled {
                continue;
            }
            self.adopt_outcome(outcome);
        }
    }

    fn adopt_outcome(&mut self, mut outcome: RunOutcome) {
        // A run that failed before building anything, a parse error while
        // typing above all, keeps the last good model on screen and
        // exportable; only a run that built something replaces it.
        if outcome.scene.is_none()
            && outcome.error.is_some()
            && let Some(previous) = self.outcome.take()
        {
            outcome.scene = previous.scene;
            outcome.snapshot = previous.snapshot;
        }
        let bounds = outcome
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.measures().bounds);
        if let Some(bounds) = bounds {
            match self.framed_bounds {
                None => self.view.frame(bounds),
                Some(_) => self.view.widen_to_include(bounds),
            }
            self.framed_bounds = Some(bounds);
        }
        if outcome.scene.is_none() {
            self.selected_face = None;
        }
        self.outcome = Some(outcome);
    }

    fn tick(&mut self, ctx: &egui::Context) {
        self.poll_worker();
        if self.run_requested {
            self.start_run(ctx);
        } else if self.auto_run
            && let Some(edited) = self.last_edit
        {
            let elapsed = edited.elapsed();
            if elapsed >= AUTO_RUN_DEBOUNCE {
                self.start_run(ctx);
            } else {
                ctx.request_repaint_after(AUTO_RUN_DEBOUNCE - elapsed);
            }
        }
        if self.worker.is_some() {
            ctx.request_repaint_after(Duration::from_millis(40));
        }
    }

    // -- files ------------------------------------------------------------

    fn open_path(&mut self, path: &Path) {
        match std::fs::read_to_string(path) {
            Ok(source) => {
                self.source = source.clone();
                self.saved_source = source;
                self.path = Some(path.to_path_buf());
                self.remember_dialog_directory(path);
                self.framed_bounds = None;
                self.customizer.clear();
                self.refresh_customizer();
                self.run_requested = true;
                self.status = Some(format!("Opened {}", path.display()));
            }
            Err(error) => {
                self.status = Some(format!("Could not open {}: {error}", path.display()));
            }
        }
    }

    fn save_to(&mut self, path: &Path) {
        match std::fs::write(path, &self.source) {
            Ok(()) => {
                self.saved_source.clone_from(&self.source);
                self.path = Some(path.to_path_buf());
                self.remember_dialog_directory(path);
                self.status = Some(format!("Saved {}", path.display()));
                // The save was the answer to the unsaved-changes prompt:
                // now the thing the user was doing can go ahead.
                if let Some(action) = self.resume_after_save.take() {
                    self.perform(action);
                }
            }
            Err(error) => {
                self.resume_after_save = None;
                self.status = Some(format!("Could not save {}: {error}", path.display()));
            }
        }
    }

    fn save(&mut self) {
        match self.path.clone() {
            Some(path) => self.save_to(&path),
            None => self.request_path(PathPurpose::SaveAs),
        }
    }

    /// Where a file dialog opens: beside the open script, else where the
    /// last dialog ended, else the home directory.
    fn dialog_directory(&self) -> Option<PathBuf> {
        self.path
            .as_ref()
            .and_then(|path| path.parent())
            .filter(|parent| parent.is_dir())
            .map(Path::to_path_buf)
            .or_else(|| self.last_dialog_directory.clone())
            .or_else(|| {
                ["HOME", "USERPROFILE"]
                    .into_iter()
                    .find_map(|variable| {
                        std::env::var_os(variable).filter(|value| !value.is_empty())
                    })
                    .map(PathBuf::from)
            })
    }

    fn remember_dialog_directory(&mut self, path: &Path) {
        if let Some(parent) = path.parent().filter(|parent| parent.is_dir()) {
            self.last_dialog_directory = Some(parent.to_path_buf());
        }
    }

    /// The desktop's own file dialog for `purpose`, blocking until the user
    /// has chosen or cancelled. `None` is a cancellation, or a desktop with
    /// no dialog to offer; the typed prompt is always there for the latter.
    fn choose_path_natively(&mut self, purpose: PathPurpose) -> Option<PathBuf> {
        let (name, extensions) = purpose.filter();
        let mut dialog = rfd::FileDialog::new()
            .set_title(purpose.title())
            .add_filter(name, extensions);
        if let Some(directory) = self.dialog_directory() {
            dialog = dialog.set_directory(directory);
        }
        let chosen = if purpose.picks_existing() {
            dialog.pick_file()
        } else {
            let suggested = self.suggested_file_name(purpose);
            dialog.set_file_name(suggested).save_file()
        }?;
        self.remember_dialog_directory(&chosen);
        Some(chosen)
    }

    /// The file name a save or export proposes.
    fn suggested_file_name(&self, purpose: PathPurpose) -> String {
        match purpose {
            PathPurpose::Open | PathPurpose::SaveAs => self
                .path
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "model.art".to_owned()),
            PathPurpose::ExportStl => format!("{}.stl", self.model_name()),
            PathPurpose::ExportObj => format!("{}.obj", self.model_name()),
            PathPurpose::ExportJournal | PathPurpose::ImportJournal => {
                format!("{}.journal.json", self.model_name())
            }
        }
    }

    /// Asks for a path the way the desktop does it, and acts on the answer;
    /// or falls back to the typed prompt where there is no native dialog.
    fn request_path(&mut self, purpose: PathPurpose) {
        if !self.native_file_dialogs {
            self.open_prompt(purpose);
            return;
        }
        match self.choose_path_natively(purpose) {
            Some(path) => self.act_on_path(&path, purpose),
            None => self.resume_after_save = None,
        }
    }

    /// Does what `purpose` asks with a path, however the path was chosen.
    fn act_on_path(&mut self, path: &Path, purpose: PathPurpose) {
        match purpose {
            PathPurpose::Open => self.open_path(path),
            PathPurpose::SaveAs => self.save_to(path),
            PathPurpose::ExportStl | PathPurpose::ExportObj | PathPurpose::ExportJournal => {
                self.export_to(path, purpose);
            }
            PathPurpose::ImportJournal => self.import_journal(path),
        }
    }

    // -- never losing unsaved work ----------------------------------------

    /// Runs `action` now if nothing would be lost, otherwise asks first.
    fn guard_unsaved(&mut self, action: DiscardAction) {
        if self.is_dirty() {
            self.discard_prompt = Some(action);
        } else {
            self.perform(action);
        }
    }

    /// Carries out an action the unsaved-changes prompt was guarding.
    fn perform(&mut self, action: DiscardAction) {
        match action {
            DiscardAction::CloseWindow => {
                self.close_confirmed = true;
                self.close_window = true;
            }
            DiscardAction::Replace(source) => self.load_example(&source),
            DiscardAction::Open(path) => self.open_path(&path),
            DiscardAction::OpenDialog => self.request_path(PathPurpose::Open),
            DiscardAction::OpenByPath => self.open_prompt(PathPurpose::Open),
        }
    }

    /// Answers the prompt with Save: the action goes ahead once the save
    /// has landed, and not at all if it does not.
    fn save_then(&mut self, action: DiscardAction) {
        self.resume_after_save = Some(action);
        self.save();
    }

    /// The modal that stands between an unsaved script and anything that
    /// would replace it.
    fn unsaved_changes_window(&mut self, ctx: &egui::Context) {
        let Some(action) = self.discard_prompt.clone() else {
            return;
        };
        let title = self.document_title();
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::new("unsaved-script")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(
                RichText::new(format!("Save changes to {title}?"))
                    .font(FontId::proportional(14.0))
                    .color(theme::text())
                    .strong(),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(match &action {
                    DiscardAction::CloseWindow => {
                        "The script has unsaved changes. Closing the window without saving loses them."
                    }
                    _ => "The script has unsaved changes. Replacing it without saving loses them.",
                })
                .color(theme::muted()),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new(RichText::new("Save").strong()))
                    .clicked()
                {
                    choice = Some(UnsavedChoice::Save);
                }
                if ui.button("Don't save").clicked() {
                    choice = Some(UnsavedChoice::Discard);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(UnsavedChoice::Cancel);
                }
            });
        });
        if modal.should_close() {
            choice = Some(UnsavedChoice::Cancel);
        }
        match choice {
            Some(UnsavedChoice::Save) => {
                self.discard_prompt = None;
                self.save_then(action);
            }
            Some(UnsavedChoice::Discard) => {
                self.discard_prompt = None;
                self.perform(action);
            }
            Some(UnsavedChoice::Cancel) => self.discard_prompt = None,
            None => {}
        }
    }

    /// The name the header shows for the script.
    fn document_title(&self) -> String {
        match &self.path {
            Some(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            None => "Untitled.art".to_owned(),
        }
    }

    fn export_to(&mut self, path: &Path, purpose: PathPurpose) {
        let Some(snapshot) = self.outcome.as_ref().and_then(|o| o.snapshot.as_ref()) else {
            self.status = Some("Nothing to export: the script has not built a body".to_owned());
            return;
        };
        let name = self.model_name();
        let written = match purpose {
            PathPurpose::ExportStl => {
                export_stl_binary(snapshot).map(|bytes| std::fs::write(path, bytes))
            }
            PathPurpose::ExportObj => {
                export_obj(snapshot, &name).map(|text| std::fs::write(path, text))
            }
            PathPurpose::ExportJournal => {
                let journal = self
                    .outcome
                    .as_ref()
                    .and_then(|outcome| outcome.journal.clone())
                    .unwrap_or_default();
                Ok(std::fs::write(path, journal))
            }
            PathPurpose::Open | PathPurpose::SaveAs | PathPurpose::ImportJournal => return,
        };
        self.status = Some(match written {
            Ok(Ok(())) => format!("Exported {}", path.display()),
            Ok(Err(error)) => format!("Could not write {}: {error}", path.display()),
            Err(error) => format!("Export failed: {}", error.message),
        });
    }

    fn model_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|path| path.file_stem())
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "model".to_owned())
    }

    fn open_prompt(&mut self, purpose: PathPurpose) {
        let suggestion = match purpose {
            PathPurpose::Open => self
                .path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            PathPurpose::SaveAs => self
                .path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "model.art".to_owned()),
            PathPurpose::ExportStl => format!("{}.stl", self.model_name()),
            PathPurpose::ExportObj => format!("{}.obj", self.model_name()),
            PathPurpose::ExportJournal | PathPurpose::ImportJournal => {
                format!("{}.journal.json", self.model_name())
            }
        };
        self.prompt = Some(PathPrompt {
            purpose,
            text: suggestion,
        });
    }

    /// Reads a journal, decompiles it, and holds the result for the user
    /// to compare with the open script and accept or discard.
    fn import_journal(&mut self, path: &Path) {
        let json = match std::fs::read_to_string(path) {
            Ok(json) => json,
            Err(error) => {
                self.status = Some(format!("Could not read {}: {error}", path.display()));
                return;
            }
        };
        match pull_journal(
            &self.source,
            self.path.as_deref(),
            &json,
            &path.display().to_string(),
        ) {
            Ok(pending) => {
                self.status = Some(format!(
                    "Decompiled {}: {} change{}",
                    path.display(),
                    pending.diff.entries.len(),
                    if pending.diff.entries.len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                ));
                self.pending_import = Some(pending);
            }
            Err(error) => {
                self.status = Some(format!("Could not pull {}: {error}", path.display()));
            }
        }
    }

    /// The journal import awaiting a decision, if any.
    #[must_use]
    pub fn pending_import(&self) -> Option<&PendingImport> {
        self.pending_import.as_ref()
    }

    /// Replaces the open script with the pending decompiled one.
    pub fn accept_import(&mut self) {
        if let Some(pending) = self.pending_import.take() {
            self.source = pending.script;
            self.framed_bounds = None;
            self.customizer.clear();
            self.refresh_customizer();
            self.run_requested = true;
            self.status = Some(format!("Pulled {} into the script", pending.origin));
        }
    }

    pub fn discard_import(&mut self) {
        self.pending_import = None;
    }

    fn import_window(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_import.as_ref() else {
            return;
        };
        let mut accepted = false;
        let mut discarded = false;
        egui::Window::new("Pull journal into script")
            .collapsible(false)
            .resizable(true)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(520.0);
                ui.label(
                    RichText::new(format!("From {}", pending.origin))
                        .color(theme::muted())
                        .small(),
                );
                ui.add_space(4.0);
                if let Some(note) = &pending.note {
                    ui.label(RichText::new(note).color(theme::warn()).small());
                } else if pending.diff.is_empty() {
                    ui.label(
                        RichText::new("The journal builds exactly what the open script builds.")
                            .color(theme::muted()),
                    );
                } else {
                    ui.label(
                        RichText::new(format!(
                            "Replacing the open script changes {} thing{}:",
                            pending.diff.entries.len(),
                            if pending.diff.entries.len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        ))
                        .color(theme::text()),
                    );
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .id_salt("import-diff")
                        .show(ui, |ui| {
                            for line in pending.diff.lines() {
                                ui.label(RichText::new(format!("  {line}")).monospace());
                            }
                        });
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new(RichText::new("Replace script").strong()))
                        .clicked()
                    {
                        accepted = true;
                    }
                    if ui.button("Keep mine").clicked() {
                        discarded = true;
                    }
                });
            });
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            discarded = true;
        }
        if accepted {
            self.accept_import();
        } else if discarded {
            self.discard_import();
        }
    }

    fn accept_prompt(&mut self, prompt: PathPrompt) {
        let path = PathBuf::from(prompt.text.trim());
        if path.as_os_str().is_empty() {
            self.status = Some("A path is needed".to_owned());
            return;
        }
        self.act_on_path(&path, prompt.purpose);
    }

    fn load_example(&mut self, source: &str) {
        self.source = source.to_owned();
        self.saved_source = source.to_owned();
        self.path = None;
        self.framed_bounds = None;
        self.customizer.clear();
        self.refresh_customizer();
        self.run_requested = true;
        self.status = None;
    }

    fn set_theme(&mut self, ctx: &egui::Context, choice: WorkbenchTheme) {
        self.theme_choice = choice;
        theme::set_active_theme(choice);
        theme::install_style(ctx);
    }

    // -- panels -----------------------------------------------------------

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (run, save_as, save, open, fit) = ctx.input_mut(|input| {
            let run = input.consume_key(egui::Modifiers::NONE, egui::Key::F5);
            let save_as = input.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::S,
            );
            let save = input.consume_key(egui::Modifiers::COMMAND, egui::Key::S);
            let open = input.consume_key(egui::Modifiers::COMMAND, egui::Key::O);
            // F only when nothing (the editor, a field) has the keyboard.
            let fit = input.key_pressed(egui::Key::F);
            (run, save_as, save, open, fit)
        });
        if run {
            self.run_requested = true;
        }
        if save_as {
            self.request_path(PathPurpose::SaveAs);
        } else if save {
            self.save();
        }
        if open {
            self.guard_unsaved(DiscardAction::OpenDialog);
        }
        if fit && ctx.memory(|memory| memory.focused().is_none()) {
            self.fit_view();
        }
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        if let Some(path) = dropped.into_iter().next() {
            self.guard_unsaved(DiscardAction::Open(path));
        }
    }

    fn fit_view(&mut self) {
        if let Some(bounds) = self.framed_bounds {
            self.view.frame(bounds);
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            let mark = ui.label(
                RichText::new("ARTIFICER")
                    .font(FontId::proportional(10.0))
                    .color(theme::accent())
                    .strong(),
            );
            mark.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Label, true, "Artificer Script Studio")
            });
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);
            let title = self.document_title();
            let title = if self.is_dirty() {
                format!("{title} •")
            } else {
                title
            };
            ui.label(
                RichText::new(title)
                    .font(FontId::proportional(14.0))
                    .color(theme::text())
                    .strong(),
            );
            ui.add_space(6.0);

            ui.menu_button("File", |ui| {
                if ui.button("New script").clicked() {
                    self.guard_unsaved(DiscardAction::Replace(NEW_SCRIPT.to_owned()));
                    ui.close();
                }
                if ui.button("Open…    Ctrl+O").clicked() {
                    self.guard_unsaved(DiscardAction::OpenDialog);
                    ui.close();
                }
                if ui.button("Save    Ctrl+S").clicked() {
                    self.save();
                    ui.close();
                }
                if ui.button("Save as…    Ctrl+Shift+S").clicked() {
                    self.request_path(PathPurpose::SaveAs);
                    ui.close();
                }
                ui.separator();
                let exportable = self
                    .outcome
                    .as_ref()
                    .is_some_and(|outcome| outcome.snapshot.is_some());
                if ui
                    .add_enabled(exportable, egui::Button::new("Export STL…"))
                    .clicked()
                {
                    self.request_path(PathPurpose::ExportStl);
                    ui.close();
                }
                if ui
                    .add_enabled(exportable, egui::Button::new("Export OBJ…"))
                    .clicked()
                {
                    self.request_path(PathPurpose::ExportObj);
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(exportable, egui::Button::new("Export journal…"))
                    .on_hover_text("The session journal as JSON, for the JSON-RPC server, the command line, or another Script Studio")
                    .clicked()
                {
                    self.request_path(PathPurpose::ExportJournal);
                    ui.close();
                }
                if ui
                    .button("Pull journal into script…")
                    .on_hover_text("Decompile a journal to a script and see what it changes before it replaces the open one")
                    .clicked()
                {
                    self.request_path(PathPurpose::ImportJournal);
                    ui.close();
                }
                ui.separator();
                // Some desktops have no file dialog to offer, and a path is
                // sometimes quicker to type than to browse to: every action
                // above is also reachable with a typed path.
                ui.menu_button("By typed path", |ui| {
                    ui.set_min_width(200.0);
                    if ui.button("Open…").clicked() {
                        self.guard_unsaved(DiscardAction::OpenByPath);
                        ui.close();
                    }
                    if ui.button("Save as…").clicked() {
                        self.open_prompt(PathPurpose::SaveAs);
                        ui.close();
                    }
                    if ui
                        .add_enabled(exportable, egui::Button::new("Export STL…"))
                        .clicked()
                    {
                        self.open_prompt(PathPurpose::ExportStl);
                        ui.close();
                    }
                    if ui
                        .add_enabled(exportable, egui::Button::new("Export OBJ…"))
                        .clicked()
                    {
                        self.open_prompt(PathPurpose::ExportObj);
                        ui.close();
                    }
                    if ui
                        .add_enabled(exportable, egui::Button::new("Export journal…"))
                        .clicked()
                    {
                        self.open_prompt(PathPurpose::ExportJournal);
                        ui.close();
                    }
                    if ui.button("Pull journal into script…").clicked() {
                        self.open_prompt(PathPurpose::ImportJournal);
                        ui.close();
                    }
                });
            });
            ui.menu_button("Examples", |ui| {
                for (name, source) in EXAMPLES {
                    if ui.button(*name).clicked() {
                        self.guard_unsaved(DiscardAction::Replace((*source).to_owned()));
                        ui.close();
                    }
                }
            });
            ui.menu_button("View", |ui| {
                if ui.button("Fit model    F").clicked() {
                    self.fit_view();
                    ui.close();
                }
                if ui.button("Reset orientation").clicked() {
                    self.view.reset_orientation();
                    ui.close();
                }
                ui.separator();
                for (mode, label) in [
                    (ModelDisplayMode::ShadedEdges, "Shaded with edges"),
                    (ModelDisplayMode::HiddenLinesRemoved, "Hidden lines removed"),
                    (ModelDisplayMode::Wireframe, "Wireframe"),
                ] {
                    if ui.radio(self.display_mode == mode, label).clicked() {
                        self.display_mode = mode;
                        ui.close();
                    }
                }
                ui.separator();
                let section = ui
                    .checkbox(&mut self.section.active, "Section analysis")
                    .on_hover_text("Clip the model to one side of a plane and cap the cut");
                section.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Checkbox, true, "Section analysis")
                });
                ui.checkbox(&mut self.show_customizer, "Customizer");
                ui.separator();
                for choice in WorkbenchTheme::ALL {
                    if ui
                        .radio(self.theme_choice == choice, choice.label())
                        .clicked()
                    {
                        let ctx = ui.ctx().clone();
                        self.set_theme(&ctx, choice);
                        ui.close();
                    }
                }
            });

            ui.add_space(8.0);
            let run = ui
                .add(egui::Button::new(RichText::new("▶ Run").strong()))
                .on_hover_text("Run the script now (F5)");
            run.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Run script")
            });
            if run.clicked() {
                self.run_requested = true;
            }
            let auto = ui
                .checkbox(&mut self.auto_run, "Auto")
                .on_hover_text("Re-run the script as you type");
            auto.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Checkbox, true, "Auto-run")
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.run_status(ui);
            });
        });
    }

    fn run_status(&self, ui: &mut egui::Ui) {
        if let Some(worker) = &self.worker {
            ui.add(egui::Spinner::new().size(12.0));
            let seconds = worker.started.elapsed().as_secs_f64();
            ui.label(
                RichText::new(format!("Running… {seconds:.1} s"))
                    .color(theme::muted())
                    .small(),
            );
            return;
        }
        let Some(outcome) = &self.outcome else {
            return;
        };
        let (text, colour) = match &outcome.error {
            Some(error) => (
                match error.location {
                    Some((line, _)) => format!("✕ {} on line {line}", error.kind),
                    None => format!("✕ {}", error.kind),
                },
                theme::bad(),
            ),
            None => (
                format!(
                    "● {} steps in {} ms",
                    outcome.steps.len(),
                    outcome.elapsed.as_millis()
                ),
                theme::good(),
            ),
        };
        let status = ui.label(RichText::new(text.clone()).color(colour).small());
        status.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("Run status: {text}"))
        });
        if let Some(message) = &self.status {
            ui.separator();
            ui.label(RichText::new(message).color(theme::muted()).small());
        }
    }

    fn editor(&mut self, ui: &mut egui::Ui) {
        let error_line = self
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.error.as_ref())
            .and_then(|error| error.location)
            .map(|(line, _)| line);
        let editor_id = egui::Id::new(EDITOR_ID);
        if let Some(line) = self.jump_to_line.take() {
            let index = self
                .source
                .lines()
                .take(line.saturating_sub(1))
                .map(|text| text.chars().count() + 1)
                .sum::<usize>();
            let mut state = egui::TextEdit::load_state(ui.ctx(), editor_id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(CCursorRange::one(CCursor::new(index))));
            egui::TextEdit::store_state(ui.ctx(), editor_id, state);
            ui.ctx()
                .memory_mut(|memory| memory.request_focus(editor_id));
        }

        let line_count =
            self.source.lines().count().max(1) + usize::from(self.source.ends_with('\n'));
        let changed = egui::ScrollArea::both()
            .id_salt("script-editor-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    // The gutter: line numbers on the same row pitch as the
                    // editor, the error's line washed in the failure colour.
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.add_space(2.0);
                        let digits = line_count.to_string().len().max(3);
                        for number in 1..=line_count {
                            let is_error = error_line == Some(number);
                            let text = RichText::new(format!("{number:>digits$}"))
                                .font(FontId::monospace(13.0))
                                .color(if is_error {
                                    theme::bad()
                                } else {
                                    theme::muted()
                                });
                            let response = ui.add(egui::Label::new(text).selectable(false));
                            if is_error {
                                ui.painter().rect_filled(
                                    response.rect.expand2(egui::vec2(4.0, 0.0)),
                                    2.0,
                                    theme::bad().gamma_multiply(0.18),
                                );
                            }
                        }
                    });
                    ui.add_space(4.0);
                    let highlighter = &mut self.highlighter;
                    let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap: f32| {
                        highlighter.layout(ui, buffer.as_str(), wrap)
                    };
                    let output = egui::TextEdit::multiline(&mut self.source)
                        .id(editor_id)
                        .code_editor()
                        .font(FontId::monospace(13.0))
                        .desired_width(f32::INFINITY)
                        .desired_rows(40)
                        .lock_focus(true)
                        .layouter(&mut layouter)
                        .show(ui);
                    output.response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Script")
                    });
                    output.response.changed()
                })
                .inner
            })
            .inner;
        if changed {
            self.note_edit();
        }
    }

    fn section_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(
            RichText::new("SECTION")
                .font(FontId::proportional(10.0))
                .color(theme::muted())
                .strong(),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for axis in SectionAxis::ALL {
                let choice = ui
                    .selectable_label(self.section.axis == axis, axis.label())
                    .on_hover_text(format!("Cut parallel to the {} plane", axis.label()));
                if choice.clicked() {
                    self.section.axis = axis;
                }
            }
        });
        ui.horizontal(|ui| {
            let extent = self.framed_bounds.map_or(50.0, |bounds| {
                let size = match self.section.axis {
                    SectionAxis::X => bounds.max.x - bounds.min.x,
                    SectionAxis::Y => bounds.max.y - bounds.min.y,
                    SectionAxis::Z => bounds.max.z - bounds.min.z,
                };
                size.abs().max(1.0)
            });
            let offset = ui
                .add(
                    egui::DragValue::new(&mut self.section.offset)
                        .speed(extent * 0.005)
                        .max_decimals(3),
                )
                .on_hover_text("Where the plane sits along its axis");
            offset.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::DragValue, true, "Section offset")
            });
            if ui
                .button("Flip")
                .on_hover_text("Keep the other side of the plane")
                .clicked()
            {
                self.section.flipped = !self.section.flipped;
            }
            if ui
                .button("Off")
                .on_hover_text("Show the whole model again")
                .clicked()
            {
                self.section.active = false;
            }
        });
        ui.add_space(4.0);
        ui.separator();
    }

    /// The faces the script named, and every other face by the step that
    /// made it. Clicking one selects it in the viewport, so a person can
    /// match a name to a face before asking for a change to it.
    fn faces_panel(&mut self, ui: &mut egui::Ui) {
        let Some(outcome) = &self.outcome else {
            return;
        };
        if outcome.faces.is_empty() {
            return;
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new("FACES")
                .font(FontId::proportional(10.0))
                .color(theme::muted())
                .strong(),
        );
        ui.add_space(4.0);
        let mut pick = None;
        for face in &outcome.faces {
            let selected = self
                .selected_face
                .is_some_and(|selection| selection.face == face.entity);
            let colour = if face.script_name.is_some() {
                theme::accent()
            } else {
                theme::text()
            };
            let row = ui
                .selectable_label(
                    selected,
                    RichText::new(face.display_name()).color(colour).small(),
                )
                .on_hover_text(if face.script_name.is_some() {
                    format!("{}\nalso {}", face.description, face.history_name)
                } else {
                    face.description.clone()
                });
            row.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    format!("Face {}", face.display_name()),
                )
            });
            if row.clicked() {
                pick = Some(face.entity);
            }
        }
        if let Some(entity) = pick {
            self.selected_face = Some(DocumentFaceSelection {
                body: BODY,
                face: entity,
            });
        }
        ui.add_space(4.0);
        ui.separator();
    }

    fn customizer_panel(&mut self, ui: &mut egui::Ui) {
        if self.section.active {
            self.section_panel(ui);
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new("CUSTOMIZER")
                .font(FontId::proportional(10.0))
                .color(theme::muted())
                .strong(),
        );
        ui.add_space(4.0);
        if let Some(error) = &self.customizer_error {
            ui.label(
                RichText::new(format!("Parameters unavailable: {error}"))
                    .color(theme::warn())
                    .small(),
            );
        }
        if self.customizer.is_empty() {
            ui.label(
                RichText::new(
                    "Declare parameters with `param name: f64 = value;` and they appear here.",
                )
                .color(theme::muted())
                .small(),
            );
            ui.add_space(6.0);
            ui.separator();
            self.faces_panel(ui);
            return;
        }
        let mut changed = false;
        let mut reset_all = false;
        egui::ScrollArea::vertical()
            .id_salt("customizer-scroll")
            .show(ui, |ui| {
                egui::Grid::new("customizer-grid")
                    .num_columns(3)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        for row in &mut self.customizer {
                            let parameter = &row.parameter;
                            let mut hover = format!("Declared on line {}", parameter.line);
                            if let Some(description) = &parameter.description {
                                hover = format!("{description}\n{hover}");
                            }
                            let title = match &parameter.unit {
                                Some(unit) => format!("{} [{unit}]", parameter.name),
                                None => parameter.name.clone(),
                            };
                            ui.label(RichText::new(title).color(theme::text()))
                                .on_hover_text(hover);
                            match parameter.default {
                                Some(default) => {
                                    let mut value = row.value.unwrap_or(default);
                                    let step = (default.abs() * 0.01).max(0.1);
                                    let mut drag_value = egui::DragValue::new(&mut value)
                                        .speed(step)
                                        .max_decimals(4);
                                    // A declared range bounds the drag; an
                                    // int or bool parameter moves in whole
                                    // steps.
                                    if let (Some(min), Some(max)) = (parameter.min, parameter.max) {
                                        drag_value = drag_value.range(min..=max);
                                    }
                                    if parameter.param_type == "int" {
                                        drag_value = drag_value.speed(1.0).max_decimals(0);
                                    } else if parameter.param_type == "bool" {
                                        drag_value =
                                            drag_value.speed(1.0).max_decimals(0).range(0.0..=1.0);
                                    }
                                    let drag = ui.add(drag_value);
                                    drag.widget_info(|| {
                                        egui::WidgetInfo::labeled(
                                            egui::WidgetType::DragValue,
                                            true,
                                            format!("Parameter {}", row.parameter.name),
                                        )
                                    });
                                    if drag.changed() {
                                        row.value = Some(value);
                                        changed = true;
                                    }
                                    let overridden = row.value.is_some_and(|v| v != default);
                                    if ui
                                        .add_enabled(overridden, egui::Button::new("↺").small())
                                        .on_hover_text(format!("Back to the script's {default}"))
                                        .clicked()
                                    {
                                        row.value = None;
                                        changed = true;
                                    }
                                }
                                None => {
                                    ui.label(RichText::new("expression").color(theme::muted()));
                                    ui.label("");
                                }
                            }
                            ui.end_row();
                        }
                    });
                ui.add_space(6.0);
                if self.customizer.iter().any(|row| row.value.is_some())
                    && ui.button("Reset all").clicked()
                {
                    reset_all = true;
                }
            });
        if reset_all {
            for row in &mut self.customizer {
                row.value = None;
            }
            changed = true;
        }
        if changed {
            self.run_requested = true;
        }
        ui.add_space(6.0);
        ui.separator();
        self.faces_panel(ui);
    }

    fn console(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("CONSOLE")
                    .font(FontId::proportional(10.0))
                    .color(theme::muted())
                    .strong(),
            );
            if let Some(snapshot) = self
                .outcome
                .as_ref()
                .and_then(|outcome| outcome.snapshot.as_ref())
            {
                let measures = snapshot.measures();
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "volume {:.3}   area {:.3}   {}",
                        measures.volume,
                        measures.surface_area,
                        measures
                            .bounds
                            .map(|bounds| format!(
                                "size {:.2} × {:.2} × {:.2}",
                                bounds.max.x - bounds.min.x,
                                bounds.max.y - bounds.min.y,
                                bounds.max.z - bounds.min.z
                            ))
                            .unwrap_or_default()
                    ))
                    .color(theme::muted())
                    .small(),
                );
            }
            if let Some(face) = self.selected_face {
                ui.separator();
                let named = self
                    .outcome
                    .as_ref()
                    .and_then(|outcome| outcome.face_name(face.face));
                let text = match named {
                    Some(named) => format!("{} · {}", named.display_name(), named.description),
                    None => format!("face {}", face.face.entity.0),
                };
                let label = ui.label(RichText::new(text.clone()).color(theme::accent()).small());
                label.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Label,
                        true,
                        format!("Selected face: {text}"),
                    )
                });
            }
        });
        ui.add_space(2.0);
        let mut jump = None;
        let mut pick_candidate = None;
        egui::ScrollArea::vertical()
            .id_salt("console-scroll")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                let Some(outcome) = &self.outcome else {
                    ui.label(RichText::new("Waiting for the first run…").color(theme::muted()));
                    return;
                };
                for step in &outcome.steps {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("●").color(theme::good()).monospace());
                        ui.label(RichText::new(&step.label).color(theme::text()).monospace());
                        ui.label(
                            RichText::new(format!("{}", step.topology))
                                .color(theme::muted())
                                .monospace(),
                        );
                        ui.label(
                            RichText::new(format!("{} ms", step.elapsed_ms))
                                .color(theme::muted())
                                .monospace(),
                        );
                    });
                    for note in &step.notes {
                        ui.label(
                            RichText::new(format!("    ! {note}"))
                                .color(theme::warn())
                                .monospace(),
                        );
                    }
                }
                if let Some(error) = &outcome.error {
                    let label = ui.add(
                        egui::Label::new(
                            RichText::new(error.headline())
                                .color(theme::bad())
                                .monospace(),
                        )
                        .sense(egui::Sense::click()),
                    );
                    label.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Script error")
                    });
                    if error.location.is_some() {
                        let label = label.on_hover_text("Go to the line");
                        if label.clicked() {
                            jump = error.location.map(|(line, _)| line);
                        }
                    }
                    // Beneath the headline, what the kernel added: the
                    // diagnostics in their own words, what to try, the
                    // entities it could have meant, and the code last.
                    for detail in &error.details {
                        ui.label(
                            RichText::new(format!("    {detail}"))
                                .color(theme::muted())
                                .monospace(),
                        );
                    }
                    if let Some(suggestion) = &error.suggestion {
                        ui.label(
                            RichText::new(format!("    Try: {suggestion}"))
                                .color(theme::warn())
                                .monospace(),
                        );
                    }
                    if !error.candidates.is_empty() {
                        ui.label(
                            RichText::new(format!(
                                "    {} candidate{}:",
                                error.candidates.len(),
                                if error.candidates.len() == 1 { "" } else { "s" }
                            ))
                            .color(theme::muted())
                            .monospace(),
                        );
                    }
                    for candidate in &error.candidates {
                        // The candidates belong to the snapshot on screen,
                        // the one the failing step was handed, so a face
                        // among them can be picked out in the viewport.
                        let selectable = candidate.entity.kind == EntityKind::Face;
                        let row = ui.add(
                            egui::Label::new(
                                RichText::new(format!("    · {}", candidate.description))
                                    .color(theme::text())
                                    .monospace(),
                            )
                            .sense(if selectable {
                                egui::Sense::click()
                            } else {
                                egui::Sense::hover()
                            }),
                        );
                        row.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                true,
                                format!("Candidate {}", candidate.description),
                            )
                        });
                        if selectable && row.on_hover_text("Select this face").clicked() {
                            pick_candidate = Some(candidate.entity);
                        }
                    }
                    if let Some(code) = &error.code {
                        ui.label(
                            RichText::new(format!("    {code}"))
                                .color(theme::muted())
                                .small()
                                .monospace(),
                        );
                    }
                } else if outcome.steps.is_empty() {
                    ui.label(
                        RichText::new("The script ran but built nothing; add a step with a label.")
                            .color(theme::muted())
                            .monospace(),
                    );
                }
            });
        if jump.is_some() {
            self.jump_to_line = jump;
        }
        if let Some(entity) = pick_candidate {
            self.selected_face = Some(DocumentFaceSelection {
                body: BODY,
                face: entity,
            });
        }
    }

    /// One frame of 3D-mouse motion, with the default mapping: the cap
    /// orbits, pans, and zooms the model, and either button fits it again.
    fn apply_spacemouse(&mut self, ctx: &egui::Context) {
        let Some(mouse) = self.spacemouse.as_ref() else {
            return;
        };
        let motion = mouse.take_motion();
        if motion.is_empty() && self.spacemouse_filter.is_still() {
            return;
        }
        let mut changed = false;
        if motion.buttons_pressed != 0 {
            self.view.reset_orientation();
            if let Some(bounds) = self.framed_bounds {
                self.view.frame(bounds);
            }
            changed = true;
        }
        let seconds = f64::from(ctx.input(|input| input.stable_dt));
        let settings = SixDofSettings::default();
        let six_dof = self.spacemouse_filter.feed(
            SixDofMotion {
                translate: motion.translate,
                rotate: motion.rotate,
            },
            seconds,
            &settings,
        );
        if self.view.apply_six_dof(six_dof, seconds, settings) {
            changed = true;
        }
        if changed {
            ctx.request_repaint();
        }
        if !self.spacemouse_filter.is_still() {
            ctx.request_repaint_after(std::time::Duration::from_millis(8));
        }
    }

    fn viewport(&mut self, ui: &mut egui::Ui) {
        self.apply_spacemouse(ui.ctx());
        let scene = self
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.scene.as_ref());
        let bounds = self.framed_bounds;
        let pivot = bounds.map_or(Point3::new(0.0, 0.0, 0.0), |bounds| {
            Point3::new(
                (bounds.min.x + bounds.max.x) * 0.5,
                (bounds.min.y + bounds.max.y) * 0.5,
                (bounds.min.z + bounds.max.z) * 0.5,
            )
        });
        let bodies: Vec<DocumentBodyInstance<'_>> = scene
            .map(|scene| vec![DocumentBodyInstance::new(BODY, scene, bounds, pivot)])
            .unwrap_or_default();
        // The phase drives the workbench's joint-motion animation, which
        // spins the active body about its pivot; a script's body stands
        // still.
        let animation_phase = 0.0;
        self.view.section_cut_plane = self.section.cut_plane();
        let output = show_document_with_feature_drag(
            ui,
            &bodies,
            bounds,
            true,
            self.display_mode,
            self.selected_face,
            None,
            None,
            &[],
            &[],
            &[],
            (!bodies.is_empty()).then_some(BODY),
            ActiveTool::Select,
            &mut self.transform,
            &mut self.view,
            animation_phase,
            None,
            &[],
            &[],
            &[],
            None,
            None,
            &mut self.drag,
            &mut self.edge_frame_memo,
            NavigationPreset::Artificer.bindings(),
        );
        if let Some(face) = output.selected_face {
            self.selected_face = Some(face);
        } else if output.clicked_empty {
            self.selected_face = None;
        }
    }

    fn path_prompt(&mut self, ctx: &egui::Context) {
        let Some(mut prompt) = self.prompt.take() else {
            return;
        };
        let mut accepted = false;
        let mut cancelled = false;
        egui::Window::new(prompt.purpose.title())
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.label(RichText::new("Path").color(theme::muted()).small());
                let field = ui
                    .add(egui::TextEdit::singleline(&mut prompt.text).desired_width(f32::INFINITY));
                field.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Path")
                });
                if !field.has_focus() && !field.lost_focus() {
                    field.request_focus();
                }
                if field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    accepted = true;
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new(
                            RichText::new(prompt.purpose.verb()).strong(),
                        ))
                        .clicked()
                    {
                        accepted = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                });
            });
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            cancelled = true;
        }
        if accepted {
            self.accept_prompt(prompt);
        } else if cancelled {
            // A save the unsaved-changes prompt asked for was abandoned:
            // so is the action that waited on it.
            self.resume_after_save = None;
        } else {
            self.prompt = Some(prompt);
        }
    }
}

impl eframe::App for ScriptStudio {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // The window's close button: a dirty script is asked about first,
        // and the close goes ahead only once the user has answered.
        if ctx.input(|input| input.viewport().close_requested())
            && !self.close_confirmed
            && self.is_dirty()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.discard_prompt = Some(DiscardAction::CloseWindow);
        }
        if std::mem::take(&mut self.close_window) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.prompt.is_none() && self.pending_import.is_none() && self.discard_prompt.is_none() {
            self.handle_shortcuts(ctx);
        }
        self.tick(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let palette = theme::palette();
        let chrome = |fill: Color32, margin: egui::Margin| {
            egui::Frame::new()
                .fill(fill)
                .inner_margin(margin)
                .stroke(egui::Stroke::new(1.0, palette.border))
        };
        egui::Panel::top("studio-header")
            .exact_size(38.0)
            .show_separator_line(false)
            .frame(chrome(palette.panel, egui::Margin::symmetric(10, 0)))
            .show(ui, |ui| self.header(ui));

        egui::Panel::bottom("studio-console")
            .resizable(true)
            .default_size(170.0)
            .min_size(60.0)
            .frame(chrome(palette.panel, egui::Margin::symmetric(10, 4)))
            .show(ui, |ui| self.console(ui));

        egui::Panel::left("studio-editor")
            .resizable(true)
            .default_size(480.0)
            .min_size(260.0)
            .frame(chrome(palette.card, egui::Margin::symmetric(6, 4)))
            .show(ui, |ui| self.editor(ui));

        if self.show_customizer {
            egui::Panel::right("studio-customizer")
                .resizable(true)
                .default_size(230.0)
                .min_size(160.0)
                .frame(chrome(palette.panel, egui::Margin::symmetric(10, 4)))
                .show(ui, |ui| self.customizer_panel(ui));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.viewport(ui));

        let ctx = ui.ctx().clone();
        self.path_prompt(&ctx);
        self.import_window(&ctx);
        self.unsaved_changes_window(&ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_highlighter_covers_the_whole_source_in_order() {
        let source =
            "param r: f64 = 2.5; // radius\nlet b = box(size: [r, 1, \"x\"], label: \"b\");\n";
        let tokens = highlight_tokens(source);
        let mut end = 0;
        for (_, range) in &tokens {
            assert_eq!(range.start, end);
            end = range.end;
        }
        assert_eq!(end, source.len());
        let kinds: Vec<TokenKind> = tokens
            .iter()
            .filter(|(kind, _)| !matches!(kind, TokenKind::Whitespace | TokenKind::Punctuation))
            .map(|(kind, _)| *kind)
            .collect();
        assert_eq!(kinds[0], TokenKind::Keyword);
        assert_eq!(kinds[1], TokenKind::Identifier);
        assert_eq!(kinds[2], TokenKind::Keyword);
        assert_eq!(kinds[3], TokenKind::Number);
        assert_eq!(kinds[4], TokenKind::Comment);
        assert!(kinds.contains(&TokenKind::Builtin));
        assert!(kinds.contains(&TokenKind::String));
    }

    #[test]
    fn a_step_failure_points_at_the_line_that_labels_it() {
        let source = "let a = box(size: [1, 2, 3], label: \"a\");\n// label: \"ghost\"\nlet b = cylinder(center: [0, 0, 0], axis: [0, 0, 1], radius: 1, height: 2,\n    label: \"b\");\n";
        assert_eq!(line_of_label(source, "a"), Some(1));
        assert_eq!(line_of_label(source, "b"), Some(4));
        assert_eq!(line_of_label(source, "ghost"), None);
    }

    #[test]
    fn running_a_script_reports_every_step_and_a_scene() {
        let outcome = run_script(WELCOME_SCRIPT, &BTreeMap::new(), &CancellationToken::new());
        assert!(outcome.succeeded(), "{:?}", outcome.error);
        assert!(outcome.steps.len() >= 3);
        assert!(outcome.scene.is_some());
        assert!(outcome.snapshot.is_some());
    }

    #[test]
    fn the_hub_names_its_faces_from_the_script_and_from_history() {
        let outcome = run_script(WELCOME_SCRIPT, &BTreeMap::new(), &CancellationToken::new());
        assert!(outcome.succeeded(), "{:?}", outcome.error);
        let named: Vec<&str> = outcome
            .faces
            .iter()
            .filter_map(|face| face.script_name.as_deref())
            .collect();
        assert_eq!(named, ["flange_bottom", "flange_top", "hub_top"]);
        let flange_top = outcome
            .faces
            .iter()
            .find(|face| face.script_name.as_deref() == Some("flange_top"))
            .unwrap();
        assert!(
            flange_top.description.starts_with("planar, facing up"),
            "{}",
            flange_top.description
        );
        assert!(
            flange_top.description.contains("8.0)"),
            "{}",
            flange_top.description
        );
        // Every face has a history name, and the bolt walls carry their step.
        assert!(
            outcome
                .faces
                .iter()
                .all(|face| !face.history_name.is_empty())
        );
        assert!(
            outcome
                .faces
                .iter()
                .any(|face| face.history_name.starts_with("bolt_0.")),
            "{:?}",
            outcome
                .faces
                .iter()
                .map(|f| f.history_name.clone())
                .collect::<Vec<_>>()
        );
        // Script names come first in the list.
        assert!(outcome.faces[0].script_name.is_some());
    }

    #[test]
    fn a_journal_pulls_back_into_the_script_behind_a_semantic_diff() {
        let outcome = run_script(WELCOME_SCRIPT, &BTreeMap::new(), &CancellationToken::new());
        let journal = outcome.journal.clone().expect("the run keeps its journal");
        // Against the script that made it, no step differs: the decompiler
        // names parameters after the dimensions, and a journal carries no
        // script names.
        let pending = pull_journal(WELCOME_SCRIPT, None, &journal, "hub.journal.json").unwrap();
        assert!(pending.note.is_none());
        assert!(
            pending.script.contains("let hub = revolve("),
            "{}",
            pending.script
        );
        assert!(
            pending
                .diff
                .lines()
                .iter()
                .all(|line| line.starts_with("param ") || line.starts_with("name ")),
            "{:?}",
            pending.diff.lines()
        );
        // Against a script with one hole fewer, the diff names the step.
        let fewer =
            WELCOME_SCRIPT.replace("param bolt_count: f64 = 4;", "param bolt_count: f64 = 3;");
        let pending = pull_journal(&fewer, None, &journal, "hub.journal.json").unwrap();
        assert!(
            pending
                .diff
                .lines()
                .iter()
                .any(|line| line.starts_with("step \"bolt_3\"")),
            "{:?}",
            pending.diff.lines()
        );
        // The decompiled script builds the same body.
        let rebuilt = run_script(&pending.script, &BTreeMap::new(), &CancellationToken::new());
        assert_eq!(
            rebuilt.snapshot.unwrap().semantic_digest(),
            outcome.snapshot.unwrap().semantic_digest()
        );
    }

    #[test]
    fn a_parameter_override_changes_the_model() {
        let source = "param w: f64 = 10.0;\nlet b = box(size: [w, 10, 10], label: \"b\");\n";
        let plain = run_script(source, &BTreeMap::new(), &CancellationToken::new());
        let mut overrides = BTreeMap::new();
        overrides.insert("w".to_owned(), 20.0);
        let wide = run_script(source, &overrides, &CancellationToken::new());
        let volume = |outcome: &RunOutcome| outcome.snapshot.as_ref().unwrap().measures().volume;
        assert!((volume(&plain) - 1000.0).abs() < 1.0e-9);
        assert!((volume(&wide) - 2000.0).abs() < 1.0e-9);
    }

    #[test]
    fn a_failing_step_keeps_the_model_built_so_far() {
        let source = "let a = box(size: [10, 10, 10], label: \"a\");\nlet f = fillet(edges: [edges(\"|Z\")], radius: 40, label: \"too_big\");\n";
        let outcome = run_script(source, &BTreeMap::new(), &CancellationToken::new());
        let error = outcome.error.as_ref().expect("the fillet fails");
        assert_eq!(error.location, Some((2, 1)), "{error:?}");
        assert_eq!(outcome.steps.len(), 1);
        assert!(outcome.scene.is_some(), "the box stays on screen");
    }

    #[test]
    fn a_cancelled_run_publishes_nothing() {
        let token = CancellationToken::new();
        token.cancel();
        let outcome = run_script(WELCOME_SCRIPT, &BTreeMap::new(), &token);
        assert!(outcome.cancelled);
        assert!(outcome.scene.is_none());
    }

    /// The console prints a refusal message first, then what to try, then
    /// the entities the kernel could have meant, and the code last.
    #[test]
    fn a_step_refusal_prints_message_suggestion_candidates_then_code() {
        use artificer_kernel::api::debug::EntityInfo;
        use artificer_protocol::{EntityId, SnapshotId};

        let snapshot = SnapshotId::new([7; 16]);
        let candidate = |id: u64, description: &str, role: Option<&str>| EntityInfo {
            kind: EntityKind::Face,
            entity_ref: EntityRef {
                snapshot,
                entity: EntityId(id),
                kind: EntityKind::Face,
            },
            geometry_description: description.to_owned(),
            role: role.map(str::to_owned),
            ordinal: None,
        };
        let error = ApiError::new(
            ApiErrorCode::SelectorAmbiguous,
            "faces(\"|Z\") matched 2 entities",
        )
        .with_suggestion("Use this selector where a set is accepted, or narrow it")
        .with_candidates(vec![
            candidate(12, "planar, facing up", Some("top")),
            candidate(13, "planar, facing down", None),
        ]);
        let command = ApiCommand::MakeBox {
            label: "raise".to_owned(),
            origin: Point3::new(0.0, 0.0, 0.0),
            size: [1.0, 1.0, 1.0],
        };
        let source = "let a = box(size: [1, 1, 1], label: \"a\");\nlet r = push_pull(face: faces(\"|Z\"), distance: 3, label: \"raise\");\n";

        let run_error = RunError::from_step(source, &command, &error);
        assert_eq!(
            run_error.headline(),
            "✕ Step \"raise\" at line 2, column 1: faces(\"|Z\") matched 2 entities"
        );
        assert_eq!(
            run_error.console_lines(),
            vec![
                "Try: Use this selector where a set is accepted, or narrow it".to_owned(),
                "2 candidates:".to_owned(),
                "· face 12 · planar, facing up (role top)".to_owned(),
                "· face 13 · planar, facing down".to_owned(),
                "selector_ambiguous".to_owned(),
            ]
        );
        assert_eq!(run_error.candidates[0].entity.entity, EntityId(12));

        // A script error has no kernel behind it: nothing but the headline.
        let parse = RunError::from_script(&ScriptError::Parse {
            message: "expected `)`".to_owned(),
            location: Some((1, 4)),
        });
        assert!(parse.console_lines().is_empty());
        assert_eq!(
            parse.headline(),
            "✕ Parse error at line 1, column 4: expected `)`"
        );
    }

    /// The kernel's own refusal of an ambiguous selector reaches the run
    /// with its candidates, so the console has faces to offer.
    #[test]
    fn an_ambiguous_selector_offers_its_candidates() {
        let source = "let a = box(size: [10, 10, 10], label: \"a\");\nlet r = push_pull(face: faces(\"planar\"), distance: 3, label: \"raise\");\n";
        let outcome = run_script(source, &BTreeMap::new(), &CancellationToken::new());
        let error = outcome
            .error
            .as_ref()
            .expect("every face of a box is planar, so one cannot be meant");
        assert_eq!(error.location, Some((2, 1)), "{error:?}");
        assert_eq!(error.candidates.len(), 6, "{error:?}");
        assert!(error.suggestion.is_some(), "{error:?}");
        assert_eq!(error.code.as_deref(), Some("selector_ambiguous"));
        let lines = error.console_lines();
        assert!(
            lines.iter().any(|line| line.starts_with("Try: ")),
            "{lines:?}"
        );
        assert_eq!(lines.last().map(String::as_str), Some("selector_ambiguous"));
        // The candidates belong to the snapshot still on screen.
        let scene = outcome.scene.as_ref().expect("the box stays on screen");
        for candidate in &error.candidates {
            assert!(
                scene
                    .triangles
                    .iter()
                    .any(|triangle| triangle.source_face == candidate.entity),
                "{candidate:?} is not a face of the displayed body"
            );
        }
    }
}
