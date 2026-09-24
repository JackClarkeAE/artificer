//! The CAM tab (ADR 0057): Auto-CAM plans the active body, the card shows
//! the setup, the operations and a simulation timeline, and Export writes
//! the G-code.
//!
//! Everything here is presentation over `artificer_cam`. Auto-CAM stages a
//! plan behind the shared pending-operation gate (ADR 0007): the plan, its
//! G-code and its simulation are computed at once so the card can be read
//! and scrubbed while it is pending, Confirm keeps them, Cancel drops them.
//! Nothing in this module executes the kernel: the crate below reads it
//! through its public queries, and the boundary script lists this file with
//! the other presentation modules that never grow an execution site.

use std::path::{Path, PathBuf};

use artificer_cam::interpreter::interpret;
use artificer_cam::post::post;
use artificer_cam::recognise::{MillAllowances, TurnAllowances, WorkOrigin, recognise_with};
use artificer_cam::simulate::{
    LatheSimulation, MillSimulation, Motion, lathe_tool_at, position_along, start_position,
};
use artificer_cam::stock::{LatheStock, MillStock};
use artificer_cam::{Machine, Material, Plan, Setup, ToolLibrary, geom, plan_setup};
use artificer_kernel::Snapshot;
use artificer_model::BodyId;
use artificer_protocol::{PlanarRegion2, Point2, Point3};
use eframe::egui;
use egui::{Color32, RichText};

use crate::{
    ExportSubject, KernelLabApp, PendingOperation, WorkbenchMode, status_line, theme, viewport,
};

/// Which part of the card is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CamCardSection {
    Setup,
    Operations,
    Simulate,
}

/// The simulation a study carries, one per machine.
pub(crate) enum Simulation {
    Lathe(LatheSimulation),
    Mill(MillSimulation),
}

impl Simulation {
    fn motions(&self) -> &[Motion] {
        match self {
            Self::Lathe(simulation) => &simulation.motions,
            Self::Mill(simulation) => &simulation.motions,
        }
    }

    fn total_seconds(&self) -> f64 {
        match self {
            Self::Lathe(simulation) => simulation.total_seconds,
            Self::Mill(simulation) => simulation.total_seconds,
        }
    }

    fn at(&self, seconds: f64) -> Option<(usize, f64)> {
        match self {
            Self::Lathe(simulation) => simulation.at(seconds),
            Self::Mill(simulation) => simulation.at(seconds),
        }
    }

    fn collisions(&self) -> &[artificer_cam::simulate::Collision] {
        match self {
            Self::Lathe(simulation) => &simulation.collisions,
            Self::Mill(simulation) => &simulation.collisions,
        }
    }
}

/// One planned body: what Auto-CAM decided, the program, and its simulation.
pub(crate) struct CamStudy {
    pub(crate) body: BodyId,
    pub(crate) setup: Setup,
    pub(crate) plan: Plan,
    pub(crate) gcode: String,
    pub(crate) simulation: Simulation,
    placement: viewport::RigidOccurrenceTransform,
    /// Every motion's path in the world, with whether it cuts.
    paths: Vec<(Vec<Point3>, bool)>,
    /// The untouched stock, in the world.
    stock_ghost: Vec<[Point3; 3]>,
}

impl CamStudy {
    fn total_seconds(&self) -> f64 {
        self.simulation.total_seconds()
    }

    /// A point in the plan's work coordinates, in the world. On the lathe a
    /// point is `(r, 0, z)` and lands in the section's half-plane.
    fn to_world(&self, p: Point3) -> Point3 {
        let world = match &self.setup {
            Setup::Turned(turned) => turned.axis.to_world(p.x, p.z),
            Setup::Milled(milled) => {
                let origin = milled.origin();
                milled
                    .frame
                    .to_world(Point3::new(p.x + origin.x, p.y + origin.y, p.z + origin.z))
            }
            Setup::MillTurn(_) | Setup::Unsupported { .. } => p,
        };
        self.placement.transform_point(world)
    }

    /// A lathe section point turned to an azimuth, in the world.
    fn turned_to_world(&self, r: f64, z: f64, azimuth: f64) -> Point3 {
        match &self.setup {
            Setup::Turned(turned) => self
                .placement
                .transform_point(turned.axis.to_world_at(r, z, azimuth)),
            _ => self.to_world(Point3::new(r, 0.0, z)),
        }
    }
}

/// The tab's state.
pub(crate) struct CamState {
    /// The plan in its card, beside `PendingOperation::StageCamPlan`.
    pub(crate) staged: Option<CamStudy>,
    /// The plan that was kept.
    pub(crate) committed: Option<CamStudy>,
    /// Why the last Auto-CAM planned nothing.
    pub(crate) refusal: Option<String>,
    pub(crate) section: CamCardSection,
    pub(crate) playing: bool,
    /// The simulation clock, in machining seconds.
    pub(crate) time: f64,
    pub(crate) speed: f64,
    last_tick: Option<f64>,
    library: ToolLibrary,
    library_path: Option<PathBuf>,
    library_loaded: bool,
    library_note: Option<String>,
    pub(crate) material: Material,
    /// Allowances the setup section edits before replanning.
    edited_turn: TurnAllowances,
    edited_mill: MillAllowances,
    /// The remaining-stock mesh for one motion index, rebuilt when the
    /// clock moves onto another motion.
    stock_mesh: Option<(usize, bool, Vec<[Point3; 3]>)>,
}

impl Default for CamState {
    fn default() -> Self {
        Self {
            staged: None,
            committed: None,
            refusal: None,
            section: CamCardSection::Simulate,
            playing: false,
            time: 0.0,
            speed: 4.0,
            last_tick: None,
            library: ToolLibrary::builtin(),
            library_path: None,
            library_loaded: false,
            library_note: None,
            material: Material::Aluminium,
            edited_turn: TurnAllowances {
                radial: artificer_cam::recognise::DEFAULT_RADIAL_ALLOWANCE,
                facing: artificer_cam::recognise::DEFAULT_FACING_ALLOWANCE,
            },
            edited_mill: MillAllowances {
                side: artificer_cam::recognise::DEFAULT_SIDE_ALLOWANCE,
                top: artificer_cam::recognise::DEFAULT_TOP_ALLOWANCE,
            },
            stock_mesh: None,
        }
    }
}

impl CamState {
    /// The plan in its card, else the one kept.
    pub(crate) fn study(&self) -> Option<&CamStudy> {
        self.staged.as_ref().or(self.committed.as_ref())
    }

    fn study_mut(&mut self) -> Option<&mut CamStudy> {
        self.staged.as_mut().or(self.committed.as_mut())
    }

    pub(crate) fn rewind(&mut self) {
        self.time = 0.0;
        self.playing = false;
        self.last_tick = None;
    }

    /// Where the tool library lives; `None` keeps the built-in set and
    /// writes nothing.
    pub(crate) fn set_library_path(&mut self, path: Option<PathBuf>) {
        self.library_path = path;
        self.library_loaded = false;
    }

    /// Reads the library file, seeding it with the built-in set on the first
    /// run, so it is there to edit by hand.
    fn ensure_library(&mut self) {
        if self.library_loaded {
            return;
        }
        self.library_loaded = true;
        let Some(path) = self.library_path.clone() else {
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match ToolLibrary::from_json(&text) {
                Ok(library) if !library.tools.is_empty() => {
                    self.library = library;
                    self.library_note = Some(format!("Tool library read from {}", path.display()));
                }
                Ok(_) => {
                    self.library_note = Some(format!(
                        "{} holds no tools; using the built-in library",
                        path.display()
                    ));
                }
                Err(error) => {
                    self.library_note = Some(format!(
                        "{} could not be read ({error}); using the built-in library",
                        path.display()
                    ));
                }
            },
            Err(_) => {
                let seeded = self
                    .library
                    .to_json()
                    .map_err(|error| error.to_string())
                    .and_then(|json| {
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                        }
                        crate::export::atomic_write(&path, json.as_bytes())
                    });
                self.library_note = Some(match seeded {
                    Ok(()) => format!("Tool library seeded at {}", path.display()),
                    Err(error) => format!("Tool library could not be seeded: {error}"),
                });
            }
        }
    }
}

/// Where `tools.json` lives: beside the theme and preferences in the user
/// data folder (ADR 0053), or where `ARTIFICER_TOOLS_PATH` says.
pub(crate) fn tools_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ARTIFICER_TOOLS_PATH").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path));
    }
    crate::user_data::data_directory().map(|data| data.join("tools.json"))
}

/// A material key from the document's material table, as CAM's table.
fn material_from_key(key: &str) -> Option<Material> {
    let key = key.to_ascii_lowercase();
    if key.contains("alumin") {
        Some(Material::Aluminium)
    } else if key.contains("steel") {
        Some(Material::MildSteel)
    } else if key.contains("brass") {
        Some(Material::Brass)
    } else if key.contains("abs") {
        Some(Material::Abs)
    } else {
        None
    }
}

/// Seconds as the card says them.
fn clock(seconds: f64) -> String {
    let total = seconds.round().max(0.0) as u64;
    let (minutes, rest) = (total / 60, total % 60);
    if minutes == 0 {
        format!("{rest} s")
    } else {
        format!("{minutes} min {rest:02} s")
    }
}

/// Runs a plan: posts it, reads the program back, simulates it, and keeps
/// the paths and the stock ghost the viewport draws.
fn finish_study(
    body: BodyId,
    placement: viewport::RigidOccurrenceTransform,
    setup: Setup,
    plan: Plan,
) -> Result<CamStudy, String> {
    let gcode = post(&plan);
    let motions = interpret(&gcode, plan.machine, start_position(&plan))
        .map_err(|error| error.to_string())?;
    let simulation = match &setup {
        Setup::Turned(turned) => {
            let stock = LatheStock::bar(turned.stock.radius, turned.stock.back, turned.stock.front);
            Simulation::Lathe(
                LatheSimulation::run(&plan, motions, stock).map_err(|error| error.to_string())?,
            )
        }
        Setup::Milled(milled) => {
            let origin = milled.origin();
            let smallest = plan
                .tools
                .iter()
                .map(|tool| tool.diameter)
                .fold(f64::INFINITY, f64::min);
            let stock = MillStock::for_tools(
                Point3::new(
                    milled.stock.min.x - origin.x,
                    milled.stock.min.y - origin.y,
                    milled.stock.min.z - origin.z,
                ),
                Point3::new(
                    milled.stock.max.x - origin.x,
                    milled.stock.max.y - origin.y,
                    milled.stock.max.z - origin.z,
                ),
                smallest,
            );
            Simulation::Mill(
                MillSimulation::run(&plan, motions, stock).map_err(|error| error.to_string())?,
            )
        }
        Setup::MillTurn(_) | Setup::Unsupported { .. } => {
            return Err("the setup cannot be planned".to_owned());
        }
    };
    let mut study = CamStudy {
        body,
        setup,
        plan,
        gcode,
        simulation,
        placement,
        paths: Vec::new(),
        stock_ghost: Vec::new(),
    };
    study.paths = study
        .simulation
        .motions()
        .iter()
        .map(|motion| {
            let points = motion
                .sampled(study.plan.machine, 0.05)
                .into_iter()
                .map(|p| study.to_world(p))
                .collect::<Vec<_>>();
            (points, motion.is_cutting())
        })
        .collect();
    study.stock_ghost = match &study.setup {
        Setup::Turned(turned) => {
            let bar = PlanarRegion2 {
                outer: geom::rectangle(
                    Point2::new(0.0, turned.stock.back),
                    Point2::new(turned.stock.radius, turned.stock.front),
                ),
                holes: Vec::new(),
            };
            revolved_triangles(&study, &bar, 48)
        }
        Setup::Milled(milled) => {
            let origin = milled.origin();
            let min = Point3::new(
                milled.stock.min.x - origin.x,
                milled.stock.min.y - origin.y,
                milled.stock.min.z - origin.z,
            );
            let max = Point3::new(
                milled.stock.max.x - origin.x,
                milled.stock.max.y - origin.y,
                milled.stock.max.z - origin.z,
            );
            box_triangles(&study, min, max)
        }
        _ => Vec::new(),
    };
    Ok(study)
}

/// A region of the lathe section revolved about the axis, as triangles.
fn revolved_triangles(
    study: &CamStudy,
    region: &PlanarRegion2,
    segments: usize,
) -> Vec<[Point3; 3]> {
    let mut triangles = Vec::new();
    let step = std::f64::consts::TAU / segments as f64;
    for source in std::iter::once(&region.outer).chain(region.holes.iter()) {
        let polyline = geom::sample_loop(source, 0.05);
        let count = polyline.len();
        for index in 0..count {
            let a = polyline[index];
            let b = polyline[(index + 1) % count];
            if a.x.abs() <= 1.0e-9 && b.x.abs() <= 1.0e-9 {
                continue;
            }
            for k in 0..segments {
                let t0 = step * k as f64;
                let t1 = step * (k + 1) as f64;
                let a0 = study.turned_to_world(a.x, a.y, t0);
                let a1 = study.turned_to_world(a.x, a.y, t1);
                let b0 = study.turned_to_world(b.x, b.y, t0);
                let b1 = study.turned_to_world(b.x, b.y, t1);
                if a.x.abs() > 1.0e-9 {
                    triangles.push([a0, b0, a1]);
                }
                if b.x.abs() > 1.0e-9 {
                    triangles.push([a1, b0, b1]);
                }
            }
        }
    }
    triangles
}

/// An axis-aligned box in work coordinates, as triangles in the world.
fn box_triangles(study: &CamStudy, min: Point3, max: Point3) -> Vec<[Point3; 3]> {
    let corner = |x: bool, y: bool, z: bool| {
        study.to_world(Point3::new(
            if x { max.x } else { min.x },
            if y { max.y } else { min.y },
            if z { max.z } else { min.z },
        ))
    };
    let quads = [
        [
            corner(false, false, true),
            corner(true, false, true),
            corner(true, true, true),
            corner(false, true, true),
        ],
        [
            corner(false, false, false),
            corner(false, true, false),
            corner(true, true, false),
            corner(true, false, false),
        ],
        [
            corner(false, false, false),
            corner(true, false, false),
            corner(true, false, true),
            corner(false, false, true),
        ],
        [
            corner(false, true, false),
            corner(false, true, true),
            corner(true, true, true),
            corner(true, true, false),
        ],
        [
            corner(false, false, false),
            corner(false, false, true),
            corner(false, true, true),
            corner(false, true, false),
        ],
        [
            corner(true, false, false),
            corner(true, true, false),
            corner(true, true, true),
            corner(true, false, true),
        ],
    ];
    let mut triangles = Vec::with_capacity(12);
    for quad in quads {
        triangles.push([quad[0], quad[1], quad[2]]);
        triangles.push([quad[0], quad[2], quad[3]]);
    }
    triangles
}

/// The heightmap as a quad mesh in the world: a flat top per cell and a
/// wall wherever two neighbours differ, coarsened by blocks when the grid
/// is larger than the viewport can take every frame.
fn heightmap_triangles(study: &CamStudy, stock: &MillStock, max_cells: usize) -> Vec<[Point3; 3]> {
    let cells = stock.columns * stock.rows;
    let block = ((cells as f64 / max_cells as f64).sqrt().ceil() as usize).max(1);
    let columns = stock.columns.div_ceil(block);
    let rows = stock.rows.div_ceil(block);
    let mut heights = vec![stock.bottom as f32; columns * rows];
    for row in 0..rows {
        for column in 0..columns {
            let mut lowest = f32::INFINITY;
            for r in row * block..((row + 1) * block).min(stock.rows) {
                for c in column * block..((column + 1) * block).min(stock.columns) {
                    lowest = lowest.min(stock.heights[r * stock.columns + c]);
                }
            }
            heights[row * columns + column] = lowest;
        }
    }
    let cell = stock.cell * block as f64;
    let at = |column: usize, row: usize, z: f64| {
        study.to_world(Point3::new(
            cell.mul_add(column as f64, stock.min.x),
            cell.mul_add(row as f64, stock.min.y),
            z,
        ))
    };
    let mut triangles = Vec::with_capacity(columns * rows * 3);
    for row in 0..rows {
        for column in 0..columns {
            let h = f64::from(heights[row * columns + column]);
            if h <= stock.bottom + 1.0e-9 {
                continue;
            }
            let a = at(column, row, h);
            let b = at(column + 1, row, h);
            let c = at(column + 1, row + 1, h);
            let d = at(column, row + 1, h);
            triangles.push([a, b, c]);
            triangles.push([a, c, d]);
            // Walls to the right and above; the outer edges fall to the
            // bottom.
            let right = if column + 1 < columns {
                f64::from(heights[row * columns + column + 1])
            } else {
                stock.bottom
            };
            if (right - h).abs() > 1.0e-6 {
                let (low, high) = (right.min(h), right.max(h));
                let p0 = at(column + 1, row, low);
                let p1 = at(column + 1, row + 1, low);
                let p2 = at(column + 1, row + 1, high);
                let p3 = at(column + 1, row, high);
                triangles.push([p0, p1, p2]);
                triangles.push([p0, p2, p3]);
            }
            let above = if row + 1 < rows {
                f64::from(heights[(row + 1) * columns + column])
            } else {
                stock.bottom
            };
            if (above - h).abs() > 1.0e-6 {
                let (low, high) = (above.min(h), above.max(h));
                let p0 = at(column, row + 1, low);
                let p1 = at(column + 1, row + 1, low);
                let p2 = at(column + 1, row + 1, high);
                let p3 = at(column, row + 1, high);
                triangles.push([p0, p1, p2]);
                triangles.push([p0, p2, p3]);
            }
            if column == 0 {
                let p0 = at(0, row, stock.bottom);
                let p1 = at(0, row + 1, stock.bottom);
                let p2 = at(0, row + 1, h);
                let p3 = at(0, row, h);
                triangles.push([p0, p1, p2]);
                triangles.push([p0, p2, p3]);
            }
            if row == 0 {
                let p0 = at(column, 0, stock.bottom);
                let p1 = at(column + 1, 0, stock.bottom);
                let p2 = at(column + 1, 0, h);
                let p3 = at(column, 0, h);
                triangles.push([p0, p1, p2]);
                triangles.push([p0, p2, p3]);
            }
        }
    }
    triangles
}

/// A cylinder standing on `tip` along the work `z`, as triangles in the
/// world: the mill's tool.
fn cylinder_triangles(
    study: &CamStudy,
    tip: Point3,
    radius: f64,
    length: f64,
    segments: usize,
) -> Vec<[Point3; 3]> {
    let step = std::f64::consts::TAU / segments as f64;
    let ring = |z: f64, k: usize| {
        let angle = step * k as f64;
        study.to_world(Point3::new(
            radius.mul_add(angle.cos(), tip.x),
            radius.mul_add(angle.sin(), tip.y),
            z,
        ))
    };
    let bottom = study.to_world(tip);
    let top = study.to_world(Point3::new(tip.x, tip.y, tip.z + length));
    let mut triangles = Vec::with_capacity(segments * 4);
    for k in 0..segments {
        let b0 = ring(tip.z, k);
        let b1 = ring(tip.z, k + 1);
        let t0 = ring(tip.z + length, k);
        let t1 = ring(tip.z + length, k + 1);
        triangles.push([b0, b1, t1]);
        triangles.push([b0, t1, t0]);
        triangles.push([bottom, b1, b0]);
        triangles.push([top, t0, t1]);
    }
    triangles
}

/// The lathe tool's region at a position, as a thin prism across the
/// section plane.
fn insert_triangles(
    study: &CamStudy,
    region: &artificer_protocol::PlanarLoop2,
    thickness: f64,
) -> Vec<[Point3; 3]> {
    let polygon = geom::sample_loop(region, 0.05);
    let Setup::Turned(turned) = &study.setup else {
        return Vec::new();
    };
    let tangent = artificer_cam::space::cross(turned.axis.direction, turned.axis.radial);
    let side = |p: Point2, sign: f64| {
        let base = turned.axis.to_world(p.x, p.y);
        study
            .placement
            .transform_point(artificer_cam::space::offset(
                base,
                artificer_cam::space::scale(tangent, sign * thickness / 2.0),
            ))
    };
    let count = polygon.len();
    if count < 3 {
        return Vec::new();
    }
    let mut triangles = Vec::new();
    for index in 1..count - 1 {
        triangles.push([
            side(polygon[0], 1.0),
            side(polygon[index], 1.0),
            side(polygon[index + 1], 1.0),
        ]);
        triangles.push([
            side(polygon[0], -1.0),
            side(polygon[index + 1], -1.0),
            side(polygon[index], -1.0),
        ]);
    }
    for index in 0..count {
        let a = polygon[index];
        let b = polygon[(index + 1) % count];
        triangles.push([side(a, 1.0), side(b, 1.0), side(b, -1.0)]);
        triangles.push([side(a, 1.0), side(b, -1.0), side(a, -1.0)]);
    }
    triangles
}

const STOCK_GHOST: Color32 = Color32::from_rgba_premultiplied(30, 36, 44, 44);
const REMAINING_STOCK: Color32 = Color32::from_rgba_premultiplied(178, 182, 190, 226);
const TOOL: Color32 = Color32::from_rgba_premultiplied(150, 122, 40, 150);
const RAPID: Color32 = Color32::from_rgba_premultiplied(190, 110, 30, 190);
const CUT: Color32 = Color32::from_rgba_premultiplied(30, 150, 210, 210);
const COLLISION: Color32 = Color32::from_rgb(240, 60, 60);
const CURRENT: Color32 = Color32::from_rgb(255, 255, 255);

impl KernelLabApp {
    /// Auto-CAM: plans the active body and stages the plan.
    pub(crate) fn stage_auto_cam(&mut self) {
        if self.pending_operation.is_some()
            || !self.history_is_at_end()
            || self.workbench_mode != WorkbenchMode::Model
        {
            return;
        }
        let Some(index) = self.active_body_index() else {
            self.cam.refusal = Some("Create a body to machine first.".to_owned());
            return;
        };
        let body = &self.bodies[index];
        let body_id = body.id;
        let snapshot = body.body.snapshot.clone();
        if let Some(material) = body.material.as_deref().and_then(material_from_key) {
            self.cam.material = material;
        }
        let placement = self.occurrence_transform_for_body(body_id);
        self.cam.ensure_library();
        match self.build_cam_study(&snapshot, body_id, placement) {
            Ok(study) => {
                let summary = self.cam_summary_line(&study);
                self.cam.staged = Some(study);
                self.cam.committed = None;
                self.cam.refusal = None;
                self.cam.rewind();
                self.cam.stock_mesh = None;
                self.cam.section = CamCardSection::Simulate;
                self.pending_operation = Some(PendingOperation::StageCamPlan);
                self.document_status = Some(format!("Auto-CAM: {summary}"));
            }
            Err(refusal) => {
                self.cam.staged = None;
                self.cam.refusal = Some(refusal.clone());
                self.cam.stock_mesh = None;
                self.document_status = Some(format!("Auto-CAM refused: {refusal}"));
            }
        }
    }

    fn build_cam_study(
        &self,
        snapshot: &Snapshot,
        body: BodyId,
        placement: viewport::RigidOccurrenceTransform,
    ) -> Result<CamStudy, String> {
        let setup = recognise_with(
            snapshot,
            self.cam.edited_turn,
            self.cam.edited_mill,
            WorkOrigin::StockCorner,
        );
        let plan = plan_setup(&setup, &self.cam.library, self.cam.material)
            .map_err(|refusal| refusal.to_string())?;
        finish_study(body, placement, setup, plan)
    }

    fn cam_summary_line(&self, study: &CamStudy) -> String {
        format!(
            "{} · {} operation{} · {} tool{} · {}{}",
            study.plan.setup_name,
            study.plan.operations.len(),
            if study.plan.operations.len() == 1 {
                ""
            } else {
                "s"
            },
            study.plan.tool_changes(),
            if study.plan.tool_changes() == 1 {
                ""
            } else {
                "s"
            },
            clock(study.total_seconds()),
            match study.simulation.collisions().len() {
                0 => String::new(),
                n => format!(" · {n} collision{}", if n == 1 { "" } else { "s" }),
            }
        )
    }

    /// Replans the staged or kept study with the card's current allowances
    /// and material; the body is re-read from the document.
    fn replan_cam(&mut self) {
        let Some((body, placement)) = self.cam.study().map(|study| (study.body, study.placement))
        else {
            return;
        };
        let Some(snapshot) = self
            .bodies
            .iter()
            .find(|held| held.id == body)
            .map(|held| held.body.snapshot.clone())
        else {
            self.cam.refusal = Some("the planned body is no longer in the document".to_owned());
            return;
        };
        match self.build_cam_study(&snapshot, body, placement) {
            Ok(study) => {
                let summary = self.cam_summary_line(&study);
                if self.cam.staged.is_some() {
                    self.cam.staged = Some(study);
                } else {
                    self.cam.committed = Some(study);
                }
                self.cam.rewind();
                self.cam.stock_mesh = None;
                self.document_status = Some(format!("Replanned: {summary}"));
            }
            Err(refusal) => {
                self.document_status = Some(format!("Replan refused: {refusal}"));
            }
        }
    }

    /// Re-posts and re-simulates the study after its operations were
    /// reordered.
    fn rerun_cam_study(&mut self) {
        let Some(study) = self.cam.study_mut() else {
            return;
        };
        let body = study.body;
        let placement = study.placement;
        let setup = study.setup.clone();
        let plan = study.plan.clone();
        match finish_study(body, placement, setup, plan) {
            Ok(rerun) => {
                let summary = self.cam_summary_line(&rerun);
                if self.cam.staged.is_some() {
                    self.cam.staged = Some(rerun);
                } else {
                    self.cam.committed = Some(rerun);
                }
                self.cam.rewind();
                self.cam.stock_mesh = None;
                self.document_status = Some(format!("Reordered: {summary}"));
            }
            Err(error) => {
                self.document_status = Some(format!("Reorder refused: {error}"));
            }
        }
    }

    /// Confirm: the staged plan is kept as document data.
    pub(crate) fn commit_cam_plan(&mut self) {
        if let Some(study) = self.cam.staged.take() {
            let summary = self.cam_summary_line(&study);
            self.cam.committed = Some(study);
            self.document_status = Some(format!("CAM plan kept: {summary}"));
        }
        self.pending_operation = None;
    }

    /// Cancel: the staged plan is dropped.
    pub(crate) fn cancel_cam_plan(&mut self) {
        self.cam.staged = None;
        self.cam.stock_mesh = None;
        self.cam.rewind();
        self.pending_operation = None;
        self.document_status = Some("Auto-CAM plan dropped".to_owned());
    }

    /// Whether the card should describe the kept plan, or the refusal: only
    /// on the CAM tab, and only when no operation is pending.
    pub(crate) fn cam_card_wanted(&self) -> bool {
        self.pending_operation.is_none()
            && self.workbench_mode == WorkbenchMode::Model
            && self.active_ribbon_tab() == crate::commands::RibbonTab::Cam
            && (self.cam.committed.is_some() || self.cam.refusal.is_some())
    }

    pub(crate) fn toggle_cam_playback(&mut self, context: &egui::Context) {
        if self.cam.study().is_none() {
            return;
        }
        if !self.cam.playing
            && self
                .cam
                .study()
                .is_some_and(|study| self.cam.time >= study.total_seconds())
        {
            self.cam.time = 0.0;
        }
        self.cam.playing = !self.cam.playing;
        self.cam.last_tick = None;
        context.request_repaint();
    }

    /// Moves the simulation clock while it plays.
    pub(crate) fn advance_cam_playback(&mut self, context: &egui::Context) {
        if !self.cam.playing {
            self.cam.last_tick = None;
            return;
        }
        let Some(total) = self.cam.study().map(CamStudy::total_seconds) else {
            self.cam.playing = false;
            return;
        };
        let now = context.input(|input| input.time);
        if let Some(previous) = self.cam.last_tick {
            self.cam.time += (now - previous).max(0.0) * self.cam.speed;
        }
        self.cam.last_tick = Some(now);
        if self.cam.time >= total {
            self.cam.time = total;
            self.cam.playing = false;
        }
        context.request_repaint();
    }

    /// Opens the export dialog on the kept plan's G-code.
    pub(crate) fn export_cam_gcode(&mut self) {
        if self.cam.committed.is_none() {
            self.document_status =
                Some("Confirm the Auto-CAM plan before exporting its G-code".to_owned());
            return;
        }
        self.open_export_dialog(ExportSubject::CamGcode);
    }

    /// Writes the kept plan's G-code to `path`.
    pub fn export_cam_gcode_to(&self, path: &Path) -> Result<(), String> {
        let study = self
            .cam
            .committed
            .as_ref()
            .ok_or_else(|| "no CAM plan has been kept".to_owned())?;
        crate::export::atomic_write(path, study.gcode.as_bytes())
    }

    /// The stock, the tool and the toolpath at the simulation clock, for the
    /// viewport; nothing off the CAM tab.
    pub(crate) fn cam_overlay(&mut self) -> Option<viewport::SceneOverlay> {
        if self.active_ribbon_tab() != crate::commands::RibbonTab::Cam
            || self.workbench_mode != WorkbenchMode::Model
        {
            return None;
        }
        let time = self.cam.time;
        let study = self.cam.study()?;
        let (index, fraction) = study.simulation.at(time).unwrap_or((0, 0.0));
        let staged = self.cam.staged.is_some();
        // The remaining stock, rebuilt when the clock crosses a motion.
        let stock_current = self
            .cam
            .stock_mesh
            .as_ref()
            .is_some_and(|(at, was_staged, _)| *at == index && *was_staged == staged);
        if !stock_current {
            let triangles = match &study.simulation {
                Simulation::Lathe(simulation) => {
                    let stock = if index == 0 && fraction <= 0.0 {
                        &simulation.initial
                    } else {
                        simulation.stock_after(index)
                    };
                    stock
                        .regions
                        .iter()
                        .flat_map(|region| revolved_triangles(study, region, 48))
                        .collect::<Vec<_>>()
                }
                Simulation::Mill(simulation) => {
                    let stock = if index == 0 && fraction <= 0.0 {
                        simulation.initial.clone()
                    } else {
                        simulation.stock_after(index)
                    };
                    heightmap_triangles(study, &stock, 9000)
                }
            };
            self.cam.stock_mesh = Some((index, staged, triangles));
        }
        let study = self.cam.study()?;
        let mut overlay = viewport::SceneOverlay::default();
        overlay.meshes.push(viewport::OverlayMesh {
            triangles: study.stock_ghost.clone(),
            color: STOCK_GHOST,
            shaded: true,
        });
        if let Some((_, _, triangles)) = &self.cam.stock_mesh {
            overlay.meshes.push(viewport::OverlayMesh {
                triangles: triangles.clone(),
                color: REMAINING_STOCK,
                shaded: true,
            });
        }
        // The tool.
        let motions = study.simulation.motions();
        if let Some(motion) = motions.get(index)
            && let Some(tool) = study.plan.tool(motion.tool)
        {
            let position = position_along(motion, study.plan.machine, fraction);
            let triangles = match study.plan.machine {
                Machine::Mill => {
                    cylinder_triangles(study, position, tool.radius(), tool.flute_length + 12.0, 24)
                }
                Machine::Lathe => insert_triangles(study, &lathe_tool_at(tool, position), 3.0),
            };
            overlay.meshes.push(viewport::OverlayMesh {
                triangles,
                color: TOOL,
                shaded: true,
            });
        }
        // The toolpath: rapids and cuts in two colours, collisions red, the
        // motion in progress white.
        let collided = study
            .simulation
            .collisions()
            .iter()
            .map(|collision| collision.motion)
            .collect::<std::collections::BTreeSet<_>>();
        for (motion_index, (points, cutting)) in study.paths.iter().enumerate() {
            let (color, width) = if collided.contains(&motion_index) {
                (COLLISION, 2.2)
            } else if motion_index == index {
                (CURRENT, 2.0)
            } else if *cutting {
                (CUT, 1.3)
            } else {
                (RAPID, 1.0)
            };
            overlay.polylines.push(viewport::OverlayPolyline {
                points: points.clone(),
                color,
                width,
            });
        }
        Some(overlay)
    }

    /// The card: the setup, the operations, or the simulation.
    pub(crate) fn cam_controls(&mut self, ui: &mut egui::Ui) {
        if let Some(note) = self.cam.library_note.clone() {
            ui.label(RichText::new(note).small().color(theme::muted()));
        }
        let Some(study) = self.cam.study() else {
            match self.cam.refusal.clone() {
                Some(refusal) => {
                    status_line(ui, "Refused by name", theme::bad());
                    ui.label(RichText::new(refusal).small().color(theme::text()));
                }
                None => {
                    ui.label(
                        RichText::new("Press Auto-CAM to plan the active body.")
                            .small()
                            .color(theme::muted()),
                    );
                }
            }
            return;
        };
        let summary = self.cam_summary_line(study);
        let collisions = study.simulation.collisions().len();
        let kept = self.cam.staged.is_none();
        status_line(
            ui,
            &summary,
            if collisions > 0 {
                theme::bad()
            } else if kept {
                theme::good()
            } else {
                theme::accent()
            },
        );
        ui.label(
            RichText::new(if kept {
                "Kept with the document"
            } else {
                "Staged · confirm to keep"
            })
            .small()
            .color(theme::muted()),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for (section, label) in [
                (CamCardSection::Setup, "Setup"),
                (CamCardSection::Operations, "Operations"),
                (CamCardSection::Simulate, "Simulate"),
            ] {
                let response = ui.selectable_label(self.cam.section == section, label);
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::Button,
                        true,
                        self.cam.section == section,
                        format!("Show CAM {label}"),
                    )
                });
                if response.clicked() {
                    self.cam.section = section;
                }
            }
        });
        ui.separator();
        match self.cam.section {
            CamCardSection::Setup => self.cam_setup_section(ui),
            CamCardSection::Operations => self.cam_operations_section(ui),
            CamCardSection::Simulate => self.cam_simulate_section(ui),
        }
    }

    fn cam_setup_section(&mut self, ui: &mut egui::Ui) {
        let Some(study) = self.cam.study() else {
            return;
        };
        let lines = match &study.setup {
            Setup::Turned(turned) => vec![
                format!(
                    "Turned on a lathe, along world {}",
                    axis_words(turned.axis.direction)
                ),
                format!(
                    "Bar Ø{:.1} × {:.1} mm, {:.1} mm long part",
                    turned.stock.radius * 2.0,
                    turned.stock.front - turned.stock.back,
                    turned.length
                ),
                "Work origin on the axis at the front face".to_owned(),
                format!(
                    "Faces read: {}{}",
                    turned.faces.len(),
                    if turned.through_bore {
                        " · bored through"
                    } else {
                        ""
                    }
                ),
            ],
            Setup::Milled(milled) => vec![
                format!("Milled 2.5D, spindle along world {}", milled.axis_label),
                format!(
                    "Stock {:.1} × {:.1} × {:.1} mm",
                    milled.stock.max.x - milled.stock.min.x,
                    milled.stock.max.y - milled.stock.min.y,
                    milled.stock.max.z - milled.stock.min.z
                ),
                format!("Work origin at the {}", milled.work_origin.label()),
                format!(
                    "{} level{} from {:.2} to {:.2}",
                    milled.levels.len(),
                    if milled.levels.len() == 1 { "" } else { "s" },
                    milled.top,
                    milled.bottom
                ),
            ],
            _ => Vec::new(),
        };
        let notes = study.plan.notes.clone();
        let lathe = matches!(study.setup, Setup::Turned(_));
        for line in lines {
            ui.label(RichText::new(line).small().color(theme::text()));
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Material").small().color(theme::muted()));
        let mut chosen = self.cam.material;
        egui::ComboBox::from_id_salt("cam_material")
            .selected_text(chosen.label())
            .width(ui.available_width() - 8.0)
            .show_ui(ui, |ui| {
                for material in Material::ALL {
                    ui.selectable_value(&mut chosen, material, material.label());
                }
            });
        let mut replan = chosen != self.cam.material;
        self.cam.material = chosen;
        ui.label(
            RichText::new("Allowances (mm)")
                .small()
                .color(theme::muted()),
        );
        ui.horizontal(|ui| {
            if lathe {
                ui.label(RichText::new("radial").small());
                ui.add(
                    egui::DragValue::new(&mut self.cam.edited_turn.radial)
                        .range(0.2..=20.0)
                        .speed(0.1),
                );
                ui.label(RichText::new("facing").small());
                ui.add(
                    egui::DragValue::new(&mut self.cam.edited_turn.facing)
                        .range(0.2..=20.0)
                        .speed(0.1),
                );
            } else {
                ui.label(RichText::new("side").small());
                ui.add(
                    egui::DragValue::new(&mut self.cam.edited_mill.side)
                        .range(0.0..=50.0)
                        .speed(0.1),
                );
                ui.label(RichText::new("top").small());
                ui.add(
                    egui::DragValue::new(&mut self.cam.edited_mill.top)
                        .range(0.0..=50.0)
                        .speed(0.1),
                );
            }
        });
        let button = ui.small_button("Replan with these");
        button.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Replan CAM")
        });
        if button.clicked() {
            replan = true;
        }
        if replan {
            self.replan_cam();
        }
        for note in notes {
            ui.label(RichText::new(note).small().color(theme::muted()));
        }
    }

    fn cam_operations_section(&mut self, ui: &mut egui::Ui) {
        let Some(study) = self.cam.study() else {
            return;
        };
        let motions = study.simulation.motions();
        let ends = match &study.simulation {
            Simulation::Lathe(simulation) => &simulation.ends,
            Simulation::Mill(simulation) => &simulation.ends,
        };
        // Each operation's time: from its first motion's start to its last
        // motion's end.
        let mut rows = Vec::new();
        for (index, operation) in study.plan.operations.iter().enumerate() {
            let first = motions.iter().position(|motion| motion.operation == index);
            let last = motions.iter().rposition(|motion| motion.operation == index);
            let seconds = match (first, last) {
                (Some(first), Some(last)) => {
                    let start = if first == 0 { 0.0 } else { ends[first - 1] };
                    ends[last] - start
                }
                _ => 0.0,
            };
            let tool = study.plan.tool(operation.tool);
            let spindle = match operation.spindle {
                artificer_cam::Spindle::Rpm(rpm) => format!("S{rpm:.0}"),
                artificer_cam::Spindle::SurfaceSpeed {
                    metres_per_minute, ..
                } => format!("G96 S{metres_per_minute:.0}"),
            };
            let feed = match operation.feed {
                artificer_cam::FeedRate::PerMinute(rate) => format!("F{rate:.0}/min"),
                artificer_cam::FeedRate::PerRevolution(rate) => format!("F{rate:.2}/rev"),
            };
            rows.push((
                format!("{}. {}", index + 1, operation.name),
                format!(
                    "T{} {} · {spindle} · {feed} · {}",
                    operation.tool,
                    tool.map_or("", |tool| tool.name.as_str()),
                    clock(seconds)
                ),
                operation.notes.clone(),
            ));
        }
        let count = rows.len();
        let mut action: Option<(usize, bool)> = None;
        for (index, (title, detail, notes)) in rows.into_iter().enumerate() {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(title).small().strong().color(theme::text()));
                    ui.label(RichText::new(detail).small().color(theme::muted()));
                    for note in notes {
                        ui.label(RichText::new(note).small().color(theme::muted()));
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    let later = ui.add_enabled(index + 1 < count, egui::Button::new("▼").small());
                    later.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            index + 1 < count,
                            format!("Move operation {} later", index + 1),
                        )
                    });
                    if later.clicked() {
                        action = Some((index, false));
                    }
                    let earlier = ui.add_enabled(index > 0, egui::Button::new("▲").small());
                    earlier.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            index > 0,
                            format!("Move operation {} earlier", index + 1),
                        )
                    });
                    if earlier.clicked() {
                        action = Some((index, true));
                    }
                });
            });
        }
        if let Some((index, earlier)) = action {
            self.move_cam_operation(index, earlier);
        }
    }

    fn cam_simulate_section(&mut self, ui: &mut egui::Ui) {
        let Some(study) = self.cam.study() else {
            return;
        };
        let total = study.total_seconds();
        let (index, _) = study.simulation.at(self.cam.time).unwrap_or((0, 0.0));
        let current = study
            .simulation
            .motions()
            .get(index)
            .and_then(|motion| study.plan.operations.get(motion.operation))
            .map(|operation| operation.name.clone())
            .unwrap_or_default();
        let collisions = study
            .simulation
            .collisions()
            .iter()
            .map(|collision| format!("line {}: {}", collision.line, collision.detail))
            .collect::<Vec<_>>();
        let motion_count = study.simulation.motions().len();
        let kept = self.cam.staged.is_none();
        let mut time = self.cam.time;
        let slider = ui.add(
            egui::Slider::new(&mut time, 0.0..=total.max(1.0e-6))
                .show_value(false)
                .text("Timeline"),
        );
        if slider.changed() {
            self.cam.time = time.clamp(0.0, total);
            self.cam.playing = false;
        }
        ui.label(
            RichText::new(format!(
                "{} / {} · motion {} of {motion_count}",
                clock(self.cam.time),
                clock(total),
                index + 1
            ))
            .small()
            .monospace()
            .color(theme::text()),
        );
        if !current.is_empty() {
            ui.label(RichText::new(current).small().color(theme::muted()));
        }
        ui.horizontal(|ui| {
            let play = ui.small_button(if self.cam.playing { "Pause" } else { "Play" });
            play.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    if self.cam.playing {
                        "Pause simulation"
                    } else {
                        "Play simulation"
                    },
                )
            });
            if play.clicked() {
                let context = ui.ctx().clone();
                self.toggle_cam_playback(&context);
            }
            let rewind = ui.small_button("Rewind");
            rewind.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Rewind to start")
            });
            if rewind.clicked() {
                self.cam.rewind();
            }
            for speed in [1.0, 4.0, 16.0, 64.0] {
                let label = format!("{speed:.0}×");
                if ui
                    .selectable_label((self.cam.speed - speed).abs() < 1.0e-9, &label)
                    .clicked()
                {
                    self.cam.speed = speed;
                }
            }
        });
        if collisions.is_empty() {
            ui.label(
                RichText::new("No collisions: every rapid stays clear of the stock.")
                    .small()
                    .color(theme::good()),
            );
        } else {
            status_line(
                ui,
                &format!(
                    "{} collision{}",
                    collisions.len(),
                    if collisions.len() == 1 { "" } else { "s" }
                ),
                theme::bad(),
            );
            for collision in collisions.iter().take(12) {
                ui.label(RichText::new(collision).small().color(theme::bad()));
            }
        }
        ui.add_space(4.0);
        let export = ui.add_enabled(kept, egui::Button::new("Export G-code…").small());
        export.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, kept, "Export G-code file")
        });
        if export.clicked() {
            self.export_cam_gcode();
        }
        if !kept {
            ui.label(
                RichText::new("Confirm the plan to export its G-code.")
                    .small()
                    .color(theme::muted()),
            );
        }
    }

    // ---- headless accessors ------------------------------------------------

    /// Machine, setup name, operation count, tool changes, total seconds
    /// and collision count of the plan in the card or kept.
    #[must_use]
    pub fn cam_plan_summary(&self) -> Option<CamPlanSummary> {
        self.cam.study().map(|study| CamPlanSummary {
            machine: study.plan.machine.label(),
            setup: study.setup.kind(),
            operations: study.plan.operations.len(),
            tool_changes: study.plan.tool_changes(),
            total_seconds: study.total_seconds(),
            collisions: study.simulation.collisions().len(),
            motions: study.simulation.motions().len(),
        })
    }

    /// The operations of the plan in the card or kept, in order.
    #[must_use]
    pub fn cam_operation_names(&self) -> Vec<String> {
        self.cam
            .study()
            .map(|study| {
                study
                    .plan
                    .operations
                    .iter()
                    .map(|operation| operation.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Moves an operation one place earlier or later, then re-posts and
    /// re-simulates the plan.
    pub fn move_cam_operation(&mut self, index: usize, earlier: bool) {
        let Some(study) = self.cam.study_mut() else {
            return;
        };
        let count = study.plan.operations.len();
        let target = if earlier {
            index.checked_sub(1)
        } else {
            (index + 1 < count).then_some(index + 1)
        };
        let Some(target) = target else {
            return;
        };
        if index >= count {
            return;
        }
        study.plan.operations.swap(index, target);
        let mut tools = Vec::new();
        for operation in &study.plan.operations {
            if !tools
                .iter()
                .any(|tool: &artificer_cam::Tool| tool.number == operation.tool)
                && let Some(tool) = study.plan.tool(operation.tool).cloned()
            {
                tools.push(tool);
            }
        }
        study.plan.tools = tools;
        self.rerun_cam_study();
    }

    /// Why the last Auto-CAM planned nothing.
    #[must_use]
    pub fn cam_refusal(&self) -> Option<String> {
        self.cam.refusal.clone()
    }

    #[must_use]
    pub const fn cam_time(&self) -> f64 {
        self.cam.time
    }

    /// Scrubs the simulation clock, as the timeline slider does.
    pub fn set_cam_time(&mut self, seconds: f64) {
        let total = self.cam.study().map_or(0.0, CamStudy::total_seconds);
        self.cam.time = seconds.clamp(0.0, total);
        self.cam.playing = false;
    }

    #[must_use]
    pub const fn cam_playing(&self) -> bool {
        self.cam.playing
    }

    pub fn set_cam_playing(&mut self, playing: bool) {
        self.cam.playing = playing && self.cam.study().is_some();
        self.cam.last_tick = None;
    }

    pub fn set_cam_speed(&mut self, speed: f64) {
        if speed.is_finite() && speed > 0.0 {
            self.cam.speed = speed;
        }
    }

    #[must_use]
    pub const fn cam_is_committed(&self) -> bool {
        self.cam.committed.is_some()
    }

    /// Where the tool library is read from and seeded; `None` keeps the
    /// built-in set and writes nothing.
    pub fn set_cam_tools_path(&mut self, path: Option<PathBuf>) {
        self.cam.set_library_path(path);
    }

    #[must_use]
    pub fn cam_tool_count(&self) -> usize {
        self.cam.library.tools.len()
    }

    /// The tool in the tool position at the clock, in the world: its tip.
    #[must_use]
    pub fn cam_tool_position(&self) -> Option<Point3> {
        let study = self.cam.study()?;
        let (index, fraction) = study.simulation.at(self.cam.time)?;
        let motion = study.simulation.motions().get(index)?;
        Some(study.to_world(position_along(motion, study.plan.machine, fraction)))
    }

    /// How many triangles the viewport overlay would draw now, for tests.
    pub fn cam_overlay_triangle_count(&mut self) -> usize {
        self.cam_overlay()
            .map(|overlay| overlay.meshes.iter().map(|mesh| mesh.triangles.len()).sum())
            .unwrap_or(0)
    }

    /// The G-code of the plan in the card or kept.
    #[must_use]
    pub fn cam_gcode(&self) -> Option<String> {
        self.cam.study().map(|study| study.gcode.clone())
    }
}

/// What a headless test reads of a plan.
#[derive(Clone, Debug, PartialEq)]
pub struct CamPlanSummary {
    pub machine: &'static str,
    pub setup: &'static str,
    pub operations: usize,
    pub tool_changes: usize,
    pub total_seconds: f64,
    pub collisions: usize,
    pub motions: usize,
}

fn axis_words(direction: artificer_protocol::Vector3) -> String {
    let named = [
        ("+X", (1.0, 0.0, 0.0)),
        ("-X", (-1.0, 0.0, 0.0)),
        ("+Y", (0.0, 1.0, 0.0)),
        ("-Y", (0.0, -1.0, 0.0)),
        ("+Z", (0.0, 0.0, 1.0)),
        ("-Z", (0.0, 0.0, -1.0)),
    ];
    for (name, (x, y, z)) in named {
        if (direction.x - x).abs() < 1.0e-9
            && (direction.y - y).abs() < 1.0e-9
            && (direction.z - z).abs() < 1.0e-9
        {
            return name.to_owned();
        }
    }
    format!(
        "({:.2}, {:.2}, {:.2})",
        direction.x, direction.y, direction.z
    )
}
