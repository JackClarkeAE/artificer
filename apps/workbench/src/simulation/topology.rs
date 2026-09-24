//! Topology optimisation (ADR 0058, M4), experimental: the structural
//! study's supports and loads carried onto the same grid, the material
//! carved down to a fraction of its volume, and the density field drawn
//! as a thresholded voxel surface that changes with every iteration.
//!
//! Nothing here is written back to the model: the result is a picture of
//! where material matters, not a body.

use std::sync::{Arc, Mutex};

use artificer_compute::{JobError, JobHandle, JobPriority};
use artificer_kernel::{
    CancellationToken, DebugScene, DebugTriangle, FaceRole, NativeKernel, Snapshot,
};
use artificer_model::BodyId;
use artificer_protocol::{EntityId, EntityKind, EntityRef, Point3, SnapshotId, Tier, Vector3};
use artificer_sim::element::SIDE_STEPS;
use artificer_sim::{
    Conditions, Resolution, TopologyError, TopologyResult, TopologyStudy, VoxelMesh,
    optimise_topology,
};
use egui::RichText;

use super::{StructuralSetup, StudyKind};
use crate::{KernelLabApp, theme};

/// An optimisation as the card sets it up. The supports and loads are the
/// structural study's.
#[derive(Clone, Debug, PartialEq)]
pub struct TopologySetup {
    pub volume_fraction: f64,
    pub iterations: usize,
    /// The density at and above which a voxel is drawn as material.
    pub threshold: f64,
    pub resolution: Resolution,
}

impl Default for TopologySetup {
    fn default() -> Self {
        Self {
            volume_fraction: 0.4,
            iterations: 30,
            threshold: 0.5,
            resolution: Resolution::Coarse,
        }
    }
}

/// The density field as the optimiser last reported it, with the mesh it
/// lies on.
#[derive(Clone, Debug)]
pub struct TopologyLive {
    pub mesh: Arc<VoxelMesh>,
    pub iteration: usize,
    pub max_iterations: usize,
    pub compliance: f64,
    pub change: f64,
    pub densities: Vec<f32>,
}

/// What a finished optimisation left.
#[derive(Clone, Debug)]
pub struct TopologyOutcome {
    pub body: BodyId,
    pub snapshot: SnapshotId,
    pub mesh: Arc<VoxelMesh>,
    pub result: TopologyResult,
    pub resolution: Resolution,
}

pub(super) struct RunningTopology {
    job: JobHandle<Result<(Arc<VoxelMesh>, TopologyResult), TopologyError>>,
    cancellation: CancellationToken,
    live: Arc<Mutex<Option<TopologyLive>>>,
}

/// The optimisation card's state.
#[derive(Default)]
pub struct TopologyState {
    pub setup: TopologySetup,
    pub(super) running: Option<RunningTopology>,
    pub outcome: Option<TopologyOutcome>,
    /// The latest field reported, live while running and final after.
    pub live: Option<TopologyLive>,
    pub body: Option<BodyId>,
    pub snapshot: Option<SnapshotId>,
}

impl TopologyState {
    pub(super) fn cancel(&mut self) {
        if let Some(running) = self.running.take() {
            running.cancellation.cancel();
        }
    }
}

/// The headline of the last optimisation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TopologySummary {
    pub body: u64,
    pub iterations: usize,
    pub voxels: usize,
    pub volume_fraction: f64,
    pub first_compliance: f64,
    pub last_compliance: f64,
    pub converged: bool,
    pub cancelled: bool,
    pub tier: Tier,
}

/// The surface of the voxels at or above a density threshold, as a scene
/// the viewport draws in place of the body.
#[must_use]
pub fn density_surface(
    mesh: &VoxelMesh,
    densities: &[f32],
    threshold: f32,
    snapshot: SnapshotId,
    epoch: u64,
) -> DebugScene {
    let grid = mesh.grid();
    let cell = grid.cell();
    let is_material = |cell: [usize; 3]| {
        mesh.element_in_cell(cell)
            .and_then(|element| densities.get(element))
            .is_some_and(|density| *density >= threshold)
    };
    let source_face = EntityRef {
        snapshot,
        entity: EntityId(0),
        kind: EntityKind::Face,
    };
    let mut triangles = Vec::new();
    for element in 0..mesh.element_count() {
        let position = mesh.cell_of_element(element);
        if !is_material(position) {
            continue;
        }
        let minimum = grid.cell_min(position);
        for (side, step) in SIDE_STEPS.iter().enumerate() {
            let neighbour = [
                position[0] as isize + isize::from(step[0]),
                position[1] as isize + isize::from(step[1]),
                position[2] as isize + isize::from(step[2]),
            ];
            let covered = neighbour.iter().all(|value| *value >= 0)
                && is_material(neighbour.map(|value| value as usize));
            if covered {
                continue;
            }
            let normal = Vector3::new(f64::from(step[0]), f64::from(step[1]), f64::from(step[2]));
            let role = match side {
                0 => FaceRole::NegativeX,
                1 => FaceRole::PositiveX,
                2 => FaceRole::NegativeY,
                3 => FaceRole::PositiveY,
                4 => FaceRole::NegativeZ,
                _ => FaceRole::PositiveZ,
            };
            let corners = side_corners(minimum, cell, side);
            for [a, b, c] in [[0, 1, 2], [0, 2, 3]] {
                triangles.push(DebugTriangle {
                    vertices: [corners[a], corners[b], corners[c]],
                    normals: [normal; 3],
                    source_face,
                    role,
                });
            }
        }
    }
    let mut digest = [0_u8; 32];
    digest[..8].copy_from_slice(&epoch.to_le_bytes());
    digest[8..12].copy_from_slice(&threshold.to_le_bytes());
    let mut id = [0_u8; 16];
    for (index, byte) in snapshot.as_bytes().iter().enumerate().take(16) {
        id[index] = *byte;
    }
    for (index, byte) in epoch.to_le_bytes().iter().enumerate() {
        id[8 + index] ^= byte;
    }
    id[0] ^= 0x5A;
    DebugScene {
        snapshot: SnapshotId::new(id),
        semantic_digest: artificer_protocol::SemanticDigest::new(digest),
        triangles,
        edges: Vec::new(),
        vertices: Vec::new(),
        carriers: Vec::new(),
    }
}

/// The four corners of one side of a cell, wound counter-clockwise seen
/// from outside.
fn side_corners(minimum: Point3, cell: f64, side: usize) -> [Point3; 4] {
    let at = |dx: f64, dy: f64, dz: f64| {
        Point3::new(
            dx.mul_add(cell, minimum.x),
            dy.mul_add(cell, minimum.y),
            dz.mul_add(cell, minimum.z),
        )
    };
    match side {
        0 => [
            at(0.0, 0.0, 0.0),
            at(0.0, 0.0, 1.0),
            at(0.0, 1.0, 1.0),
            at(0.0, 1.0, 0.0),
        ],
        1 => [
            at(1.0, 0.0, 0.0),
            at(1.0, 1.0, 0.0),
            at(1.0, 1.0, 1.0),
            at(1.0, 0.0, 1.0),
        ],
        2 => [
            at(0.0, 0.0, 0.0),
            at(1.0, 0.0, 0.0),
            at(1.0, 0.0, 1.0),
            at(0.0, 0.0, 1.0),
        ],
        3 => [
            at(0.0, 1.0, 0.0),
            at(0.0, 1.0, 1.0),
            at(1.0, 1.0, 1.0),
            at(1.0, 1.0, 0.0),
        ],
        4 => [
            at(0.0, 0.0, 0.0),
            at(0.0, 1.0, 0.0),
            at(1.0, 1.0, 0.0),
            at(1.0, 0.0, 0.0),
        ],
        _ => [
            at(0.0, 0.0, 1.0),
            at(1.0, 0.0, 1.0),
            at(1.0, 1.0, 1.0),
            at(0.0, 1.0, 1.0),
        ],
    }
}

fn run_topology(
    snapshot: &Snapshot,
    structural: &StructuralSetup,
    setup: &TopologySetup,
    cancellation: &CancellationToken,
    live: &Mutex<Option<TopologyLive>>,
) -> Result<(Arc<VoxelMesh>, TopologyResult), TopologyError> {
    let Some(bounds) = snapshot.measures().bounds else {
        return Err(TopologyError::EmptyGrid);
    };
    let cell = setup
        .resolution
        .cell_for(bounds)
        .ok_or(TopologyError::EmptyGrid)?;
    let grid = NativeKernel::voxelise(snapshot, cell);
    if grid.is_empty() {
        return Err(TopologyError::EmptyGrid);
    }
    let mesh = Arc::new(VoxelMesh::from_grid(grid));
    let conditions = Conditions::from_study(&mesh, &structural.study()).map_err(|error| {
        use artificer_sim::StructuralError;
        match error {
            StructuralError::NoSupports | StructuralError::SupportOnNoCells { .. } => {
                TopologyError::NoSupports
            }
            StructuralError::EmptyGrid => TopologyError::EmptyGrid,
            _ => TopologyError::NoLoad,
        }
    })?;
    let mut study = TopologyStudy::new(structural.material(), conditions, setup.volume_fraction);
    study.max_iterations = setup.iterations.max(1);
    let reported = Arc::clone(&mesh);
    let result = optimise_topology(&mesh, &study, cancellation, &mut |progress| {
        if let Ok(mut held) = live.lock() {
            *held = Some(TopologyLive {
                mesh: Arc::clone(&reported),
                iteration: progress.iteration,
                max_iterations: progress.max_iterations,
                compliance: progress.compliance,
                change: progress.change,
                densities: progress
                    .densities
                    .iter()
                    .map(|density| *density as f32)
                    .collect(),
            });
        }
    })?;
    Ok((mesh, result))
}

impl KernelLabApp {
    /// Opens the optimisation card, carrying the structural study's setup.
    pub fn open_topology_study(&mut self) {
        let Some(body) = self
            .simulation
            .structural
            .body
            .or_else(|| self.active_body_id())
        else {
            self.document_status = Some("An optimisation needs an active body".to_owned());
            return;
        };
        if self.simulation.structural.body != Some(body) {
            self.simulation.structural = StructuralSetup {
                body: Some(body),
                ..StructuralSetup::default()
            };
        }
        if self.simulation.topology.body != Some(body) {
            self.simulation.topology.outcome = None;
            self.simulation.topology.live = None;
        }
        self.simulation.topology.body = Some(body);
        self.simulation.open = Some(StudyKind::Topology);
        self.simulation.message = None;
        self.document_status = Some(
            "Topology optimisation (experimental) · uses the structural study's supports and loads"
                .to_owned(),
        );
    }

    /// Runs the optimisation.
    pub fn optimise_topology_study(&mut self) {
        if self.simulation.topology.running.is_some() {
            return;
        }
        let Some(body) = self.simulation.topology.body else {
            return;
        };
        let Some(record) = self.bodies.iter().find(|held| held.id == body) else {
            self.simulation.message =
                Some("The studied body is no longer in the workspace".to_owned());
            return;
        };
        let structural = self.simulation.structural.clone();
        let setup = self.simulation.topology.setup.clone();
        let snapshot = record.body.snapshot.clone();
        self.simulation.topology.snapshot = Some(snapshot.id());
        let cancellation = CancellationToken::new();
        let live = Arc::new(Mutex::new(None));
        self.simulation.message = None;
        self.simulation.topology.outcome = None;
        let Some(scheduler) = self.feature_preview_scheduler.as_ref() else {
            let outcome = run_topology(&snapshot, &structural, &setup, &cancellation, &live);
            if let Ok(held) = live.lock() {
                self.simulation.topology.live.clone_from(&held);
            }
            self.take_topology_outcome(body, snapshot.id(), setup.resolution, outcome);
            return;
        };
        let job_cancellation = cancellation.clone();
        let reported = Arc::clone(&live);
        let job = scheduler.submit(JobPriority::Commit, None, move |_| {
            run_topology(&snapshot, &structural, &setup, &job_cancellation, &reported)
        });
        self.simulation.topology.running = Some(RunningTopology {
            job,
            cancellation,
            live,
        });
        self.document_status = Some("Optimising…".to_owned());
    }

    fn take_topology_outcome(
        &mut self,
        body: BodyId,
        snapshot: SnapshotId,
        resolution: Resolution,
        outcome: Result<(Arc<VoxelMesh>, TopologyResult), TopologyError>,
    ) {
        match outcome {
            Ok((mesh, result)) => {
                let first = result.compliance_history.first().copied().unwrap_or(0.0);
                let last = result.compliance_history.last().copied().unwrap_or(0.0);
                self.document_status = Some(format!(
                    "Optimised in {} iterations: compliance {:.3} → {:.3} at {:.0}% volume · experimental",
                    result.iterations,
                    first,
                    last,
                    100.0 * result.volume_fraction
                ));
                if result.cancelled {
                    self.simulation.message = Some(format!(
                        "Stopped after {} iterations; the field is where it got to",
                        result.iterations
                    ));
                }
                self.simulation.topology.outcome = Some(TopologyOutcome {
                    body,
                    snapshot,
                    mesh,
                    result,
                    resolution,
                });
            }
            Err(error) => {
                self.simulation.message = Some(format!("Refused: {error}"));
                self.document_status = Some(format!("Optimisation refused: {error}"));
            }
        }
    }

    /// Stops a running optimisation, keeping the field it reached.
    pub fn cancel_topology_study(&mut self) {
        if let Some(running) = self.simulation.topology.running.as_ref() {
            running.cancellation.cancel();
        }
    }

    pub(super) fn poll_topology(&mut self, context: &egui::Context) {
        let Some(running) = self.simulation.topology.running.as_ref() else {
            return;
        };
        if let Ok(held) = running.live.lock()
            && let Some(live) = held.as_ref()
            && self
                .simulation
                .topology
                .live
                .as_ref()
                .is_none_or(|known| known.iteration != live.iteration)
        {
            self.simulation.topology.live = Some(live.clone());
        }
        match running.job.try_take() {
            None => {
                if let Some(live) = self.simulation.topology.live.as_ref() {
                    self.document_status = Some(format!(
                        "Optimising · iteration {} of {} · compliance {:.3} · change {:.3}",
                        live.iteration, live.max_iterations, live.compliance, live.change
                    ));
                }
                context.request_repaint();
            }
            Some(finished) => {
                let running = self
                    .simulation
                    .topology
                    .running
                    .take()
                    .expect("a finished optimisation");
                let Some(body) = self.simulation.topology.body else {
                    drop(running);
                    return;
                };
                let snapshot = self
                    .simulation
                    .topology
                    .snapshot
                    .unwrap_or(SnapshotId::ZERO);
                let resolution = self.simulation.topology.setup.resolution;
                match finished {
                    Ok(outcome) => self.take_topology_outcome(body, snapshot, resolution, outcome),
                    Err(JobError::Cancelled) => {
                        self.simulation.message = Some("Optimisation cancelled".to_owned());
                    }
                    Err(error) => {
                        self.simulation.message =
                            Some(format!("The optimisation failed: {error:?}"));
                    }
                }
                drop(running);
                context.request_repaint();
            }
        }
    }

    /// The last optimisation's headline numbers.
    #[must_use]
    pub fn topology_summary(&self) -> Option<TopologySummary> {
        let outcome = self.simulation.topology.outcome.as_ref()?;
        Some(TopologySummary {
            body: outcome.body.get(),
            iterations: outcome.result.iterations,
            voxels: outcome.result.voxels,
            volume_fraction: outcome.result.volume_fraction,
            first_compliance: outcome
                .result
                .compliance_history
                .first()
                .copied()
                .unwrap_or(0.0),
            last_compliance: outcome
                .result
                .compliance_history
                .last()
                .copied()
                .unwrap_or(0.0),
            converged: outcome.result.converged,
            cancelled: outcome.result.cancelled,
            tier: outcome.result.tier,
        })
    }

    /// Sets the fraction of the volume to keep, as the slider does.
    pub fn set_topology_volume_fraction(&mut self, fraction: f64) {
        if fraction.is_finite() {
            self.simulation.topology.setup.volume_fraction = fraction.clamp(0.05, 0.95);
        }
    }

    /// Sets how many iterations the optimiser may take.
    pub fn set_topology_iterations(&mut self, iterations: usize) {
        self.simulation.topology.setup.iterations = iterations.clamp(1, 200);
    }

    /// The optimisation card.
    pub(super) fn topology_card(&mut self, ui: &mut egui::Ui) {
        crate::status_line(ui, "Topology optimisation · EXPERIMENTAL", theme::warn());
        ui.label(
            RichText::new(
                "Carves the body down to a fraction of its volume that carries the structural study's loads best. A picture of where material matters; nothing is written to the model.",
            )
            .small()
            .color(theme::muted()),
        );
        ui.add_space(4.0);
        let supports = self.simulation.structural.fixed_count();
        let loads = self.simulation.structural.load_count();
        ui.label(
            RichText::new(format!(
                "From the structural study: {supports} fixed face{}, {loads} load{}",
                if supports == 1 { "" } else { "s" },
                if loads == 1 { "" } else { "s" }
            ))
            .small()
            .color(if supports > 0 && loads > 0 {
                theme::good()
            } else {
                theme::warn()
            }),
        );
        if supports == 0 || loads == 0 {
            ui.label(
                RichText::new(
                    "Set the structural study's faces first; the optimisation carries them.",
                )
                .small()
                .color(theme::muted()),
            );
        }
        let mut fraction = self.simulation.topology.setup.volume_fraction;
        if ui
            .add(
                egui::Slider::new(&mut fraction, 0.1..=0.9)
                    .text("Keep")
                    .custom_formatter(|value, _| format!("{:.0}%", 100.0 * value)),
            )
            .changed()
        {
            self.set_topology_volume_fraction(fraction);
        }
        let mut iterations = self.simulation.topology.setup.iterations;
        if ui
            .add(egui::Slider::new(&mut iterations, 5..=100).text("Iterations"))
            .changed()
        {
            self.set_topology_iterations(iterations);
        }
        ui.horizontal(|ui| {
            for resolution in Resolution::ALL {
                let held = self.simulation.topology.setup.resolution == resolution;
                if ui.selectable_label(held, resolution.label()).clicked() {
                    self.simulation.topology.setup.resolution = resolution;
                }
            }
        });

        ui.add_space(6.0);
        if self.simulation.topology.running.is_some() {
            let (iteration, total) = self
                .simulation
                .topology
                .live
                .as_ref()
                .map_or((0, self.simulation.topology.setup.iterations), |live| {
                    (live.iteration, live.max_iterations)
                });
            ui.add(
                egui::ProgressBar::new(iteration as f32 / total.max(1) as f32)
                    .text(format!("Iteration {iteration} of {total}")),
            );
            if ui.button("Stop optimising").clicked() {
                self.cancel_topology_study();
            }
        } else {
            let ready = supports > 0 && loads > 0;
            let optimise = ui.add_enabled(
                ready,
                egui::Button::new("Optimise").min_size(egui::vec2(ui.available_width(), 30.0)),
            );
            optimise.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Optimise topology")
            });
            if optimise
                .on_disabled_hover_text("The structural study needs a fixed face and a load first.")
                .clicked()
            {
                self.optimise_topology_study();
            }
        }
        if let Some(message) = self.simulation.message.clone() {
            ui.label(RichText::new(message).small().color(theme::warn()));
        }

        if let Some(live) = self.simulation.topology.live.as_ref() {
            ui.add_space(4.0);
            theme::property_row(
                ui,
                "Iteration",
                &format!("{} of {}", live.iteration, live.max_iterations),
            );
            theme::property_row(ui, "Compliance", &format!("{:.4}", live.compliance));
            theme::property_row(ui, "Change", &format!("{:.3}", live.change));
            theme::property_row(ui, "Voxels", &live.densities.len().to_string());
        }
        if let Some(outcome) = self.simulation.topology.outcome.as_ref() {
            let first = outcome
                .result
                .compliance_history
                .first()
                .copied()
                .unwrap_or(0.0);
            let last = outcome
                .result
                .compliance_history
                .last()
                .copied()
                .unwrap_or(0.0);
            crate::status_line(
                ui,
                &format!(
                    "{} iterations · compliance {:.3} → {:.3} · {:.0}% kept",
                    outcome.result.iterations,
                    first,
                    last,
                    100.0 * outcome.result.volume_fraction
                ),
                if outcome.result.converged {
                    theme::good()
                } else {
                    theme::warn()
                },
            );
        }
        let mut threshold = self.simulation.topology.setup.threshold;
        if ui
            .add(egui::Slider::new(&mut threshold, 0.1..=0.9).text("Threshold"))
            .on_hover_text("A voxel at or above this density is drawn as material.")
            .changed()
        {
            self.simulation.topology.setup.threshold = threshold.clamp(0.05, 0.95);
        }
        ui.label(
            RichText::new(
                "Approximate and experimental: SIMP on the voxel grid with a density filter of 1.5 cells; the drawn surface is a threshold of a smooth field.",
            )
            .small()
            .color(theme::muted()),
        );

        ui.add_space(4.0);
        let dismiss = ui.button("Dismiss study");
        dismiss.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Dismiss study")
        });
        if dismiss.clicked() {
            self.simulation.dismiss();
        }
    }
}
