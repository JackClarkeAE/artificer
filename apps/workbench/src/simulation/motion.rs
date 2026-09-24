//! The motion timeline (ADR 0058, M1): the mechanism's existing joint
//! drivers and sweep under a scrubber, with the clearance at every frame
//! plotted against time and the frame two parts first meet flagged on the
//! plot and on the parts.
//!
//! No new solver: the frames are the same sixty-four positions the play
//! button and the interference sweep walk, so what is measured is exactly
//! the motion that is watched.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use artificer_compute::{JobError, JobHandle, JobPriority};
use artificer_kernel::CancellationToken;
use artificer_kernel::api::analysis::Subject;
use artificer_model::BodyId;
use artificer_sim::{MotionTimeline, clearance_timeline};
use egui::RichText;

use super::StudyKind;
use crate::{KernelLabApp, SWEEP_STEPS, theme};

/// A measured timeline and the bodies its subjects stand for, in subject
/// order.
#[derive(Clone, Debug)]
pub struct MeasuredTimeline {
    pub timeline: MotionTimeline,
    pub bodies: Vec<BodyId>,
}

/// A measurement running off the UI thread.
pub(super) struct RunningTimeline {
    job: JobHandle<MotionTimeline>,
    cancellation: CancellationToken,
    progress: Arc<AtomicUsize>,
    total: usize,
    bodies: Vec<BodyId>,
}

/// The timeline card's state.
#[derive(Default)]
pub struct MotionState {
    pub timeline: Option<MeasuredTimeline>,
    pub(super) running: Option<RunningTimeline>,
}

impl MotionState {
    pub(super) fn cancel(&mut self) {
        if let Some(running) = self.running.take() {
            running.cancellation.cancel();
        }
    }
}

/// The headline of a measured timeline, for a caller without the frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionSummary {
    pub frames: usize,
    pub first_collision: Option<usize>,
    pub tightest_frame: Option<usize>,
    pub tightest_distance: Option<f64>,
    pub cancelled: bool,
}

/// The frame a phase of the motion stands at.
fn frame_of_phase(phase: f64) -> usize {
    let turns = (phase / std::f64::consts::TAU).rem_euclid(1.0);
    ((turns * SWEEP_STEPS as f64).round() as usize) % SWEEP_STEPS
}

/// The phase a frame stands at. Frame zero is held a hair past zero, so
/// the mechanism stays posed at the start of its travel rather than
/// falling back to the joint sliders, which is what a phase of exactly
/// zero means to the animation.
fn phase_of_frame(frame: usize) -> f64 {
    if frame == 0 {
        1.0e-9
    } else {
        std::f64::consts::TAU * frame as f64 / SWEEP_STEPS as f64
    }
}

impl KernelLabApp {
    /// Opens the motion timeline card.
    pub fn open_motion_timeline(&mut self) {
        if !self.animation_drives_joints() {
            self.document_status =
                Some("A timeline needs a joint to drive; this document has none".to_owned());
            return;
        }
        self.simulation.open = Some(StudyKind::Motion);
        self.simulation.message = None;
        self.document_status = Some(format!(
            "Motion timeline · {} frames over the mechanism's travel",
            SWEEP_STEPS
        ));
    }

    /// The frame the mechanism is posed at.
    #[must_use]
    pub fn motion_frame(&self) -> usize {
        frame_of_phase(self.motion.phase)
    }

    /// Poses the mechanism at one frame and holds it there.
    pub fn set_motion_frame(&mut self, frame: usize) {
        self.motion.playing = false;
        self.motion.phase = phase_of_frame(frame % SWEEP_STEPS);
        self.last_motion_time = None;
    }

    /// Plays the motion from the frame it is at.
    pub fn play_motion_timeline(&mut self, context: &egui::Context) {
        self.motion.playing = true;
        self.last_motion_time = None;
        context.request_repaint();
    }

    /// Holds the motion at the frame it has reached.
    pub fn pause_motion_timeline(&mut self) {
        self.motion.playing = false;
        self.last_motion_time = None;
    }

    /// Measures the clearance at every frame, off the UI thread where
    /// there is a scheduler and inline where there is not.
    pub fn measure_motion_clearance(&mut self) {
        if self.simulation.motion.running.is_some() {
            return;
        }
        let joints = self.drivable_joints();
        if joints.is_empty() {
            self.simulation.message = Some("No joint to drive".to_owned());
            return;
        }
        let bodies = self
            .bodies
            .iter()
            .filter(|body| body.visible)
            .map(|body| body.id)
            .collect::<Vec<_>>();
        if bodies.len() < 2 {
            self.simulation.message =
                Some("A timeline measures between two visible bodies; show another".to_owned());
            return;
        }
        let subjects = self
            .bodies
            .iter()
            .filter(|body| body.visible)
            .map(|body| Subject::new(format!("Body {}", body.ordinal), body.body.snapshot.clone()))
            .collect::<Vec<_>>();
        let Some(steps) = self.sweep_steps(&joints, &bodies) else {
            self.simulation.message =
                Some("The mechanism could not be posed through its travel".to_owned());
            return;
        };
        let total = steps.len();
        let precision = self.document_precision();
        let cancellation = CancellationToken::new();
        let progress = Arc::new(AtomicUsize::new(0));
        self.simulation.message = None;
        let Some(scheduler) = self.feature_preview_scheduler.as_ref() else {
            let timeline =
                clearance_timeline(&subjects, &steps, precision, &cancellation, &mut |_, _| {});
            self.take_timeline(timeline, bodies);
            return;
        };
        let job_cancellation = cancellation.clone();
        let reported = Arc::clone(&progress);
        let job = scheduler.submit(JobPriority::Commit, None, move |_| {
            clearance_timeline(
                &subjects,
                &steps,
                precision,
                &job_cancellation,
                &mut |step, _| reported.store(step, Ordering::Relaxed),
            )
        });
        self.simulation.motion.running = Some(RunningTimeline {
            job,
            cancellation,
            progress,
            total,
            bodies,
        });
        self.document_status = Some(format!("Measuring {total} frames…"));
    }

    fn take_timeline(&mut self, timeline: MotionTimeline, bodies: Vec<BodyId>) {
        self.document_status = Some(match timeline.first_collision {
            Some(frame) => format!(
                "Collision at frame {} of {}",
                frame + 1,
                timeline.steps_offered
            ),
            None => timeline.tightest_clear().map_or_else(
                || format!("No contact over {} frames", timeline.steps_measured),
                |tightest| {
                    format!(
                        "Clear over {} frames; closest {} at frame {}",
                        timeline.steps_measured,
                        self.length_unit().format(tightest.distance),
                        tightest.step + 1
                    )
                },
            ),
        });
        if timeline.cancelled {
            self.simulation.message = Some(format!(
                "Stopped after {} of {} frames; the rest is unmeasured",
                timeline.steps_measured, timeline.steps_offered
            ));
        }
        self.simulation.motion.timeline = Some(MeasuredTimeline { timeline, bodies });
    }

    /// Stops a running measurement, keeping the frames it reached.
    pub fn cancel_motion_measurement(&mut self) {
        if let Some(running) = self.simulation.motion.running.as_ref() {
            running.cancellation.cancel();
        }
    }

    pub(super) fn poll_motion_timeline(&mut self, context: &egui::Context) {
        let Some(running) = self.simulation.motion.running.as_ref() else {
            return;
        };
        match running.job.try_take() {
            None => {
                let reached = running.progress.load(Ordering::Relaxed).min(running.total);
                self.document_status =
                    Some(format!("Measuring frame {reached} of {}…", running.total));
                context.request_repaint();
            }
            Some(finished) => {
                let running = self
                    .simulation
                    .motion
                    .running
                    .take()
                    .expect("a finished measurement");
                match finished {
                    Ok(timeline) => self.take_timeline(timeline, running.bodies),
                    Err(JobError::Cancelled) => {
                        self.simulation.message = Some("Measurement cancelled".to_owned());
                    }
                    Err(error) => {
                        self.simulation.message =
                            Some(format!("The measurement failed: {error:?}"));
                    }
                }
                context.request_repaint();
            }
        }
    }

    /// The headline of the measured timeline.
    #[must_use]
    pub fn motion_timeline_summary(&self) -> Option<MotionSummary> {
        let measured = self.simulation.motion.timeline.as_ref()?;
        let tightest = measured.timeline.tightest_clear();
        Some(MotionSummary {
            frames: measured.timeline.frames.len(),
            first_collision: measured.timeline.first_collision,
            tightest_frame: tightest.map(|frame| frame.step),
            tightest_distance: tightest.map(|frame| frame.distance),
            cancelled: measured.timeline.cancelled,
        })
    }

    /// The bodies flagged at the current frame: the pair that may share
    /// space there, or nothing while the mechanism is clear.
    #[must_use]
    pub fn motion_flagged_bodies(&self) -> Vec<BodyId> {
        let Some(measured) = self.simulation.motion.timeline.as_ref() else {
            return Vec::new();
        };
        if self.simulation.open != Some(StudyKind::Motion) {
            return Vec::new();
        }
        let frame = self.motion_frame();
        let Some(reading) = measured.timeline.frames.get(frame) else {
            return Vec::new();
        };
        if !reading.may_overlap {
            return Vec::new();
        }
        [reading.pair.0, reading.pair.1]
            .into_iter()
            .filter_map(|subject| measured.bodies.get(subject).copied())
            .collect()
    }

    /// The timeline card.
    pub(super) fn motion_card(&mut self, ui: &mut egui::Ui) {
        let joints = self.drivable_joints();
        crate::status_line(
            ui,
            &format!(
                "Motion timeline · {} joint{}",
                joints.len(),
                if joints.len() == 1 { "" } else { "s" }
            ),
            theme::accent(),
        );
        let frame = self.motion_frame();

        // ---- transport ------------------------------------------------------
        ui.horizontal(|ui| {
            let playing = self.motion.playing;
            let play = ui.add_sized(
                [76.0, 26.0],
                egui::Button::new(if playing { "Pause" } else { "Play" }),
            );
            play.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    if playing {
                        "Pause timeline"
                    } else {
                        "Play timeline"
                    },
                )
            });
            if play.clicked() {
                if playing {
                    self.pause_motion_timeline();
                } else {
                    self.play_motion_timeline(ui.ctx());
                }
            }
            let stop = ui.add_sized([60.0, 26.0], egui::Button::new("Stop"));
            stop.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Stop timeline")
            });
            if stop
                .on_hover_text("Return the mechanism to the pose its joints are set to.")
                .clicked()
            {
                self.motion.pause();
                self.last_motion_time = None;
            }
        });
        let mut speed = self.motion.speed_rpm;
        if ui
            .add(
                egui::Slider::new(&mut speed, -30.0..=30.0)
                    .text("Speed")
                    .suffix(" rpm"),
            )
            .changed()
        {
            self.motion.set_speed_rpm(speed);
        }
        let mut scrub = frame;
        let scrubber = ui.add(
            egui::Slider::new(&mut scrub, 0..=SWEEP_STEPS - 1)
                .text("Frame")
                .show_value(true),
        );
        if scrubber.changed() {
            self.set_motion_frame(scrub);
        }
        for joint in &joints {
            let angle = self.animated_joint_angle(joint.id, joint.limits);
            ui.label(
                RichText::new(format!("{}: {:.1}°", joint.name, angle.to_degrees()))
                    .small()
                    .color(theme::muted()),
            );
        }

        // ---- measurement ----------------------------------------------------
        ui.add_space(6.0);
        if let Some(running) = self.simulation.motion.running.as_ref() {
            let reached = running.progress.load(Ordering::Relaxed).min(running.total);
            ui.add(
                egui::ProgressBar::new(reached as f32 / running.total.max(1) as f32)
                    .text(format!("Measuring {reached} / {}", running.total)),
            );
            if ui.button("Stop measuring").clicked() {
                self.cancel_motion_measurement();
            }
        } else {
            let measure = ui.add_sized(
                [ui.available_width(), 28.0],
                egui::Button::new("Measure clearance"),
            );
            measure.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Measure clearance")
            });
            if measure
                .on_hover_text(
                    "Pose the mechanism at every frame and measure how close the parts come. \
                     The frame two parts first meet is marked on the plot and on the parts.",
                )
                .clicked()
            {
                self.measure_motion_clearance();
            }
        }
        if let Some(message) = self.simulation.message.clone() {
            ui.label(RichText::new(message).small().color(theme::warn()));
        }

        if let Some(measured) = self.simulation.motion.timeline.as_ref() {
            let timeline = &measured.timeline;
            let unit = self.length_unit();
            ui.add_space(4.0);
            let (colour, headline) = match timeline.first_collision {
                Some(collision) => (
                    theme::bad(),
                    format!(
                        "Collision at frame {} of {}",
                        collision + 1,
                        timeline.steps_offered
                    ),
                ),
                None => (
                    theme::good(),
                    format!("Clear over {} frames", timeline.steps_measured),
                ),
            };
            crate::status_line(ui, &headline, colour);
            if let Some(tightest) = timeline.tightest_clear() {
                ui.label(
                    RichText::new(format!(
                        "Closest {} at frame {} ({:.1}°)",
                        unit.format(tightest.distance),
                        tightest.step + 1,
                        tightest
                            .drivers
                            .first()
                            .copied()
                            .unwrap_or(0.0)
                            .to_degrees()
                    ))
                    .small()
                    .color(theme::muted()),
                );
            }
            clearance_plot(ui, timeline, frame, unit);
            if let Some(reading) = timeline.frames.get(frame) {
                let (colour, text) = if reading.may_overlap {
                    (
                        theme::bad(),
                        format!(
                            "Frame {}: Body {} and Body {} share space",
                            frame + 1,
                            reading.pair.0 + 1,
                            reading.pair.1 + 1
                        ),
                    )
                } else {
                    (
                        theme::text(),
                        format!(
                            "Frame {}: {} clear",
                            frame + 1,
                            unit.format(reading.distance)
                        ),
                    )
                };
                ui.label(RichText::new(text).small().color(colour));
            }
        }

        ui.add_space(4.0);
        let dismiss = ui.button("Dismiss timeline");
        dismiss.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Dismiss timeline")
        });
        if dismiss.clicked() {
            self.motion.pause();
            self.simulation.dismiss();
        }
    }
}

/// Minimum clearance against frame, the collision frames banded red and
/// the current frame marked.
fn clearance_plot(
    ui: &mut egui::Ui,
    timeline: &MotionTimeline,
    current: usize,
    unit: crate::units::LengthUnit,
) {
    let width = ui.available_width() - 4.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 64.0), egui::Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Clearance over the motion")
    });
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, theme::card());
    let frames = timeline.steps_offered.max(1);
    let x_of = |frame: usize| rect.left() + rect.width() * frame as f32 / frames as f32;
    let largest = timeline.largest().max(1.0e-6);
    let y_of = |distance: f64| {
        let clamped = if distance.is_finite() {
            distance.max(0.0) / largest
        } else {
            1.0
        };
        rect.bottom() - 6.0 - (rect.height() - 12.0) * clamped as f32
    };
    // Frames that may share space are a red band under the curve.
    for reading in &timeline.frames {
        if reading.may_overlap {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x_of(reading.step), rect.top()),
                    egui::pos2(x_of(reading.step + 1), rect.bottom()),
                ),
                0.0,
                theme::bad().gamma_multiply(0.35),
            );
        }
    }
    let points = timeline
        .frames
        .iter()
        .map(|reading| egui::pos2(x_of(reading.step), y_of(reading.distance)))
        .collect::<Vec<_>>();
    if points.len() >= 2 {
        painter.add(egui::Shape::line(
            points,
            egui::Stroke::new(1.5, theme::accent()),
        ));
    }
    let x = x_of(current);
    painter.line_segment(
        [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
        egui::Stroke::new(1.0, theme::text()),
    );
    painter.text(
        rect.left_top() + egui::vec2(4.0, 2.0),
        egui::Align2::LEFT_TOP,
        unit.format(largest),
        egui::FontId::monospace(9.0),
        theme::muted(),
    );
    painter.text(
        rect.left_bottom() + egui::vec2(4.0, -2.0),
        egui::Align2::LEFT_BOTTOM,
        "0",
        egui::FontId::monospace(9.0),
        theme::muted(),
    );
}
