//! Journal to `.art`: a session's command history written back as a
//! script a person can read, edit and run.
//!
//! Every step becomes a `let` bound to the feature call that made it, so
//! later steps can name it. Dimensions become `param`s with their current
//! values. Selectors are written as the script language spells them;
//! snapshot-bound references, which no script can write, are regenerated
//! as history selectors from the step that produced the entity, so they
//! stay stable across edits. Steps the faceted tier built are annotated.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use artificer_protocol::{EntityKind, EntityRef, Point2, Point3, Tier, Vector3};

use crate::api::commands::{ApiCommand, ExtrudeOp, PatternPlacement, SketchEntity, SketchPlane};
use crate::api::debug::{ApiError, ApiErrorCode};
use crate::api::journal::Journal;
use crate::api::selectors::{
    EntitySelector, Extremum, GeometricSelector, Metric, NormalMatch, SurfaceFilter,
};
use crate::api::session::Session;
use crate::{CancellationToken, NativeKernel};

/// What the decompiler turns into `param`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamPolicy {
    /// Every dimension (distances, diameters, radii, heights, spacings,
    /// counts, draft angles) becomes a `param` named `<label>_<field>`.
    Dimensions,
    /// Every value is written inline.
    None,
}

/// How a journal is written as a script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecompileOptions {
    pub params: ParamPolicy,
    /// A comment block at the top naming where the script came from.
    pub header: bool,
}

impl Default for DecompileOptions {
    fn default() -> Self {
        Self {
            params: ParamPolicy::Dimensions,
            header: true,
        }
    }
}

/// Replays a journal into a fresh session and writes it as a script.
pub fn decompile_journal(
    journal: &Journal,
    options: &DecompileOptions,
) -> Result<String, ApiError> {
    let mut session = Session::new();
    let token = CancellationToken::default();
    for entry in &journal.entries {
        session.execute(entry.command.clone(), &token)?;
    }
    session.to_art(options)
}

/// A rendered script and the names it used for steps.
struct Writer<'a> {
    session: &'a Session,
    options: &'a DecompileOptions,
    /// The script identifier of each step, by label.
    idents: BTreeMap<String, String>,
    taken: BTreeSet<String>,
    params: Vec<(String, f64)>,
    body: String,
}

impl Session {
    /// Writes the session's journal as a `.art` script that rebuilds the
    /// same body: the same commands under the same labels, so the
    /// snapshot digest is the same.
    pub fn to_art(&self, options: &DecompileOptions) -> Result<String, ApiError> {
        let mut writer = Writer {
            session: self,
            options,
            idents: BTreeMap::new(),
            taken: BTreeSet::new(),
            params: Vec::new(),
            body: String::new(),
        };
        for entry in &self.journal.entries {
            let ident = writer.ident_for(&entry.label);
            writer.idents.insert(entry.label.clone(), ident);
        }
        for entry in &self.journal.entries {
            writer.step(&entry.command)?;
        }
        for (name, selector) in &self.names {
            let selector = writer.selector(selector)?;
            let _ = writeln!(writer.body, "let {} = {selector};", ident(name));
        }

        let mut script = String::new();
        if options.header {
            script.push_str("// Decompiled from an Artificer session journal.\n");
            let _ = writeln!(
                script,
                "// {} steps; final snapshot {}.",
                self.journal.len(),
                self.snapshot.id()
            );
            if self.tier() == Tier::Approximate {
                script.push_str(
                    "// A step below fell to the faceted tier; it is marked `approximate`.\n",
                );
            }
            script.push('\n');
        }
        for (name, value) in &writer.params {
            let _ = writeln!(script, "param {name}: f64 = {};", number(*value));
        }
        if !writer.params.is_empty() {
            script.push('\n');
        }
        script.push_str(&writer.body);
        Ok(script)
    }
}

impl Writer<'_> {
    /// A script identifier for a label, unique among the steps.
    fn ident_for(&mut self, label: &str) -> String {
        let base = ident(label);
        let mut candidate = base.clone();
        let mut suffix = 2;
        while self.taken.contains(&candidate) {
            candidate = format!("{base}_{suffix}");
            suffix += 1;
        }
        self.taken.insert(candidate.clone());
        candidate
    }

    fn step_ident(&self, label: &str) -> Result<String, ApiError> {
        self.idents.get(label).cloned().ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::SessionError,
                format!("The journal refers to a step \"{label}\" it never recorded"),
            )
        })
    }

    /// A dimension: a `param` under the dimensions policy, inline otherwise.
    fn dimension(&mut self, label: &str, field: &str, value: f64) -> String {
        match self.options.params {
            ParamPolicy::Dimensions => {
                let name = self.ident_for(&format!("{}_{field}", ident(label)));
                self.params.push((name.clone(), value));
                name
            }
            ParamPolicy::None => number(value),
        }
    }

    fn step(&mut self, command: &ApiCommand) -> Result<(), ApiError> {
        let label = command.label().to_owned();
        let ident = self.step_ident(&label)?;
        if let Some(report) = self.session.step_reports.get(&label)
            && report.tier() == Tier::Approximate
        {
            let _ = writeln!(
                self.body,
                "// approximate: the faceted tier built this step ({})",
                report.rung.as_deref().unwrap_or("faceted")
            );
        }
        let call = match command {
            ApiCommand::MakeBox { origin, size, .. } => format!(
                "box(origin: {}, size: [{}, {}, {}], label: {})",
                point3(*origin),
                self.dimension(&label, "width", size[0]),
                self.dimension(&label, "depth", size[1]),
                self.dimension(&label, "height", size[2]),
                quoted(&label)
            ),
            ApiCommand::MakeCylinder {
                center,
                axis,
                radius,
                height,
                ..
            } => format!(
                "cylinder(center: {}, axis: {}, radius: {}, height: {}, label: {})",
                point3(*center),
                vector3(*axis),
                self.dimension(&label, "radius", *radius),
                self.dimension(&label, "height", *height),
                quoted(&label)
            ),
            ApiCommand::Sketch { on, entities, .. } => {
                let plane = match on {
                    SketchPlane::XY => "\"XY\"".to_owned(),
                    SketchPlane::XZ => "\"XZ\"".to_owned(),
                    SketchPlane::YZ => "\"YZ\"".to_owned(),
                    SketchPlane::OnFace { face } => self.selector(face)?,
                };
                let entities = entities
                    .iter()
                    .map(|entity| match entity {
                        SketchEntity::Line { start, end } => {
                            format!("line(start: {}, end: {})", point2(*start), point2(*end))
                        }
                        SketchEntity::Circle { center, radius } => format!(
                            "circle(center: {}, radius: {})",
                            point2(*center),
                            number(*radius)
                        ),
                        SketchEntity::Arc {
                            center,
                            radius,
                            start_angle,
                            end_angle,
                        } => format!(
                            "arc(center: {}, radius: {}, start_angle: {}, end_angle: {})",
                            point2(*center),
                            number(*radius),
                            number(start_angle.to_degrees()),
                            number(end_angle.to_degrees())
                        ),
                        SketchEntity::Rectangle {
                            origin,
                            width,
                            height,
                        } => format!(
                            "rect(origin: {}, width: {}, height: {})",
                            point2(*origin),
                            number(*width),
                            number(*height)
                        ),
                    })
                    .collect::<Vec<_>>();
                format!(
                    "sketch(on: {plane}, entities: [\n    {}\n], label: {})",
                    entities.join(",\n    "),
                    quoted(&label)
                )
            }
            ApiCommand::Extrude {
                sketch,
                regions,
                distance,
                operation,
                draft_degrees,
                ..
            } => {
                let mut call = format!(
                    "extrude(sketch: {}{}, distance: {}, operation: {}",
                    self.step_ident(&sketch.0)?,
                    regions_text(regions),
                    self.dimension(&label, "distance", *distance),
                    operation_text(*operation),
                );
                if *draft_degrees != 0.0 {
                    let _ = write!(
                        call,
                        ", draft: {}",
                        self.dimension(&label, "draft", *draft_degrees)
                    );
                }
                let _ = write!(call, ", label: {})", quoted(&label));
                call
            }
            ApiCommand::Revolve {
                sketch,
                regions,
                axis_origin,
                axis_direction,
                angle_degrees,
                operation,
                ..
            } => format!(
                "revolve(sketch: {}{}, axis_origin: {}, axis: {}, angle: {}, operation: {}, label: {})",
                self.step_ident(&sketch.0)?,
                regions_text(regions),
                point3(*axis_origin),
                vector3(*axis_direction),
                number(*angle_degrees),
                operation_text(*operation),
                quoted(&label)
            ),
            ApiCommand::PushPull { face, distance, .. } => format!(
                "push_pull(face: {}, distance: {}, label: {})",
                self.selector(face)?,
                self.dimension(&label, "distance", *distance),
                quoted(&label)
            ),
            ApiCommand::DrillHole {
                face,
                center,
                diameter,
                depth,
                ..
            } => format!(
                "drill(face: {}, center: {}, diameter: {}, depth: {}, label: {})",
                self.selector(face)?,
                point2(*center),
                self.dimension(&label, "diameter", *diameter),
                self.dimension(&label, "depth", *depth),
                quoted(&label)
            ),
            ApiCommand::Fillet { edges, radius, .. } => format!(
                "fillet(edges: {}, radius: {}, label: {})",
                self.selectors(edges)?,
                self.dimension(&label, "radius", *radius),
                quoted(&label)
            ),
            ApiCommand::Shell { open, wall, .. } => {
                let open = if open.is_empty() {
                    String::new()
                } else {
                    format!("open: {}, ", self.selectors(open)?)
                };
                format!(
                    "shell({open}wall: {}, label: {})",
                    self.dimension(&label, "wall", *wall),
                    quoted(&label)
                )
            }
            ApiCommand::Chamfer {
                edges, distance, ..
            } => format!(
                "chamfer(edges: {}, distance: {}, label: {})",
                self.selectors(edges)?,
                self.dimension(&label, "distance", *distance),
                quoted(&label)
            ),
            ApiCommand::Mirror {
                plane_origin,
                plane_normal,
                ..
            } => format!(
                "mirror(origin: {}, normal: {}, label: {})",
                point3(*plane_origin),
                vector3(*plane_normal),
                quoted(&label)
            ),
            ApiCommand::LinearPattern {
                direction,
                spacing,
                count,
                ..
            } => format!(
                "pattern(direction: {}, spacing: {}, count: {}, label: {})",
                vector3(*direction),
                self.dimension(&label, "spacing", *spacing),
                self.dimension(&label, "count", f64::from(*count)),
                quoted(&label)
            ),
            ApiCommand::FeaturePattern {
                step, placement, ..
            } => {
                let source = self.step_ident(&step.0)?;
                match placement {
                    PatternPlacement::Linear {
                        direction,
                        spacing,
                        count,
                    } => format!(
                        "pattern(step: {source}, direction: {}, spacing: {}, count: {}, label: {})",
                        vector3(*direction),
                        self.dimension(&label, "spacing", *spacing),
                        self.dimension(&label, "count", f64::from(*count)),
                        quoted(&label)
                    ),
                    PatternPlacement::Circular {
                        axis_origin,
                        axis_direction,
                        count,
                        angle_step_degrees,
                    } => {
                        let angle = if *angle_step_degrees == 0.0 {
                            String::new()
                        } else {
                            format!(", angle: {}", number(*angle_step_degrees))
                        };
                        format!(
                            "pattern(step: {source}, axis: {}, axis_origin: {}, count: {}{angle}, label: {})",
                            vector3(*axis_direction),
                            point3(*axis_origin),
                            self.dimension(&label, "count", f64::from(*count)),
                            quoted(&label)
                        )
                    }
                }
            }
            ApiCommand::BooleanUnion { target, tool, .. } => format!(
                "union(target: {}, tool: {}, label: {})",
                self.step_ident(&target.0)?,
                self.step_ident(&tool.0)?,
                quoted(&label)
            ),
            ApiCommand::BooleanDifference { target, tool, .. } => format!(
                "difference(target: {}, tool: {}, label: {})",
                self.step_ident(&target.0)?,
                self.step_ident(&tool.0)?,
                quoted(&label)
            ),
            ApiCommand::BooleanIntersection { target, tool, .. } => format!(
                "intersection(target: {}, tool: {}, label: {})",
                self.step_ident(&target.0)?,
                self.step_ident(&tool.0)?,
                quoted(&label)
            ),
        };
        let _ = writeln!(self.body, "let {ident} = {call};");
        Ok(())
    }

    fn selectors(&mut self, selectors: &[EntitySelector]) -> Result<String, ApiError> {
        let rendered = selectors
            .iter()
            .map(|selector| self.selector(selector))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(match rendered.as_slice() {
            [single] => single.clone(),
            many => format!("[{}]", many.join(", ")),
        })
    }

    /// A selector as the script language spells it.
    fn selector(&mut self, selector: &EntitySelector) -> Result<String, ApiError> {
        Ok(match selector {
            EntitySelector::ByHistory {
                from_step,
                kind,
                role,
                ordinal,
            } => {
                let method = match kind {
                    EntityKind::Face => "face",
                    EntityKind::Edge => "edge",
                    other => {
                        return Err(ApiError::new(
                            ApiErrorCode::InvalidInput,
                            format!("A script cannot select a {other:?} by history"),
                        ));
                    }
                };
                let ordinal =
                    ordinal.map_or(String::new(), |ordinal| format!(", ordinal: {ordinal}"));
                format!(
                    "{}.{method}({}{ordinal})",
                    self.step_ident(&from_step.0)?,
                    quoted(role)
                )
            }
            EntitySelector::ByGeometry { selector } => geometric(selector, self)?,
            EntitySelector::Direct { entity_ref } => self.direct(*entity_ref)?,
        })
    }

    /// A snapshot-bound reference, regenerated as the history selector of
    /// the step that made the entity, or failing that as the nearest entity
    /// to where it was.
    fn direct(&mut self, entity: EntityRef) -> Result<String, ApiError> {
        for label in &self.session.step_order {
            let Some(report) = self.session.step_reports.get(label) else {
                continue;
            };
            if report.output_snapshot != entity.snapshot {
                continue;
            }
            for record in &report.history {
                let Some(role) = &record.role else { continue };
                if role.name.contains("preserved") || !record.outputs.contains(&entity) {
                    continue;
                }
                let method = match entity.kind {
                    EntityKind::Face => "face",
                    EntityKind::Edge => "edge",
                    _ => continue,
                };
                let ordinal = role
                    .ordinal
                    .map_or(String::new(), |ordinal| format!(", ordinal: {ordinal}"));
                return Ok(format!(
                    "{}.{method}({}{ordinal})",
                    self.step_ident(label)?,
                    quoted(&role.name)
                ));
            }
        }
        let snapshot = self
            .session
            .snapshot_cache
            .get(&entity.snapshot)
            .ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::SelectorNotFound,
                    format!(
                        "The entity {entity} belongs to a snapshot the session no longer holds"
                    ),
                )
            })?;
        let (point, kind) = match entity.kind {
            EntityKind::Face => (
                NativeKernel::describe_face(snapshot, entity)
                    .map_err(ApiError::from)?
                    .centre,
                "face",
            ),
            EntityKind::Edge => (
                NativeKernel::describe_edge(snapshot, entity)
                    .map_err(ApiError::from)?
                    .midpoint,
                "edge",
            ),
            other => {
                return Err(ApiError::new(
                    ApiErrorCode::InvalidInput,
                    format!("A script cannot select a {other:?} directly"),
                ));
            }
        };
        Ok(format!(
            "nearest(point: {}, kind: \"{kind}\")",
            point3(point)
        ))
    }
}

fn geometric(selector: &GeometricSelector, writer: &mut Writer<'_>) -> Result<String, ApiError> {
    Ok(match selector {
        GeometricSelector::FaceByNormal {
            direction,
            match_kind,
        } => {
            let axis = axis_word(*direction);
            match (axis, match_kind) {
                (Some(word), NormalMatch::Closest) => format!("faces(\"{word}\")"),
                _ => format!(
                    "faces(direction: {}, match: \"{}\")",
                    vector3(*direction),
                    match match_kind {
                        NormalMatch::Closest => "closest",
                        NormalMatch::Farthest => "farthest",
                        NormalMatch::Parallel => "parallel",
                        NormalMatch::Perpendicular => "perpendicular",
                    }
                ),
            }
        }
        GeometricSelector::NearestTo { point, kind } => format!(
            "nearest(point: {}, kind: \"{}\")",
            point3(*point),
            match kind {
                EntityKind::Face => "face",
                EntityKind::Edge => "edge",
                EntityKind::Vertex => "vertex",
                other => {
                    return Err(ApiError::new(
                        ApiErrorCode::InvalidInput,
                        format!("A script cannot select the nearest {other:?}"),
                    ));
                }
            }
        ),
        GeometricSelector::ByType { surface_type, .. } => format!(
            "faces(\"{}\")",
            match surface_type {
                SurfaceFilter::Planar => "planar",
                SurfaceFilter::Cylindrical => "cylindrical",
                SurfaceFilter::Spherical => "spherical",
                SurfaceFilter::Conical => "conical",
                SurfaceFilter::Toroidal => "toroidal",
            }
        ),
        GeometricSelector::EdgeBetween { face_a, face_b } => format!(
            "edge_between(a: {}, b: {})",
            writer.selector(face_a)?,
            writer.selector(face_b)?
        ),
        GeometricSelector::ByExtremum {
            metric,
            extremum,
            kind,
        } => {
            let function = match kind {
                EntityKind::Face => "faces",
                EntityKind::Edge => "edges",
                other => {
                    return Err(ApiError::new(
                        ApiErrorCode::InvalidInput,
                        format!("A script cannot select a {other:?} by extremum"),
                    ));
                }
            };
            format!(
                "{function}(metric: \"{}\", extremum: \"{}\")",
                match metric {
                    Metric::Area => "area",
                    Metric::Length => "length",
                    Metric::Radius => "radius",
                },
                match extremum {
                    Extremum::Maximum => "max",
                    Extremum::Minimum => "min",
                }
            )
        }
        GeometricSelector::EdgesParallelTo { direction } => match axis_word(*direction) {
            Some(word) if word.starts_with('>') => format!("edges(\"|{}\")", &word[1..]),
            _ => format!("edges(direction: {})", vector3(*direction)),
        },
    })
}

/// `>Z`, `<X` and so on for a direction along an axis.
fn axis_word(direction: Vector3) -> Option<&'static str> {
    let unit = |value: f64| (value.abs() - 1.0).abs() < 1.0e-12;
    let zero = |value: f64| value.abs() < 1.0e-12;
    Some(match (direction.x, direction.y, direction.z) {
        (x, y, z) if unit(x) && zero(y) && zero(z) => {
            if x > 0.0 {
                ">X"
            } else {
                "<X"
            }
        }
        (x, y, z) if zero(x) && unit(y) && zero(z) => {
            if y > 0.0 {
                ">Y"
            } else {
                "<Y"
            }
        }
        (x, y, z) if zero(x) && zero(y) && unit(z) => {
            if z > 0.0 {
                ">Z"
            } else {
                "<Z"
            }
        }
        _ => return None,
    })
}

fn regions_text(regions: &[u32]) -> String {
    if regions.is_empty() {
        String::new()
    } else {
        format!(
            ", regions: [{}]",
            regions
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn operation_text(operation: ExtrudeOp) -> &'static str {
    match operation {
        ExtrudeOp::New => "\"new\"",
        ExtrudeOp::Add => "\"add\"",
        ExtrudeOp::Cut => "\"cut\"",
    }
}

/// A label as a script identifier: letters, digits and underscores, not
/// starting with a digit, not a keyword.
pub fn ident(label: &str) -> String {
    let mut out: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if out.is_empty() || out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert_str(0, "s_");
    }
    if matches!(
        out.as_str(),
        "param" | "let" | "for" | "in" | "fn" | "return" | "use" | "with" | "true" | "false" | "pi"
    ) {
        out.push('_');
    }
    out
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A number as script text, round-tripping exactly: whole numbers without
/// a fraction, everything else with the shortest digits that read back to
/// the same float.
pub fn number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1.0e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn point3(point: Point3) -> String {
    format!(
        "[{}, {}, {}]",
        number(point.x),
        number(point.y),
        number(point.z)
    )
}

fn vector3(vector: Vector3) -> String {
    format!(
        "[{}, {}, {}]",
        number(vector.x),
        number(vector.y),
        number(vector.z)
    )
}

fn point2(point: Point2) -> String {
    format!("[{}, {}]", number(point.x), number(point.y))
}
