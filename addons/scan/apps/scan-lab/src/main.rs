//! Transmute lab: the scanner simulator with knobs.
//!
//! Load an ideal mesh (CAD export, synthetic part), drag the scanner's
//! parameters — sample density, spot radius, noise, dropout — and watch
//! the scan it would produce, side by side with the original under one
//! orbiting camera. The preview simulates a decimated copy so sliders
//! answer at interactive rates; **Save** runs the full-resolution mesh
//! through the same deterministic pipeline as `artificer-scan
//! simulate`, so a saved fixture is exactly what the CLI would have
//! produced with the same seed and options.
//!
//! Everything renders through `scan-core`'s software rasterizer — the
//! same camera and shading as the snapshot images — so what the lab
//! shows is what CI renders.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use artificer_scan_core::TriangleMesh;
use artificer_scan_core::render::{Camera, render_comparison_rgb};
use artificer_scan_core::simulate::{SimulateOptions, simulate_scan};

/// Preview meshes stay under this many triangles so a slider drag
/// resimulates in tens of milliseconds. Display-only — saving always
/// runs the full mesh.
const PREVIEW_TRIANGLES: usize = 90_000;

enum Job {
    Load {
        path: PathBuf,
        generation: u64,
    },
    Simulate {
        mesh: Arc<TriangleMesh>,
        options: SimulateOptions,
        generation: u64,
        save_to: Option<PathBuf>,
    },
}

enum Done {
    Loaded {
        original: Arc<TriangleMesh>,
        preview: Arc<TriangleMesh>,
        path: PathBuf,
        generation: u64,
    },
    Simulated {
        mesh: Arc<TriangleMesh>,
        generation: u64,
        elapsed: Duration,
        saved_to: Option<PathBuf>,
        notes: Vec<String>,
    },
    Failed {
        what: String,
        generation: u64,
    },
}

fn load_mesh(path: &PathBuf) -> Result<TriangleMesh, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("stl") => artificer_scan_core::stl::read_stl(&bytes).map_err(|e| e.to_string()),
        Some("ply") => artificer_scan_core::ply::read_ply(&bytes).map_err(|e| e.to_string()),
        Some("obj") => artificer_scan_core::obj::read_obj(&bytes).map_err(|e| e.to_string()),
        Some("step" | "stp") => artificer_scan_core::step::read_step(&bytes, 0.03)
            .map(|(mesh, _notes)| mesh)
            .map_err(|e| e.to_string()),
        _ => Err(format!(
            "unsupported format for {} (expected .stl, .ply, .obj, or .step)",
            path.display()
        )),
    }
}

/// Decimates for display until the preview budget holds.
fn preview_of(mesh: &TriangleMesh) -> TriangleMesh {
    if mesh.triangles().len() <= PREVIEW_TRIANGLES {
        return mesh.clone();
    }
    let mut cell = mesh.bounds_diagonal() / 320.0;
    let mut result = mesh.simplified_by_clustering(cell);
    while result.0.triangles().len() > PREVIEW_TRIANGLES {
        cell *= 1.35;
        result = mesh.simplified_by_clustering(cell);
    }
    result.0
}

fn worker(jobs: Receiver<Job>, done: Sender<Done>, ctx: egui::Context) {
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Load { path, generation } => {
                let sent = match load_mesh(&path) {
                    Ok(mesh) => {
                        let preview = preview_of(&mesh);
                        done.send(Done::Loaded {
                            original: Arc::new(mesh),
                            preview: Arc::new(preview),
                            path,
                            generation,
                        })
                    }
                    Err(error) => done.send(Done::Failed {
                        what: error,
                        generation,
                    }),
                };
                if sent.is_err() {
                    return;
                }
            }
            Job::Simulate {
                mesh,
                options,
                generation,
                save_to,
            } => {
                let started = Instant::now();
                let scan = simulate_scan(&mesh, &options);
                let mut saved_to = None;
                let result = if let Some(path) = save_to {
                    match std::fs::write(
                        &path,
                        artificer_scan_core::stl::write_binary_stl(&scan.mesh),
                    ) {
                        Ok(()) => {
                            saved_to = Some(path);
                            None
                        }
                        Err(error) => Some(format!("cannot write {}: {error}", path.display())),
                    }
                } else {
                    None
                };
                let sent = match result {
                    Some(error) => done.send(Done::Failed {
                        what: error,
                        generation,
                    }),
                    None => done.send(Done::Simulated {
                        mesh: Arc::new(scan.mesh),
                        generation,
                        elapsed: started.elapsed(),
                        saved_to,
                        notes: scan.notes,
                    }),
                };
                if sent.is_err() {
                    return;
                }
            }
        }
        ctx.request_repaint();
    }
}

struct LabApp {
    jobs: Sender<Job>,
    done: Receiver<Done>,
    source: Option<PathBuf>,
    original: Option<Arc<TriangleMesh>>,
    preview: Option<Arc<TriangleMesh>>,
    simulated: Option<Arc<TriangleMesh>>,
    options: SimulateOptions,
    /// Preview simulations skip refinement when the preview is already
    /// denser than the target — refining a decimated mesh only spends
    /// time re-adding triangles decimation removed.
    live: bool,
    camera: Camera,
    texture: Option<egui::TextureHandle>,
    view_dirty: bool,
    sim_dirty: bool,
    last_change: Instant,
    generation: u64,
    in_flight: Option<u64>,
    computing_full: bool,
    last_elapsed: Option<Duration>,
    notes: Vec<String>,
    status: String,
    save_path: String,
}

impl LabApp {
    fn new(ctx: &egui::Context, initial: Option<PathBuf>) -> Self {
        let (job_sender, job_receiver) = channel();
        let (done_sender, done_receiver) = channel();
        let thread_ctx = ctx.clone();
        std::thread::spawn(move || worker(job_receiver, done_sender, thread_ctx));
        let mut app = LabApp {
            jobs: job_sender,
            done: done_receiver,
            source: None,
            original: None,
            preview: None,
            simulated: None,
            options: SimulateOptions::default(),
            live: true,
            camera: Camera::default(),
            texture: None,
            view_dirty: true,
            sim_dirty: false,
            last_change: Instant::now(),
            generation: 0,
            in_flight: None,
            computing_full: false,
            last_elapsed: None,
            notes: Vec::new(),
            status: "drop a mesh file here, or pass one on the command line".to_owned(),
            save_path: String::new(),
        };
        if let Some(path) = initial {
            app.request_load(path);
        }
        app
    }

    fn request_load(&mut self, path: PathBuf) {
        self.generation += 1;
        self.status = format!("loading {} ...", path.display());
        self.save_path = path
            .with_file_name(format!(
                "{}_scanned.stl",
                path.file_stem().and_then(|s| s.to_str()).unwrap_or("mesh")
            ))
            .display()
            .to_string();
        let _ = self.jobs.send(Job::Load {
            path,
            generation: self.generation,
        });
    }

    /// The options a preview run uses: identical physics, minus
    /// refinement when the decimated preview is already at or below
    /// the target density.
    fn preview_options(&self) -> SimulateOptions {
        SimulateOptions {
            density: 0.0,
            ..self.options
        }
    }

    fn request_preview_sim(&mut self) {
        let Some(preview) = &self.preview else { return };
        self.generation += 1;
        self.in_flight = Some(self.generation);
        let _ = self.jobs.send(Job::Simulate {
            mesh: preview.clone(),
            options: self.preview_options(),
            generation: self.generation,
            save_to: None,
        });
    }

    fn request_full_save(&mut self) {
        let Some(original) = &self.original else {
            return;
        };
        self.generation += 1;
        self.in_flight = Some(self.generation);
        self.computing_full = true;
        self.status = format!(
            "simulating the full {} triangles ...",
            original.triangles().len()
        );
        let _ = self.jobs.send(Job::Simulate {
            mesh: original.clone(),
            options: self.options,
            generation: self.generation,
            save_to: Some(PathBuf::from(self.save_path.clone())),
        });
    }

    fn drain_results(&mut self) {
        while let Ok(result) = self.done.try_recv() {
            match result {
                Done::Loaded {
                    original,
                    preview,
                    path,
                    generation,
                } => {
                    if generation < self.generation {
                        continue;
                    }
                    self.status = format!(
                        "{}: {} triangles ({} in preview)",
                        path.display(),
                        original.triangles().len(),
                        preview.triangles().len()
                    );
                    self.source = Some(path);
                    self.original = Some(original);
                    self.preview = Some(preview);
                    self.simulated = None;
                    self.view_dirty = true;
                    self.sim_dirty = true;
                    self.last_change = Instant::now();
                }
                Done::Simulated {
                    mesh,
                    generation,
                    elapsed,
                    saved_to,
                    notes,
                } => {
                    if self.in_flight == Some(generation) {
                        self.in_flight = None;
                    }
                    if generation + 8 < self.generation {
                        continue;
                    }
                    self.last_elapsed = Some(elapsed);
                    self.notes = notes;
                    if let Some(path) = saved_to {
                        self.computing_full = false;
                        self.status = format!(
                            "saved {} ({} triangles)",
                            path.display(),
                            mesh.triangles().len()
                        );
                        // A full-resolution result also makes the truest
                        // preview available; show it.
                        self.simulated = Some(Arc::new(preview_of(&mesh)));
                    } else {
                        self.simulated = Some(mesh);
                    }
                    self.view_dirty = true;
                }
                Done::Failed { what, generation } => {
                    if self.in_flight == Some(generation) {
                        self.in_flight = None;
                    }
                    self.computing_full = false;
                    self.status = what;
                }
            }
        }
    }

    fn render_view(&mut self, ctx: &egui::Context) {
        let Some(preview) = &self.preview else { return };
        let right = self.simulated.as_ref().unwrap_or(preview);
        let frame = render_comparison_rgb(preview, right, None, &self.camera, 1560, 660);
        let mut rgb = Vec::with_capacity(frame.width * frame.height * 3);
        for pixel in &frame.color {
            rgb.extend_from_slice(pixel);
        }
        let image = egui::ColorImage::from_rgb([frame.width, frame.height], &rgb);
        match &mut self.texture {
            Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
            None => {
                self.texture =
                    Some(ctx.load_texture("lab-view", image, egui::TextureOptions::LINEAR));
            }
        }
        self.view_dirty = false;
    }
}

impl eframe::App for LabApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_results();
        // Dropped files load the mesh they name.
        let dropped: Option<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .find_map(|file| file.path.clone())
        });
        if let Some(path) = dropped {
            self.request_load(path);
        }
        // Debounced live resimulation: knobs settle for 150 ms, then
        // one job goes out; a stale result is dropped by generation.
        if self.live
            && self.sim_dirty
            && self.in_flight.is_none()
            && self.last_change.elapsed() > Duration::from_millis(150)
        {
            self.sim_dirty = false;
            self.request_preview_sim();
        }
        if self.sim_dirty || self.in_flight.is_some() {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);

        egui::Panel::right("controls")
            .exact_size(310.0)
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.heading("transmute scan lab");
                ui.label(egui::RichText::new(&self.status).weak());
                ui.separator();
                let mut changed = false;
                ui.label("sample density (mm) — full run only");
                changed |= ui
                    .add(egui::Slider::new(&mut self.options.density, 0.05..=2.0).logarithmic(true))
                    .changed();
                ui.label("spot radius (mm) — rounds every crease");
                changed |= ui
                    .add(egui::Slider::new(&mut self.options.smooth, 0.0..=3.0))
                    .changed();
                ui.label("surface noise sigma (mm)");
                changed |= ui
                    .add(egui::Slider::new(&mut self.options.noise, 0.0..=0.3))
                    .changed();
                ui.label("dropout holes");
                changed |= ui
                    .add(egui::Slider::new(&mut self.options.dropout, 0..=25))
                    .changed();
                ui.label("dropout size (mm)");
                changed |= ui
                    .add(egui::Slider::new(
                        &mut self.options.dropout_size,
                        1.0..=40.0,
                    ))
                    .changed();
                ui.horizontal(|ui| {
                    ui.label("seed");
                    changed |= ui
                        .add(egui::DragValue::new(&mut self.options.seed))
                        .changed();
                });
                ui.separator();
                ui.checkbox(&mut self.live, "simulate while dragging");
                if changed {
                    self.sim_dirty = true;
                    self.last_change = Instant::now();
                }
                if !self.live && ui.button("simulate preview").clicked() {
                    self.request_preview_sim();
                }
                ui.separator();
                ui.label("save full-resolution scan to:");
                ui.text_edit_singleline(&mut self.save_path);
                let can_save = self.original.is_some() && !self.computing_full;
                if ui
                    .add_enabled(can_save, egui::Button::new("save STL (full mesh)"))
                    .clicked()
                {
                    self.request_full_save();
                }
                if self.in_flight.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(if self.computing_full {
                            "simulating full mesh ..."
                        } else {
                            "simulating preview ..."
                        });
                    });
                } else if let Some(elapsed) = self.last_elapsed {
                    ui.label(
                        egui::RichText::new(format!("last run {} ms", elapsed.as_millis())).weak(),
                    );
                }
                ui.separator();
                for note in &self.notes {
                    ui.label(egui::RichText::new(note).weak().small());
                }
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "preview simulates the decimated display mesh; save runs the \
                         full mesh with the same seed — identical to the CLI's output",
                    )
                    .weak()
                    .small(),
                );
            });

        egui::CentralPanel::default().show(ui, |ui| {
            if self.texture.is_none() && self.preview.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label("drop an .stl / .ply / .obj here");
                });
                return;
            }
            if self.view_dirty {
                self.render_view(&ui.ctx().clone());
            }
            if let Some(texture) = &self.texture {
                let available = ui.available_size();
                let response = ui.add(
                    egui::Image::from_texture(&*texture)
                        .fit_to_exact_size(available)
                        .sense(egui::Sense::drag()),
                );
                if response.dragged() {
                    let delta = response.drag_delta();
                    self.camera.theta -= delta.x as f64 * 0.008;
                    self.camera.phi = (self.camera.phi - delta.y as f64 * 0.008)
                        .clamp(0.05, std::f64::consts::PI - 0.05);
                    self.view_dirty = true;
                }
                if response.hovered() && scroll.abs() > 0.0 {
                    self.camera.radius_scale = (self.camera.radius_scale
                        * (1.0 - scroll as f64 * 0.0015))
                        .clamp(0.25, 4.0);
                    self.view_dirty = true;
                }
            }
        });
    }
}

fn main() -> eframe::Result {
    let initial = std::env::args().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1780.0, 860.0])
            .with_title("transmute scan lab"),
        ..Default::default()
    };
    eframe::run_native(
        "transmute-scan-lab",
        options,
        Box::new(move |cc| Ok(Box::new(LabApp::new(&cc.egui_ctx, initial)))),
    )
}
