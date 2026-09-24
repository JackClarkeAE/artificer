//! The steady-state thermal study (ADR 0058, M3): faces held at
//! temperatures, the rest of the skin losing heat to the air, the
//! temperature painted on the part through the same field machinery the
//! stress is.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use artificer_compute::{JobError, JobHandle, JobPriority};
use artificer_kernel::{CancellationToken, DebugScene, NativeKernel, Snapshot};
use artificer_model::BodyId;
use artificer_protocol::{EntityRef, SnapshotId, Tier};
use artificer_sim::{
    Convection, FieldSampler, MATERIALS, Material, Progress, Resolution, ThermalError,
    ThermalResult, ThermalStudy, VoxelMesh, material_by_key, solve_steady_state,
};
use egui::RichText;

use super::StudyKind;
use crate::{KernelLabApp, theme};

/// A thermal study as the card sets it up.
#[derive(Clone, Debug, PartialEq)]
pub struct ThermalSetup {
    pub body: Option<BodyId>,
    /// Faces held at temperatures, in degrees Celsius.
    pub faces: Vec<(EntityRef, f64)>,
    pub material_key: String,
    pub resolution: Resolution,
    pub convection: bool,
    pub coefficient_w_m2k: f64,
    pub ambient_c: f64,
    /// The temperature the next "Hold selected faces" applies.
    pub next_temperature_c: f64,
}

impl Default for ThermalSetup {
    fn default() -> Self {
        Self {
            body: None,
            faces: Vec::new(),
            material_key: "aluminium-6061".to_owned(),
            resolution: Resolution::Coarse,
            convection: true,
            coefficient_w_m2k: 10.0,
            ambient_c: 20.0,
            next_temperature_c: 100.0,
        }
    }
}

impl ThermalSetup {
    fn material(&self) -> Material {
        material_by_key(&self.material_key).unwrap_or(MATERIALS[0])
    }

    fn study(&self) -> ThermalStudy {
        let mut study = ThermalStudy::new(self.material());
        study.held.clone_from(&self.faces);
        if self.convection {
            study.convection = Some(Convection {
                coefficient_w_m2k: self.coefficient_w_m2k,
                ambient_c: self.ambient_c,
            });
        }
        study
    }
}

/// What one thermal solve came back with, ready to draw.
#[derive(Clone, Debug)]
pub struct ThermalOutcome {
    pub body: BodyId,
    pub snapshot: SnapshotId,
    pub resolution: Resolution,
    pub result: ThermalResult,
    /// The temperature at every facet corner of the display scene.
    pub vertex_temperature: Vec<f32>,
    pub voxel_count: usize,
    pub cell: f64,
}

pub(super) struct RunningThermal {
    job: JobHandle<Result<ThermalOutcome, ThermalError>>,
    cancellation: CancellationToken,
    progress: Arc<Mutex<Progress>>,
    voxels: Arc<AtomicUsize>,
}

/// The thermal card's state.
#[derive(Default)]
pub struct ThermalState {
    pub setup: ThermalSetup,
    pub(super) running: Option<RunningThermal>,
    pub outcome: Option<ThermalOutcome>,
    pub show_temperature: bool,
}

impl ThermalState {
    pub(super) fn cancel(&mut self) {
        if let Some(running) = self.running.take() {
            running.cancellation.cancel();
        }
    }
}

/// The headline of the last thermal result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThermalSummary {
    pub body: u64,
    pub resolution: Resolution,
    pub voxels: usize,
    pub min_c: f64,
    pub max_c: f64,
    pub held_nodes: usize,
    pub convecting_sides: usize,
    pub iterations: usize,
    pub residual: f64,
    pub converged: bool,
    pub tier: Tier,
}

fn run_thermal(
    body: BodyId,
    snapshot: &Snapshot,
    scene: &DebugScene,
    setup: &ThermalSetup,
    cancellation: &CancellationToken,
    progress: &mut dyn FnMut(Progress),
    voxels: &AtomicUsize,
) -> Result<ThermalOutcome, ThermalError> {
    let Some(bounds) = snapshot.measures().bounds else {
        return Err(ThermalError::EmptyGrid);
    };
    let cell = setup
        .resolution
        .cell_for(bounds)
        .ok_or(ThermalError::EmptyGrid)?;
    progress(Progress {
        phase: "voxelising",
        done: 0,
        total: 1,
        residual: f64::NAN,
    });
    let grid = NativeKernel::voxelise(snapshot, cell);
    if grid.is_empty() {
        return Err(ThermalError::EmptyGrid);
    }
    let mesh = VoxelMesh::from_grid(grid);
    voxels.store(mesh.element_count(), Ordering::Relaxed);
    let result = solve_steady_state(&mesh, &setup.study(), cancellation, progress)?;
    let sampler = FieldSampler::new(&mesh);
    let mut vertex_temperature = Vec::with_capacity(scene.triangles.len() * 3);
    for triangle in &scene.triangles {
        for corner in 0..3 {
            let inward = sampler.inward(triangle.vertices[corner], triangle.normals[corner]);
            vertex_temperature.push(
                sampler
                    .scalar_at(inward, &result.temperature)
                    .map_or(f32::NAN, |value| value as f32),
            );
        }
    }
    Ok(ThermalOutcome {
        body,
        snapshot: snapshot.id(),
        resolution: setup.resolution,
        voxel_count: result.voxels,
        cell,
        result,
        vertex_temperature,
    })
}

impl KernelLabApp {
    /// Opens the thermal study card on the active body.
    pub fn open_thermal_study(&mut self) {
        let Some(body) = self.active_body_id() else {
            self.document_status = Some("A thermal study needs an active body".to_owned());
            return;
        };
        if self.simulation.thermal.setup.body != Some(body) {
            self.simulation.thermal.setup = ThermalSetup {
                body: Some(body),
                ..ThermalSetup::default()
            };
            self.simulation.thermal.outcome = None;
        }
        self.simulation.open = Some(StudyKind::Thermal);
        self.simulation.message = None;
        self.document_status =
            Some("Thermal study · pick faces, hold them at temperatures, then Solve".to_owned());
    }

    /// The faces currently selected on the thermally studied body.
    fn thermal_selected_faces(&self) -> Vec<EntityRef> {
        let Some(body) = self.simulation.thermal.setup.body else {
            return Vec::new();
        };
        self.selected_faces
            .iter()
            .filter(|selection| selection.body.get() == body.get())
            .map(|selection| selection.face)
            .collect()
    }

    /// Holds the selected faces at the card's temperature.
    pub fn simulation_hold_selected_faces(&mut self) {
        let faces = self.thermal_selected_faces();
        if faces.is_empty() {
            self.simulation.message =
                Some("Select a face of the studied body first, then press this".to_owned());
            return;
        }
        let celsius = self.simulation.thermal.setup.next_temperature_c;
        for face in faces {
            self.simulation
                .thermal
                .setup
                .faces
                .retain(|(held, _)| *held != face);
            self.simulation.thermal.setup.faces.push((face, celsius));
        }
        self.simulation
            .thermal
            .setup
            .faces
            .sort_by_key(|(face, _)| face.entity.0);
        self.simulation.message = None;
        self.clear_model_entity_selection();
    }

    /// Sets the temperature the next held faces take.
    pub fn set_simulation_hold_temperature(&mut self, celsius: f64) {
        if celsius.is_finite() {
            self.simulation.thermal.setup.next_temperature_c = celsius;
        }
    }

    /// Runs the thermal study.
    pub fn solve_thermal_study(&mut self) {
        if self.simulation.thermal.running.is_some() {
            return;
        }
        let Some(body) = self.simulation.thermal.setup.body else {
            return;
        };
        let Some(record) = self.bodies.iter().find(|held| held.id == body) else {
            self.simulation.message =
                Some("The studied body is no longer in the workspace".to_owned());
            return;
        };
        let setup = self.simulation.thermal.setup.clone();
        let snapshot = record.body.snapshot.clone();
        let scene = record.body.scene.clone();
        let cancellation = CancellationToken::new();
        let progress = Arc::new(Mutex::new(Progress {
            phase: "starting",
            done: 0,
            total: 1,
            residual: f64::NAN,
        }));
        let voxels = Arc::new(AtomicUsize::new(0));
        self.simulation.message = None;
        let Some(scheduler) = self.feature_preview_scheduler.as_ref() else {
            let outcome = run_thermal(
                body,
                &snapshot,
                &scene,
                &setup,
                &cancellation,
                &mut |_| {},
                &voxels,
            );
            self.take_thermal_outcome(outcome);
            return;
        };
        let job_cancellation = cancellation.clone();
        let reported = Arc::clone(&progress);
        let counted = Arc::clone(&voxels);
        let job = scheduler.submit(JobPriority::Commit, None, move |_| {
            run_thermal(
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
        self.simulation.thermal.running = Some(RunningThermal {
            job,
            cancellation,
            progress,
            voxels,
        });
        self.document_status = Some("Solving the thermal study…".to_owned());
    }

    fn take_thermal_outcome(&mut self, outcome: Result<ThermalOutcome, ThermalError>) {
        match outcome {
            Ok(outcome) => {
                self.document_status = Some(format!(
                    "Thermal study: {:.1} °C to {:.1} °C · approximate on {} voxels",
                    outcome.result.min, outcome.result.max, outcome.voxel_count
                ));
                if !outcome.result.solve.converged {
                    self.simulation.message = Some(format!(
                        "Stopped at a residual of {:.1e} without converging",
                        outcome.result.solve.residual
                    ));
                }
                self.simulation.thermal.outcome = Some(outcome);
                self.simulation.thermal.show_temperature = true;
            }
            Err(error) => {
                self.simulation.message = Some(format!("Refused: {error}"));
                self.document_status = Some(format!("Thermal study refused: {error}"));
            }
        }
    }

    /// Stops a running thermal solve.
    pub fn cancel_thermal_study(&mut self) {
        if let Some(running) = self.simulation.thermal.running.as_ref() {
            running.cancellation.cancel();
        }
    }

    pub(super) fn poll_thermal(&mut self, context: &egui::Context) {
        let Some(running) = self.simulation.thermal.running.as_ref() else {
            return;
        };
        match running.job.try_take() {
            None => {
                if let Ok(progress) = running.progress.lock() {
                    let voxels = running.voxels.load(Ordering::Relaxed);
                    self.document_status = Some(format!(
                        "{} on {voxels} voxels · iteration {} · residual {:.1e}",
                        progress.phase, progress.done, progress.residual
                    ));
                }
                context.request_repaint();
            }
            Some(finished) => {
                self.simulation.thermal.running = None;
                match finished {
                    Ok(outcome) => self.take_thermal_outcome(outcome),
                    Err(JobError::Cancelled) => {
                        self.simulation.message = Some("Solve cancelled".to_owned());
                    }
                    Err(error) => {
                        self.simulation.message = Some(format!("The solve failed: {error:?}"));
                    }
                }
                context.request_repaint();
            }
        }
    }

    /// The last thermal result's headline numbers.
    #[must_use]
    pub fn thermal_summary(&self) -> Option<ThermalSummary> {
        let outcome = self.simulation.thermal.outcome.as_ref()?;
        Some(ThermalSummary {
            body: outcome.body.get(),
            resolution: outcome.resolution,
            voxels: outcome.voxel_count,
            min_c: outcome.result.min,
            max_c: outcome.result.max,
            held_nodes: outcome.result.held_nodes,
            convecting_sides: outcome.result.convecting_sides,
            iterations: outcome.result.solve.iterations,
            residual: outcome.result.solve.residual,
            converged: outcome.result.solve.converged,
            tier: outcome.result.tier,
        })
    }

    /// The thermal study card.
    pub(super) fn thermal_card(&mut self, ui: &mut egui::Ui) {
        let body_label = self
            .simulation
            .thermal
            .setup
            .body
            .and_then(|body| self.bodies.iter().find(|held| held.id == body))
            .map_or_else(
                || "no body".to_owned(),
                |body| format!("Body {}", body.ordinal),
            );
        crate::status_line(
            ui,
            &format!("Steady-state thermal · {body_label}"),
            theme::accent(),
        );
        ui.label(
            RichText::new("Approximate: voxel mesh, lumped convection. Refine to trust.")
                .small()
                .color(theme::muted()),
        );
        ui.add_space(4.0);

        ui.label(RichText::new("HELD FACES").small().color(theme::muted()));
        let selected = self.thermal_selected_faces().len();
        let mut remove = None;
        for (index, (face, celsius)) in self.simulation.thermal.setup.faces.iter().enumerate() {
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
                ui.painter()
                    .circle_filled(dot.center(), 3.0, theme::accent());
                ui.label(RichText::new(format!("Face #{} · {celsius:.1} °C", face.entity)).small());
                let response = ui.small_button("×").on_hover_text("Release this face");
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        format!("Release face {}", face.entity),
                    )
                });
                if response.clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            self.simulation.thermal.setup.faces.remove(index);
        }
        if self.simulation.thermal.setup.faces.is_empty() {
            ui.label(
                RichText::new(
                    "Click faces of the body in the viewport, then hold them at a temperature.",
                )
                .small()
                .color(theme::muted()),
            );
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.simulation.thermal.setup.next_temperature_c)
                    .speed(1.0)
                    .range(-273.0..=3000.0)
                    .suffix(" °C"),
            );
            let hold = ui.add_enabled(selected > 0, egui::Button::new("Hold selected faces"));
            hold.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Hold selected faces")
            });
            if hold
                .on_disabled_hover_text("Select a face of the studied body first.")
                .clicked()
            {
                self.simulation_hold_selected_faces();
            }
        });
        ui.checkbox(
            &mut self.simulation.thermal.setup.convection,
            "Lose heat to the air",
        )
        .on_hover_text("Every exposed side that is not held loses heat by convection.");
        if self.simulation.thermal.setup.convection {
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut self.simulation.thermal.setup.coefficient_w_m2k)
                        .speed(0.5)
                        .range(0.0..=10_000.0)
                        .prefix("h ")
                        .suffix(" W/m²K"),
                )
                .on_hover_text("Still air is about 10, a fan about 50, water about 500.");
                ui.add(
                    egui::DragValue::new(&mut self.simulation.thermal.setup.ambient_c)
                        .speed(0.5)
                        .range(-273.0..=1000.0)
                        .prefix("air ")
                        .suffix(" °C"),
                );
            });
        }

        ui.add_space(4.0);
        ui.label(RichText::new("MATERIAL").small().color(theme::muted()));
        let current = self.simulation.thermal.setup.material();
        let mut chosen = None;
        egui::ComboBox::from_id_salt("thermal_material")
            .selected_text(current.name)
            .width(ui.available_width() - 8.0)
            .show_ui(ui, |ui| {
                for material in MATERIALS {
                    if ui
                        .selectable_label(material.key == current.key, material.name)
                        .on_hover_text(format!("k {} W/mK", material.conductivity_w_mk))
                        .clicked()
                    {
                        chosen = Some(material.key.to_owned());
                    }
                }
            });
        if let Some(key) = chosen {
            self.simulation.thermal.setup.material_key = key;
        }
        ui.label(RichText::new("RESOLUTION").small().color(theme::muted()));
        ui.horizontal(|ui| {
            for resolution in Resolution::ALL {
                let held = self.simulation.thermal.setup.resolution == resolution;
                if ui.selectable_label(held, resolution.label()).clicked() {
                    self.simulation.thermal.setup.resolution = resolution;
                }
            }
        });

        ui.add_space(6.0);
        if let Some(running) = self.simulation.thermal.running.as_ref() {
            let (phase, done) = running
                .progress
                .lock()
                .map_or(("starting", 0), |progress| (progress.phase, progress.done));
            let voxels = running.voxels.load(Ordering::Relaxed);
            ui.add(
                egui::ProgressBar::new(0.5)
                    .animate(true)
                    .text(format!("{phase} · {voxels} voxels · iteration {done}")),
            );
            if ui.button("Cancel solve").clicked() {
                self.cancel_thermal_study();
            }
        } else {
            let ready = !self.simulation.thermal.setup.faces.is_empty();
            let solve = ui.add_enabled(
                ready,
                egui::Button::new("Solve thermal study")
                    .min_size(egui::vec2(ui.available_width(), 30.0)),
            );
            solve.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Solve thermal study")
            });
            if solve
                .on_disabled_hover_text("Hold at least one face at a temperature first.")
                .clicked()
            {
                self.solve_thermal_study();
            }
        }
        if let Some(message) = self.simulation.message.clone() {
            ui.label(RichText::new(message).small().color(theme::warn()));
        }

        if let Some(outcome) = self.simulation.thermal.outcome.as_ref() {
            ui.add_space(6.0);
            let result = &outcome.result;
            crate::status_line(
                ui,
                &format!("{:.1} °C to {:.1} °C", result.min, result.max),
                theme::good(),
            );
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
                "Skin",
                &format!(
                    "{} held nodes · {} sides to the air",
                    result.held_nodes, result.convecting_sides
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
                RichText::new(
                    "Approximate. Convection is lumped at the corners of each exposed side, and the grid is a staircase of the part.",
                )
                .small()
                .color(theme::muted()),
            );
            ui.checkbox(
                &mut self.simulation.thermal.show_temperature,
                "Temperature colours",
            )
            .on_hover_text("Paint the body by temperature: red hottest, blue coldest.");
        }

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
