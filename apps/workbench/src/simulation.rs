//! The Simulation tab (ADR 0058): a static structural study on the active
//! body, drawn on the part itself.
//!
//! This module stages studies and shows their results. It never executes
//! the kernel or mutates the document: the study reads an immutable
//! snapshot through the kernel's queries (`voxelise`, `describe_faces`,
//! `debug_scene`), runs the simulation crate off the UI thread, and paints
//! what came back over the body's own facets — a stress colour through the
//! same surface-field machinery the clearance heat map uses, and a
//! deformation by displacing the tessellation's vertices before they are
//! uploaded.
//!
//! ## Honesty on the card
//!
//! Every study here is an approximation and the card says so: how many
//! voxels it ran on, that a coarse voxel mesh is stiffer than the part,
//! what the solver's residual was, and — after a rerun at the next
//! resolution — how much the answer moved. The result carries
//! [`Tier::Approximate`].

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use artificer_compute::{JobError, JobHandle, JobPriority};
use artificer_kernel::{CancellationToken, DebugScene, NativeKernel, Snapshot};
use artificer_model::BodyId;
use artificer_protocol::{EntityRef, Point3, SnapshotId, Tier};
use artificer_sim::{
    FieldSampler, Load, MATERIALS, Material, Progress, Resolution, StructuralError,
    StructuralResult, StructuralStudy, Support, VoxelMesh, material_by_key, solve_static,
};
use egui::RichText;

use crate::{KernelLabApp, theme, viewport};

mod motion;
mod thermal;
mod topology;

pub use motion::{MeasuredTimeline, MotionState, MotionSummary};
pub use thermal::{ThermalOutcome, ThermalSetup, ThermalState, ThermalSummary};
pub use topology::{
    TopologyLive, TopologyOutcome, TopologySetup, TopologyState, TopologySummary, density_surface,
};

/// Which study the card is showing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StudyKind {
    Structural,
    Thermal,
    /// Experimental.
    Topology,
    Motion,
}

/// The direction a face force acts along, as the card offers it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadDirection {
    NegativeZ,
    PositiveZ,
    NegativeX,
    PositiveX,
    NegativeY,
    PositiveY,
}

impl LoadDirection {
    pub const ALL: [Self; 6] = [
        Self::NegativeZ,
        Self::PositiveZ,
        Self::NegativeX,
        Self::PositiveX,
        Self::NegativeY,
        Self::PositiveY,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::NegativeZ => "−Z (down)",
            Self::PositiveZ => "+Z (up)",
            Self::NegativeX => "−X",
            Self::PositiveX => "+X",
            Self::NegativeY => "−Y",
            Self::PositiveY => "+Y",
        }
    }

    pub const fn unit(self) -> [f64; 3] {
        match self {
            Self::NegativeZ => [0.0, 0.0, -1.0],
            Self::PositiveZ => [0.0, 0.0, 1.0],
            Self::NegativeX => [-1.0, 0.0, 0.0],
            Self::PositiveX => [1.0, 0.0, 0.0],
            Self::NegativeY => [0.0, -1.0, 0.0],
            Self::PositiveY => [0.0, 1.0, 0.0],
        }
    }
}

/// One face condition as the card lists it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FaceCondition {
    Fixed,
    Force {
        newtons: f64,
        direction: LoadDirection,
    },
    Pressure {
        megapascals: f64,
    },
}

impl FaceCondition {
    fn describe(self) -> String {
        match self {
            Self::Fixed => "fixed".to_owned(),
            Self::Force { newtons, direction } => {
                format!("{newtons:.1} N {}", direction.label())
            }
            Self::Pressure { megapascals } => format!("{megapascals:.2} MPa pressure"),
        }
    }
}

/// A structural study as the user sets it up on the card.
#[derive(Clone, Debug, PartialEq)]
pub struct StructuralSetup {
    /// The body the study runs on.
    pub body: Option<BodyId>,
    pub faces: Vec<(EntityRef, FaceCondition)>,
    pub material_key: String,
    pub resolution: Resolution,
    pub gravity: bool,
    /// The magnitude and direction the next "Load selected faces" applies.
    pub force_newtons: f64,
    pub force_direction: LoadDirection,
    /// The pressure the next "Press selected faces" applies.
    pub pressure_megapascals: f64,
}

impl Default for StructuralSetup {
    fn default() -> Self {
        Self {
            body: None,
            faces: Vec::new(),
            material_key: "aluminium-6061".to_owned(),
            resolution: Resolution::Coarse,
            gravity: false,
            force_newtons: 100.0,
            force_direction: LoadDirection::NegativeZ,
            pressure_megapascals: 1.0,
        }
    }
}

impl StructuralSetup {
    fn material(&self) -> Material {
        material_by_key(&self.material_key).unwrap_or(MATERIALS[0])
    }

    fn fixed_count(&self) -> usize {
        self.faces
            .iter()
            .filter(|(_, condition)| *condition == FaceCondition::Fixed)
            .count()
    }

    fn load_count(&self) -> usize {
        self.faces.len() - self.fixed_count() + usize::from(self.gravity)
    }

    /// The study the simulation crate runs.
    fn study(&self) -> StructuralStudy {
        let mut study = StructuralStudy::new(self.material());
        for (face, condition) in &self.faces {
            match *condition {
                FaceCondition::Fixed => study.supports.push(Support::Fixed { face: *face }),
                FaceCondition::Force { newtons, direction } => {
                    study.loads.push(Load::Force {
                        face: *face,
                        newtons: direction.unit().map(|component| component * newtons),
                    });
                }
                FaceCondition::Pressure { megapascals } => study.loads.push(Load::Pressure {
                    face: *face,
                    megapascals,
                }),
            }
        }
        if self.gravity {
            study.loads.push(Load::Gravity {
                direction: [0.0, 0.0, -1.0],
            });
        }
        study
    }
}

/// What one solve came back with, ready to draw.
#[derive(Clone, Debug)]
pub struct StructuralOutcome {
    pub body: BodyId,
    pub snapshot: SnapshotId,
    pub resolution: Resolution,
    pub material: Material,
    pub result: StructuralResult,
    /// Von Mises at every facet corner of the body's display scene, in
    /// megapascals, in scene order.
    pub vertex_stress: Vec<f32>,
    /// The displacement at every facet corner, in millimetres.
    pub vertex_displacement: Vec<[f32; 3]>,
    /// The displacement at every display edge endpoint and vertex point.
    pub edge_displacement: Vec<[[f32; 3]; 2]>,
    pub point_displacement: Vec<[f32; 3]>,
    pub voxel_count: usize,
    pub cell: f64,
}

/// A solve running off the UI thread.
struct RunningSolve {
    job: JobHandle<Result<StructuralOutcome, StructuralError>>,
    cancellation: CancellationToken,
    progress: Arc<Mutex<Progress>>,
    voxels: Arc<std::sync::atomic::AtomicUsize>,
    resolution: Resolution,
}

/// What one picture was built from, so it is rebuilt only when that
/// changes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DisplayKey {
    Structural {
        body: BodyId,
        snapshot: SnapshotId,
        exaggeration_bits: u64,
        show_field: bool,
    },
    Thermal {
        body: BodyId,
        snapshot: SnapshotId,
        show_field: bool,
    },
    Topology {
        body: BodyId,
        iteration: usize,
        voxels: usize,
        threshold_bits: u64,
    },
}

/// What the viewport is handed: the scene to draw for the studied body and
/// the colours over it.
#[derive(Clone, Debug)]
pub struct SimulationDisplay {
    pub key: DisplayKey,
    pub body: BodyId,
    pub scene: DebugScene,
    pub field: Vec<f32>,
    pub palette: viewport::HeatPalette,
    pub legend: Vec<viewport::HeatBand>,
    pub epoch: u64,
    pub show_field: bool,
    pub exaggeration: f64,
}

/// The tab's whole state.
#[derive(Default)]
pub struct SimulationState {
    pub open: Option<StudyKind>,
    pub structural: StructuralSetup,
    running: Option<RunningSolve>,
    pub outcome: Option<StructuralOutcome>,
    /// The result at the resolution before the last rerun, for the
    /// convergence hint.
    previous: Option<(Resolution, StructuralResult)>,
    pub show_stress: bool,
    /// How many times the drawn deformation is exaggerated; zero draws the
    /// part as modelled.
    pub exaggeration: f64,
    pub thermal: ThermalState,
    pub topology: TopologyState,
    pub motion: MotionState,
    display: Option<SimulationDisplay>,
    epoch: u64,
    /// The last refusal or completion, for the card and the tests.
    pub message: Option<String>,
}

impl SimulationState {
    /// Whether the card has anything to show.
    #[must_use]
    pub fn is_showing(&self) -> bool {
        self.open.is_some()
    }

    /// Whether any study is solving right now.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.is_some()
            || self.thermal.running.is_some()
            || self.topology.running.is_some()
            || self.motion.running.is_some()
    }

    /// Lifts the display out for the duration of a frame's drawing, the way
    /// the clearance heat map is.
    pub fn take_display(&mut self) -> Option<SimulationDisplay> {
        self.display.take()
    }

    pub fn restore_display(&mut self, display: Option<SimulationDisplay>) {
        if self.display.is_none() {
            self.display = display;
        }
    }

    /// Forgets every study and its picture.
    pub fn dismiss(&mut self) {
        if let Some(running) = self.running.take() {
            running.cancellation.cancel();
        }
        self.thermal.cancel();
        self.topology.cancel();
        self.motion.cancel();
        self.open = None;
        self.outcome = None;
        self.previous = None;
        self.thermal.outcome = None;
        self.topology.outcome = None;
        self.topology.live = None;
        self.motion.timeline = None;
        self.display = None;
        self.message = None;
    }

    /// What the open study wants drawn over its body, or `None` for the
    /// body as modelled.
    fn display_key(&self) -> Option<DisplayKey> {
        match self.open? {
            StudyKind::Structural => {
                let outcome = self.outcome.as_ref()?;
                Some(DisplayKey::Structural {
                    body: outcome.body,
                    snapshot: outcome.snapshot,
                    exaggeration_bits: self.exaggeration.to_bits(),
                    show_field: self.show_stress,
                })
            }
            StudyKind::Thermal => {
                let outcome = self.thermal.outcome.as_ref()?;
                Some(DisplayKey::Thermal {
                    body: outcome.body,
                    snapshot: outcome.snapshot,
                    show_field: self.thermal.show_temperature,
                })
            }
            StudyKind::Topology => {
                let live = self.topology.live.as_ref()?;
                Some(DisplayKey::Topology {
                    body: self.topology.body?,
                    iteration: live.iteration,
                    voxels: live.densities.len(),
                    threshold_bits: self.topology.setup.threshold.to_bits(),
                })
            }
            StudyKind::Motion => None,
        }
    }

    /// Rebuilds the drawn scene when what it is built from changed.
    fn refresh_display(&mut self, scenes: impl Fn(BodyId) -> Option<DebugScene>) {
        let Some(key) = self.display_key() else {
            self.display = None;
            return;
        };
        if self
            .display
            .as_ref()
            .is_some_and(|display| display.key == key)
        {
            return;
        }
        self.epoch += 1;
        let epoch = self.epoch;
        self.display = match key {
            DisplayKey::Structural { body, .. } => {
                let outcome = self.outcome.as_ref().expect("the key names an outcome");
                let Some(mut scene) = scenes(body) else {
                    self.display = None;
                    return;
                };
                if self.exaggeration > 0.0 {
                    displace_scene(&mut scene, outcome, self.exaggeration);
                }
                // The GPU cache keys on the scene's snapshot id and the
                // field's epoch; a fresh id per rebuild is what makes it
                // re-upload.
                scene.snapshot = SnapshotId::new(display_id_bytes(outcome.snapshot, epoch));
                scene.semantic_digest = display_digest(outcome.snapshot, epoch);
                let peak = outcome.result.peak_node_von_mises.max(1.0e-6) as f32;
                let palette = viewport::HeatPalette::Gradient {
                    near: 0.0,
                    far: peak,
                };
                // The palette paints `near` red and `far` blue; stress is
                // stored as its distance below the peak so the peak reads
                // red.
                let field = outcome
                    .vertex_stress
                    .iter()
                    .map(|stress| (peak - stress).max(0.0))
                    .collect::<Vec<_>>();
                Some(SimulationDisplay {
                    key,
                    body,
                    scene,
                    field,
                    palette,
                    legend: scale_legend(palette, peak, 0.0, "MPa"),
                    epoch,
                    show_field: self.show_stress,
                    exaggeration: self.exaggeration,
                })
            }
            DisplayKey::Thermal { body, .. } => {
                let outcome = self
                    .thermal
                    .outcome
                    .as_ref()
                    .expect("the key names an outcome");
                let Some(mut scene) = scenes(body) else {
                    self.display = None;
                    return;
                };
                scene.snapshot = SnapshotId::new(display_id_bytes(outcome.snapshot, epoch));
                scene.semantic_digest = display_digest(outcome.snapshot, epoch);
                let (low, high) = (outcome.result.min as f32, outcome.result.max as f32);
                let span = (high - low).max(1.0e-6);
                let palette = viewport::HeatPalette::Gradient {
                    near: 0.0,
                    far: span,
                };
                // Hottest reads red: the field is the distance below the
                // maximum.
                let field = outcome
                    .vertex_temperature
                    .iter()
                    .map(|temperature| (high - temperature).max(0.0))
                    .collect::<Vec<_>>();
                Some(SimulationDisplay {
                    key,
                    body,
                    scene,
                    field,
                    palette,
                    legend: scale_legend(palette, high, low, "°C"),
                    epoch,
                    show_field: self.thermal.show_temperature,
                    exaggeration: 0.0,
                })
            }
            DisplayKey::Topology { body, .. } => {
                let live = self.topology.live.as_ref().expect("the key names a field");
                let snapshot = self.topology.snapshot.unwrap_or(SnapshotId::ZERO);
                let scene = density_surface(
                    &live.mesh,
                    &live.densities,
                    self.topology.setup.threshold as f32,
                    snapshot,
                    epoch,
                );
                Some(SimulationDisplay {
                    key,
                    body,
                    scene,
                    field: Vec::new(),
                    palette: viewport::HeatPalette::Gradient {
                        near: 0.0,
                        far: 1.0,
                    },
                    legend: Vec::new(),
                    epoch,
                    show_field: false,
                    exaggeration: 0.0,
                })
            }
        };
    }
}

/// A snapshot id that is unique to one rebuild of the picture.
fn display_id_bytes(snapshot: SnapshotId, epoch: u64) -> [u8; 16] {
    let mut sixteen = [0_u8; 16];
    for (index, byte) in snapshot.as_bytes().iter().enumerate().take(16) {
        sixteen[index] = *byte;
    }
    for (index, byte) in epoch.to_le_bytes().iter().enumerate() {
        sixteen[index] ^= byte;
    }
    sixteen[15] ^= 0xA5;
    sixteen
}

fn display_digest(snapshot: SnapshotId, epoch: u64) -> artificer_protocol::SemanticDigest {
    let mut bytes = [0_u8; 32];
    for (index, byte) in snapshot.as_bytes().iter().enumerate().take(16) {
        bytes[index] = *byte;
    }
    bytes[16..24].copy_from_slice(&epoch.to_le_bytes());
    artificer_protocol::SemanticDigest::new(bytes)
}

/// The legend printed beside a picture painted from `high` in red down to
/// `low` in blue, in a unit.
fn scale_legend(
    palette: viewport::HeatPalette,
    high: f32,
    low: f32,
    unit: &str,
) -> Vec<viewport::HeatBand> {
    let span = high - low;
    let band = |fraction: f32| viewport::HeatBand {
        color: palette
            .color(span * fraction)
            .unwrap_or(egui::Color32::GRAY),
        label: format!("{} {unit}", figure(high - span * fraction)),
    };
    vec![band(0.0), band(0.5), band(1.0)]
}

fn figure(value: f32) -> String {
    if value >= 100.0 {
        format!("{value:.0}")
    } else if value >= 1.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.3}")
    }
}

/// Moves every vertex, edge end and vertex point of a scene by its
/// displacement, exaggerated.
fn displace_scene(scene: &mut DebugScene, outcome: &StructuralOutcome, exaggeration: f64) {
    let shift = |point: &mut Point3, displacement: [f32; 3]| {
        point.x += exaggeration * f64::from(displacement[0]);
        point.y += exaggeration * f64::from(displacement[1]);
        point.z += exaggeration * f64::from(displacement[2]);
    };
    for (index, triangle) in scene.triangles.iter_mut().enumerate() {
        for corner in 0..3 {
            if let Some(displacement) = outcome.vertex_displacement.get(3 * index + corner) {
                shift(&mut triangle.vertices[corner], *displacement);
            }
        }
    }
    for (edge, displacement) in scene.edges.iter_mut().zip(&outcome.edge_displacement) {
        shift(&mut edge.endpoints[0], displacement[0]);
        shift(&mut edge.endpoints[1], displacement[1]);
    }
    for (vertex, displacement) in scene.vertices.iter_mut().zip(&outcome.point_displacement) {
        shift(&mut vertex.point, *displacement);
    }
    // A silhouette is drawn from the exact carrier, which the deformed
    // facets no longer lie on.
    scene.carriers.clear();
}

/// Runs a study on a snapshot: voxelise, solve, and read the fields back
/// onto the display scene. Pure, so it runs on a worker or inline.
fn run_structural(
    body: BodyId,
    snapshot: &Snapshot,
    scene: &DebugScene,
    setup: &StructuralSetup,
    cancellation: &CancellationToken,
    progress: &mut dyn FnMut(Progress),
    voxels: &std::sync::atomic::AtomicUsize,
) -> Result<StructuralOutcome, StructuralError> {
    let Some(bounds) = snapshot.measures().bounds else {
        return Err(StructuralError::EmptyGrid);
    };
    let cell = setup
        .resolution
        .cell_for(bounds)
        .ok_or(StructuralError::EmptyGrid)?;
    progress(Progress {
        phase: "voxelising",
        done: 0,
        total: 1,
        residual: f64::NAN,
    });
    let grid = NativeKernel::voxelise(snapshot, cell);
    if grid.is_empty() {
        return Err(StructuralError::EmptyGrid);
    }
    let mesh = VoxelMesh::from_grid(grid);
    voxels.store(mesh.element_count(), Ordering::Relaxed);
    let study = setup.study();
    let result = solve_static(&mesh, &study, cancellation, progress)?;

    let sampler = FieldSampler::new(&mesh);
    let read_vector = |point: Point3| -> [f32; 3] {
        sampler
            .vector_at(point, &result.displacement)
            .map_or([0.0; 3], |value| value.map(|component| component as f32))
    };
    let mut vertex_stress = Vec::with_capacity(scene.triangles.len() * 3);
    let mut vertex_displacement = Vec::with_capacity(scene.triangles.len() * 3);
    for triangle in &scene.triangles {
        for corner in 0..3 {
            let inward = sampler.inward(triangle.vertices[corner], triangle.normals[corner]);
            vertex_stress.push(
                sampler
                    .scalar_at(inward, &result.node_von_mises)
                    .map_or(f32::NAN, |value| value as f32),
            );
            vertex_displacement.push(read_vector(inward));
        }
    }
    let edge_displacement = scene
        .edges
        .iter()
        .map(|edge| {
            [
                read_vector(edge.endpoints[0]),
                read_vector(edge.endpoints[1]),
            ]
        })
        .collect();
    let point_displacement = scene
        .vertices
        .iter()
        .map(|vertex| read_vector(vertex.point))
        .collect();
    Ok(StructuralOutcome {
        body,
        snapshot: snapshot.id(),
        resolution: setup.resolution,
        material: setup.material(),
        voxel_count: result.voxels,
        cell,
        result,
        vertex_stress,
        vertex_displacement,
        edge_displacement,
        point_displacement,
    })
}

/// A summary of the last structural result, for a caller that wants the
/// numbers without the fields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StructuralSummary {
    pub body: u64,
    pub resolution: Resolution,
    pub voxels: usize,
    pub cell: f64,
    pub max_von_mises: f64,
    pub peak_node_von_mises: f64,
    pub max_deflection: f64,
    pub safety_factor: f64,
    pub iterations: usize,
    pub residual: f64,
    pub converged: bool,
    pub tier: Tier,
}

impl KernelLabApp {
    /// Opens the structural study card on the active body.
    pub fn open_structural_study(&mut self) {
        let Some(body) = self.active_body_id() else {
            self.document_status = Some("A structural study needs an active body".to_owned());
            return;
        };
        if self.simulation.structural.body != Some(body) {
            self.simulation.structural = StructuralSetup {
                body: Some(body),
                ..StructuralSetup::default()
            };
            self.simulation.outcome = None;
            self.simulation.previous = None;
            self.simulation.display = None;
        }
        self.simulation.open = Some(StudyKind::Structural);
        self.simulation.message = None;
        self.document_status =
            Some("Structural study · pick faces, then Fix or Load them, then Solve".to_owned());
    }

    /// The faces currently selected on the studied body.
    fn simulation_selected_faces(&self) -> Vec<EntityRef> {
        let Some(body) = self.simulation.structural.body else {
            return Vec::new();
        };
        self.selected_faces
            .iter()
            .filter(|selection| selection.body.get() == body.get())
            .map(|selection| selection.face)
            .collect()
    }

    /// Adds a condition on every selected face of the studied body,
    /// replacing what the face had.
    fn simulation_condition_selected_faces(&mut self, condition: FaceCondition) {
        let faces = self.simulation_selected_faces();
        if faces.is_empty() {
            self.simulation.message =
                Some("Select a face of the studied body first, then press this".to_owned());
            return;
        }
        for face in faces {
            self.simulation
                .structural
                .faces
                .retain(|(held, _)| *held != face);
            self.simulation.structural.faces.push((face, condition));
        }
        self.simulation
            .structural
            .faces
            .sort_by_key(|(face, _)| face.entity.0);
        self.simulation.message = None;
        self.clear_model_entity_selection();
    }

    /// Fixes the selected faces of the studied body.
    pub fn simulation_fix_selected_faces(&mut self) {
        self.simulation_condition_selected_faces(FaceCondition::Fixed);
    }

    /// Loads the selected faces with the card's force.
    pub fn simulation_load_selected_faces(&mut self) {
        let condition = FaceCondition::Force {
            newtons: self.simulation.structural.force_newtons,
            direction: self.simulation.structural.force_direction,
        };
        self.simulation_condition_selected_faces(condition);
    }

    /// Presses on the selected faces with the card's pressure.
    pub fn simulation_press_selected_faces(&mut self) {
        let condition = FaceCondition::Pressure {
            megapascals: self.simulation.structural.pressure_megapascals,
        };
        self.simulation_condition_selected_faces(condition);
    }

    /// Runs the structural study at its resolution, off the UI thread
    /// where there is a scheduler and inline where there is not.
    pub fn solve_structural_study(&mut self) {
        self.solve_structural_at(self.simulation.structural.resolution);
    }

    /// Reruns the study one resolution finer, keeping the current result
    /// to report the change against.
    pub fn rerun_structural_finer(&mut self) {
        let Some(finer) = self.simulation.structural.resolution.finer() else {
            self.simulation.message = Some("Already at the finest resolution".to_owned());
            return;
        };
        self.solve_structural_at(finer);
    }

    fn solve_structural_at(&mut self, resolution: Resolution) {
        if self.simulation.running.is_some() {
            return;
        }
        let Some(body) = self.simulation.structural.body else {
            return;
        };
        let Some(record) = self.bodies.iter().find(|held| held.id == body) else {
            self.simulation.message =
                Some("The studied body is no longer in the workspace".to_owned());
            return;
        };
        self.simulation.structural.resolution = resolution;
        let setup = self.simulation.structural.clone();
        let snapshot = record.body.snapshot.clone();
        let scene = record.body.scene.clone();
        let cancellation = CancellationToken::new();
        let progress = Arc::new(Mutex::new(Progress {
            phase: "starting",
            done: 0,
            total: 1,
            residual: f64::NAN,
        }));
        let voxels = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        self.simulation.message = None;
        let Some(scheduler) = self.feature_preview_scheduler.as_ref() else {
            // No scheduler is the deterministic-test configuration, where
            // running it here is what a caller wants anyway.
            let outcome = run_structural(
                body,
                &snapshot,
                &scene,
                &setup,
                &cancellation,
                &mut |_| {},
                &voxels,
            );
            self.take_structural_outcome(outcome);
            return;
        };
        let job_cancellation = cancellation.clone();
        let reported = Arc::clone(&progress);
        let counted = Arc::clone(&voxels);
        let job = scheduler.submit(JobPriority::Commit, None, move |_| {
            run_structural(
                body,
                &snapshot,
                &scene,
                &setup,
                &job_cancellation,
                &mut |progress| {
                    if let Ok(mut held) = reported.lock() {
                        *held = progress;
                    }
                },
                &counted,
            )
        });
        self.simulation.running = Some(RunningSolve {
            job,
            cancellation,
            progress,
            voxels,
            resolution,
        });
        self.document_status = Some(format!(
            "Solving the structural study at {} resolution…",
            resolution.label()
        ));
    }

    fn take_structural_outcome(&mut self, outcome: Result<StructuralOutcome, StructuralError>) {
        match outcome {
            Ok(outcome) => {
                if let Some(held) = self.simulation.outcome.take()
                    && held.resolution != outcome.resolution
                {
                    self.simulation.previous = Some((held.resolution, held.result));
                }
                self.document_status = Some(format!(
                    "Structural study: max stress {} MPa, max deflection {}, safety factor {} · approximate on {} voxels",
                    figure(outcome.result.max_von_mises as f32),
                    self.length_unit().format(outcome.result.max_deflection),
                    factor(outcome.result.safety_factor),
                    outcome.voxel_count
                ));
                if !outcome.result.solve.converged {
                    self.simulation.message = Some(if outcome.result.solve.cancelled {
                        "Stopped before it converged; the fields are partial".to_owned()
                    } else {
                        format!(
                            "Stopped at a residual of {:.1e} without converging; the part may be too thin for this grid",
                            outcome.result.solve.residual
                        )
                    });
                }
                self.simulation.outcome = Some(outcome);
                self.simulation.show_stress = true;
                if self.simulation.exaggeration == 0.0 {
                    self.simulation.exaggeration = 10.0;
                }
            }
            Err(error) => {
                self.simulation.message = Some(format!("Refused: {error}"));
                self.document_status = Some(format!("Structural study refused: {error}"));
            }
        }
    }

    /// Stops a running solve. What it had is kept and says so.
    pub fn cancel_structural_study(&mut self) {
        if let Some(running) = self.simulation.running.as_ref() {
            running.cancellation.cancel();
        }
    }

    /// Collects a finished solve and keeps the drawn picture current.
    /// Called once a frame.
    pub(crate) fn poll_simulation(&mut self, context: &egui::Context) {
        if let Some(running) = self.simulation.running.as_ref() {
            match running.job.try_take() {
                None => {
                    if let Ok(progress) = running.progress.lock() {
                        let voxels = running.voxels.load(Ordering::Relaxed);
                        self.document_status = Some(match progress.phase {
                            "voxelising" | "starting" => "Voxelising the body…".to_owned(),
                            phase => format!(
                                "{phase} on {voxels} voxels · iteration {} · residual {:.1e}",
                                progress.done, progress.residual
                            ),
                        });
                    }
                    context.request_repaint();
                }
                Some(finished) => {
                    let running = self.simulation.running.take().expect("a finished solve");
                    match finished {
                        Ok(outcome) => self.take_structural_outcome(outcome),
                        Err(JobError::Cancelled) => {
                            self.simulation.message =
                                Some("Solve cancelled; nothing partial is drawn".to_owned());
                            self.document_status = Some(format!(
                                "Structural study at {} cancelled",
                                running.resolution.label()
                            ));
                        }
                        Err(error) => {
                            self.simulation.message = Some(format!("The solve failed: {error:?}"));
                            self.document_status = Some(format!(
                                "Structural study at {} failed: {error:?}",
                                running.resolution.label()
                            ));
                        }
                    }
                    context.request_repaint();
                }
            }
        }
        self.poll_thermal(context);
        self.poll_topology(context);
        self.poll_motion_timeline(context);
        let scenes = |body: BodyId| {
            self.bodies
                .iter()
                .find(|held| held.id == body)
                .map(|held| held.body.scene.clone())
        };
        let mut simulation = std::mem::take(&mut self.simulation);
        simulation.refresh_display(scenes);
        self.simulation = simulation;
    }

    /// The last structural result's headline numbers.
    #[must_use]
    pub fn structural_summary(&self) -> Option<StructuralSummary> {
        let outcome = self.simulation.outcome.as_ref()?;
        Some(StructuralSummary {
            body: outcome.body.get(),
            resolution: outcome.resolution,
            voxels: outcome.voxel_count,
            cell: outcome.cell,
            max_von_mises: outcome.result.max_von_mises,
            peak_node_von_mises: outcome.result.peak_node_von_mises,
            max_deflection: outcome.result.max_deflection,
            safety_factor: outcome.result.safety_factor,
            iterations: outcome.result.solve.iterations,
            residual: outcome.result.solve.residual,
            converged: outcome.result.solve.converged,
            tier: outcome.result.tier,
        })
    }

    /// The card's last refusal or note, for a caller checking it.
    #[must_use]
    pub fn simulation_message(&self) -> Option<&str> {
        self.simulation.message.as_deref()
    }

    /// The conditions on the structural study's faces, as the card lists
    /// them: the face's entity id and its condition in words.
    #[must_use]
    pub fn simulation_face_conditions(&self) -> Vec<(u64, String)> {
        self.simulation
            .structural
            .faces
            .iter()
            .map(|(face, condition)| (face.entity.0, condition.describe()))
            .collect()
    }

    /// How the studied body is being drawn: how many facet corners carry a
    /// stress reading, and the largest distance any drawn vertex has been
    /// moved from the model. `None` when the body is drawn as modelled.
    #[must_use]
    pub fn simulation_display_extent(&self) -> Option<(usize, f64)> {
        let display = self.simulation.display.as_ref()?;
        // Only a structural picture moves anything.
        let moved = match display.key {
            DisplayKey::Structural { .. } => {
                self.simulation.outcome.as_ref().map_or(0.0, |outcome| {
                    outcome
                        .vertex_displacement
                        .iter()
                        .map(|[x, y, z]| display.exaggeration * f64::from(x.hypot(*y).hypot(*z)))
                        .fold(0.0, f64::max)
                })
            }
            DisplayKey::Thermal { .. } | DisplayKey::Topology { .. } => 0.0,
        };
        Some((
            if display.show_field {
                display.field.len()
            } else {
                0
            },
            moved,
        ))
    }

    /// How many triangles the studied body is drawn with: its own facets
    /// under a field, or the voxel skin of a density field. `None` when it
    /// is drawn as modelled.
    #[must_use]
    pub fn simulation_display_triangles(&self) -> Option<usize> {
        self.simulation
            .display
            .as_ref()
            .map(|display| display.scene.triangles.len())
    }

    /// Whether the motion is playing.
    #[must_use]
    pub const fn motion_is_playing(&self) -> bool {
        self.motion.playing
    }

    /// Where the joints have put every component: the solved pose, which
    /// is what the viewport draws, rather than the assembled pose the
    /// document stores.
    #[must_use]
    pub fn posed_component_poses(&self) -> Vec<(u64, [f64; 3], [f64; 4])> {
        self.document
            .component_instances()
            .iter()
            .map(|component| {
                let pose = self.kinematics.pose(component.id).unwrap_or(component.pose);
                (
                    component.id.get(),
                    [
                        pose.translation.x(),
                        pose.translation.y(),
                        pose.translation.z(),
                    ],
                    [
                        pose.rotation.w(),
                        pose.rotation.x(),
                        pose.rotation.y(),
                        pose.rotation.z(),
                    ],
                )
            })
            .collect()
    }

    /// Whether the thermal study lets the unheld skin lose heat to the air.
    pub const fn set_thermal_convection(&mut self, convection: bool) {
        self.simulation.thermal.setup.convection = convection;
    }

    /// Sets the drawn exaggeration, as the slider does.
    pub fn set_simulation_exaggeration(&mut self, exaggeration: f64) {
        if exaggeration.is_finite() {
            self.simulation.exaggeration = exaggeration.clamp(0.0, 1000.0);
        }
    }

    /// The change from the previous resolution's result to the current
    /// one, as the convergence hint reads it: `None` until a study has
    /// been rerun at another resolution.
    #[must_use]
    pub fn simulation_convergence_hint(&self) -> Option<String> {
        self.convergence_hint()
    }

    fn convergence_hint(&self) -> Option<String> {
        let outcome = self.simulation.outcome.as_ref()?;
        let (previous_resolution, previous) = self.simulation.previous.as_ref()?;
        let change = |before: f64, after: f64| {
            if before.abs() > 0.0 {
                format!("{:+.1}%", 100.0 * (after - before) / before)
            } else {
                "—".to_owned()
            }
        };
        Some(format!(
            "{} → {}: max deflection {}, max stress {} · a settling answer is one to trust",
            previous_resolution.label(),
            outcome.resolution.label(),
            change(previous.max_deflection, outcome.result.max_deflection),
            change(previous.max_von_mises, outcome.result.max_von_mises),
        ))
    }

    /// The card, whichever study is open.
    pub(crate) fn simulation_card(&mut self, ui: &mut egui::Ui) {
        match self.simulation.open {
            Some(StudyKind::Structural) => self.structural_card(ui),
            Some(StudyKind::Thermal) => self.thermal_card(ui),
            Some(StudyKind::Topology) => self.topology_card(ui),
            Some(StudyKind::Motion) => self.motion_card(ui),
            None => {}
        }
    }

    /// The bodies the open study flags on the part: the pair that may share
    /// space at the motion timeline's current frame.
    #[must_use]
    pub fn simulation_flagged_bodies(&self) -> Vec<BodyId> {
        self.motion_flagged_bodies()
    }

    /// The structural study card.
    fn structural_card(&mut self, ui: &mut egui::Ui) {
        let body_label = self
            .simulation
            .structural
            .body
            .and_then(|body| self.bodies.iter().find(|held| held.id == body))
            .map_or_else(
                || "no body".to_owned(),
                |body| format!("Body {}", body.ordinal),
            );
        crate::status_line(
            ui,
            &format!("Static structural · {body_label}"),
            theme::accent(),
        );
        ui.label(
            RichText::new("Approximate: voxel mesh, trilinear elements. Refine to trust.")
                .small()
                .color(theme::muted()),
        );
        ui.add_space(4.0);

        // ---- conditions ----------------------------------------------------
        ui.label(RichText::new("FACES").small().color(theme::muted()));
        let selected = self.simulation_selected_faces().len();
        let mut remove = None;
        for (index, (face, condition)) in self.simulation.structural.faces.iter().enumerate() {
            ui.horizontal(|ui| {
                let colour = if *condition == FaceCondition::Fixed {
                    theme::accent()
                } else {
                    theme::good()
                };
                let (dot, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.0, colour);
                ui.label(
                    RichText::new(format!("Face #{} · {}", face.entity, condition.describe()))
                        .small(),
                );
                let response = ui.small_button("×").on_hover_text("Remove this condition");
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        format!("Remove condition on face {}", face.entity),
                    )
                });
                if response.clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            self.simulation.structural.faces.remove(index);
        }
        if self.simulation.structural.faces.is_empty() {
            ui.label(
                RichText::new("Click faces of the body in the viewport, then fix or load them.")
                    .small()
                    .color(theme::muted()),
            );
        }
        let pick_hint = if selected == 0 {
            "Select a face of the studied body first.".to_owned()
        } else {
            format!("{selected} selected")
        };
        if ui
            .add_enabled(selected > 0, egui::Button::new("Fix selected faces"))
            .on_hover_text("Hold every node on the selected faces still.")
            .on_disabled_hover_text(&pick_hint)
            .clicked()
        {
            self.simulation_fix_selected_faces();
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.simulation.structural.force_newtons)
                    .speed(1.0)
                    .range(-1.0e6..=1.0e6)
                    .suffix(" N"),
            )
            .on_hover_text("The total force spread over the faces it is applied to.");
            let mut direction = self.simulation.structural.force_direction;
            egui::ComboBox::from_id_salt("simulation_force_direction")
                .selected_text(direction.label())
                .show_ui(ui, |ui| {
                    for candidate in LoadDirection::ALL {
                        ui.selectable_value(&mut direction, candidate, candidate.label());
                    }
                });
            self.simulation.structural.force_direction = direction;
        });
        if ui
            .add_enabled(selected > 0, egui::Button::new("Load selected faces"))
            .on_hover_text("Push on the selected faces with the force above.")
            .on_disabled_hover_text(&pick_hint)
            .clicked()
        {
            self.simulation_load_selected_faces();
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.simulation.structural.pressure_megapascals)
                    .speed(0.05)
                    .range(0.0..=1.0e4)
                    .suffix(" MPa"),
            )
            .on_hover_text("A pressure into the part, everywhere on the faces it is applied to.");
            if ui
                .add_enabled(selected > 0, egui::Button::new("Press selected faces"))
                .on_disabled_hover_text(&pick_hint)
                .clicked()
            {
                self.simulation_press_selected_faces();
            }
        });
        ui.checkbox(&mut self.simulation.structural.gravity, "Own weight (−Z)")
            .on_hover_text("Add the part's weight as a body load, downward along Z.");

        // ---- material and resolution ----------------------------------------
        ui.add_space(4.0);
        ui.label(RichText::new("MATERIAL").small().color(theme::muted()));
        let current = self.simulation.structural.material();
        let mut chosen = None;
        egui::ComboBox::from_id_salt("simulation_material")
            .selected_text(current.name)
            .width(ui.available_width() - 8.0)
            .show_ui(ui, |ui| {
                for material in MATERIALS {
                    if ui
                        .selectable_label(material.key == current.key, material.name)
                        .on_hover_text(format!(
                            "E {} GPa · ν {:.2} · yield {} MPa",
                            material.youngs_modulus_mpa / 1000.0,
                            material.poisson_ratio,
                            material.yield_strength_mpa
                        ))
                        .clicked()
                    {
                        chosen = Some(material.key.to_owned());
                    }
                }
            });
        if let Some(key) = chosen {
            self.simulation.structural.material_key = key;
        }
        ui.label(RichText::new("RESOLUTION").small().color(theme::muted()));
        let bounds = self
            .simulation
            .structural
            .body
            .and_then(|body| self.bodies.iter().find(|held| held.id == body))
            .and_then(|body| body.body.snapshot.measures().bounds);
        ui.horizontal(|ui| {
            for resolution in Resolution::ALL {
                let held = self.simulation.structural.resolution == resolution;
                let cell = bounds.and_then(|bounds| resolution.cell_for(bounds));
                let response = ui.selectable_label(held, resolution.label());
                let response = match cell {
                    Some(cell) => response.on_hover_text(format!(
                        "{} cells along the longest side · {:.2} mm cells",
                        resolution.cells_along_longest_side(),
                        cell
                    )),
                    None => response,
                };
                if response.clicked() {
                    self.simulation.structural.resolution = resolution;
                }
            }
        });

        // ---- solve ----------------------------------------------------------
        ui.add_space(6.0);
        if let Some(running) = self.simulation.running.as_ref() {
            let (phase, done, total, residual) =
                running
                    .progress
                    .lock()
                    .map_or(("starting", 0, 1, f64::NAN), |progress| {
                        (
                            progress.phase,
                            progress.done,
                            progress.total,
                            progress.residual,
                        )
                    });
            let voxels = running.voxels.load(Ordering::Relaxed);
            let fraction = if residual.is_finite() && residual > 0.0 {
                // Residuals fall geometrically; log-scale the bar so it
                // moves through the solve rather than at its end.
                ((-residual.log10()) / 6.0).clamp(0.0, 1.0) as f32
            } else {
                (done as f32 / total.max(1) as f32).clamp(0.0, 1.0)
            };
            ui.add(
                egui::ProgressBar::new(fraction)
                    .text(format!("{phase} · {voxels} voxels · iteration {done}")),
            );
            if ui
                .button("Cancel solve")
                .on_hover_text("Stop the solver. Nothing partial is drawn.")
                .clicked()
            {
                self.cancel_structural_study();
            }
        } else {
            let ready = self.simulation.structural.fixed_count() > 0
                && self.simulation.structural.load_count() > 0;
            let response = ui.add_enabled(
                ready,
                egui::Button::new("Solve structural study")
                    .min_size(egui::vec2(ui.available_width(), 30.0)),
            );
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Solve structural study")
            });
            let response = response.on_disabled_hover_text(
                "Fix at least one face and load at least one face or add the part's weight.",
            );
            if response.clicked() {
                self.solve_structural_study();
            }
        }
        if let Some(message) = self.simulation.message.clone() {
            ui.label(RichText::new(message).small().color(theme::warn()));
        }

        // ---- results --------------------------------------------------------
        if let Some(outcome) = self.simulation.outcome.as_ref() {
            ui.add_space(6.0);
            let result = &outcome.result;
            let unit = self.length_unit();
            let stress_colour = if result.safety_factor < 1.0 {
                theme::bad()
            } else if result.safety_factor < 2.0 {
                theme::warn()
            } else {
                theme::good()
            };
            crate::status_line(
                ui,
                &format!(
                    "Safety factor {} against {} MPa yield",
                    factor(result.safety_factor),
                    outcome.material.yield_strength_mpa
                ),
                stress_colour,
            );
            theme::property_row(
                ui,
                "Max stress",
                &format!("{} MPa", figure(result.max_von_mises as f32)),
            )
            .on_hover_text(
                "Von Mises at the most worked element's centre. The node peak, half a cell nearer the surface, is what the colours show.",
            );
            theme::property_row(
                ui,
                "Surface peak",
                &format!("{} MPa", figure(result.peak_node_von_mises as f32)),
            );
            theme::property_row(ui, "Max deflection", &unit.format(result.max_deflection));
            theme::property_row(
                ui,
                "Mesh",
                &format!(
                    "{} voxels of {:.2} mm · {}",
                    outcome.voxel_count,
                    outcome.cell,
                    outcome.resolution.label()
                ),
            );
            theme::property_row(
                ui,
                "Solver",
                &format!(
                    "{} iterations · residual {:.1e}",
                    result.solve.iterations, result.solve.residual
                ),
            );
            ui.label(
                RichText::new(format!(
                    "Approximate. A {} voxel mesh is stiffer than the part and under-reads surface stress; the answer rises as it is refined.",
                    outcome.resolution.label().to_lowercase()
                ))
                .small()
                .color(theme::muted()),
            );
            if let Some(hint) = self.convergence_hint() {
                ui.label(RichText::new(hint).small().color(theme::accent()));
            }
            let finer = outcome.resolution.finer();
            let rerun = ui.add_enabled(
                finer.is_some() && self.simulation.running.is_none(),
                egui::Button::new("Rerun finer"),
            );
            rerun.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Rerun finer")
            });
            let rerun = rerun.on_hover_text(
                "Solve again at the next resolution and report how much the answer moved.",
            );
            if rerun.clicked() {
                self.rerun_structural_finer();
            }

            // ---- display ----------------------------------------------------
            ui.add_space(6.0);
            ui.label(RichText::new("DISPLAY").small().color(theme::muted()));
            ui.checkbox(&mut self.simulation.show_stress, "Stress colours")
                .on_hover_text(
                    "Paint the body by von Mises stress: red at the peak, blue at rest.",
                );
            let mut exaggeration = self.simulation.exaggeration;
            if ui
                .add(
                    egui::Slider::new(&mut exaggeration, 0.0..=100.0)
                        .text("Exaggerate")
                        .suffix("×"),
                )
                .on_hover_text("Draw the deformation this many times larger than it is. Zero draws the part as modelled.")
                .changed()
            {
                self.set_simulation_exaggeration(exaggeration);
            }
        }

        ui.add_space(4.0);
        let dismiss = ui.button("Dismiss study");
        dismiss.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Dismiss study")
        });
        if dismiss
            .on_hover_text("Clear the study and draw the body as modelled. Nothing in the document changes either way.")
            .clicked()
        {
            self.simulation.dismiss();
        }
    }
}

fn factor(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.2}")
    } else {
        "∞".to_owned()
    }
}

/// A surface field over the studied body for the viewport, when it is
/// showing one.
#[must_use]
pub fn display_field(
    display: &SimulationDisplay,
    samples: usize,
) -> Option<viewport::SurfaceField<'_>> {
    if !display.show_field || display.field.len() != samples {
        return None;
    }
    Some(viewport::SurfaceField {
        values: &display.field,
        palette: display.palette,
        epoch: display.epoch,
        legend: Some(&display.legend),
    })
}
