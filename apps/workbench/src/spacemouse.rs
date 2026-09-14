//! 3Dconnexion SpaceMouse navigation for the workbench.
//!
//! One puck serves every document. The device is opened once per process,
//! when the first document is made, and each document afterwards holds a
//! handle to the same reader; only the document in front runs its frame
//! logic, so the motion a frame takes goes to the view the user is looking
//! at. The reader's wake-up asks the egui context to repaint, so a nudge of
//! the cap is drawn without the application polling on a timer.
//!
//! The camera mapping itself lives with the camera, in
//! [`ViewState::apply_six_dof`]; this module owns the device, the user's
//! settings, and the two buttons: button 1 fits and resets the view, and
//! button 2 flips between the view now and the view it was pressed in last,
//! which is the cheapest useful bookmark a puck can offer.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use artificer_spacemouse::{DeviceStatus, Motion, SpaceMouse};
use egui::RichText;

use crate::presentation::{SixDofFilter, SixDofMotion, SixDofSettings, ViewState};
use crate::{KernelLabApp, WorkbenchMode, status_line, theme};

/// The one reader per process, shared by every document.
static SHARED_DEVICE: OnceLock<Option<Arc<SpaceMouse>>> = OnceLock::new();

/// The puck, the settings it is driven with, and the bookmark its second
/// button flips to.
#[derive(Debug, Default)]
pub struct SpaceMouseNavigation {
    device: Option<Arc<SpaceMouse>>,
    /// How the cap steers the camera. A user preference in spirit; held
    /// per document for now, starting from the defaults.
    pub settings: SixDofSettings,
    bookmark: Option<ViewState>,
    /// The motion the camera actually follows: the raw reports, shaped and
    /// low-passed, so a burst of reports and a slow frame do not stair-step
    /// the view.
    filter: SixDofFilter,
    /// When the frame loop last took motion, so each frame integrates the
    /// time that really passed rather than the renderer's estimate.
    last_poll: Option<Instant>,
}

/// How soon the next frame is asked for while the cap is deflected or the
/// filter is still settling: a steady cadence for the integration, rather
/// than one repaint per report burst.
const FOLLOW_UP_FRAME: Duration = Duration::from_millis(8);

impl SpaceMouseNavigation {
    /// Attaches to the process's puck, opening it on the first call. The
    /// wake-up holds a clone of `context` and asks it to repaint whenever
    /// the cap moves or a button goes down.
    #[must_use]
    pub fn attach(context: &egui::Context) -> Self {
        let device = SHARED_DEVICE
            .get_or_init(|| {
                let context = context.clone();
                SpaceMouse::open_with_wake(move || context.request_repaint()).map(Arc::new)
            })
            .clone();
        Self {
            device,
            ..Self::default()
        }
    }

    /// What the reader knows about the device, or `None` when HID input is
    /// unavailable on this system (or in a test, which never opens it).
    #[must_use]
    pub fn status(&self) -> Option<DeviceStatus> {
        self.device.as_ref().map(|device| device.status())
    }

    /// Whether a puck is attached and reporting.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.device
            .as_ref()
            .is_some_and(|device| device.is_connected())
    }

    /// The motion since the last frame, or `None` without a device.
    #[must_use]
    pub fn take_motion(&self) -> Option<Motion> {
        self.device.as_ref().map(|device| device.take_motion())
    }
}

impl KernelLabApp {
    /// Applies whatever the puck reported since the last frame. Called once
    /// per frame from the application's logic pass.
    pub(crate) fn poll_spacemouse(&mut self, context: &egui::Context) {
        let Some(raw) = self.spacemouse.take_motion() else {
            return;
        };
        let now = Instant::now();
        let seconds = self.spacemouse.last_poll.map_or(1.0 / 60.0, |previous| {
            now.duration_since(previous).as_secs_f64()
        });
        self.spacemouse.last_poll = Some(now);
        if raw.is_empty() && self.spacemouse.filter.is_still() {
            return;
        }
        let settings = self.spacemouse.settings;
        let steered = self.spacemouse.filter.feed(
            SixDofMotion {
                translate: raw.translate,
                rotate: raw.rotate,
            },
            seconds,
            &settings,
        );
        let motion = Motion {
            translate: steered.translate,
            rotate: steered.rotate,
            buttons_pressed: raw.buttons_pressed,
        };
        self.apply_spacemouse_motion(motion, seconds, context);
        if !self.spacemouse.filter.is_still() {
            // Keep the frames coming at a steady cadence until the filter
            // settles, whether or not the puck reports again meanwhile.
            context.request_repaint_after(FOLLOW_UP_FRAME);
        }
    }

    /// The camera's response to one frame of puck motion held for
    /// `seconds`. Public so the tests can drive it without a puck; a
    /// document with no device never reaches it from the frame loop.
    ///
    /// The puck steers only the three-dimensional model view: while a
    /// sketch canvas is up, or the camera is mid-flight to a face, the
    /// motion is dropped rather than fought over.
    pub fn apply_spacemouse_motion(
        &mut self,
        motion: Motion,
        seconds: f64,
        context: &egui::Context,
    ) -> bool {
        if self.workbench_mode != WorkbenchMode::Model || self.face_camera_transition.is_some() {
            return false;
        }
        let mut changed = false;
        if motion.button_pressed(1) {
            self.reset_view(context);
            self.frame_visible_document();
            changed = true;
        }
        if motion.button_pressed(2) {
            let now = self.view;
            if let Some(previous) = self.spacemouse.bookmark.replace(now) {
                self.view = previous;
            }
            changed = true;
        }
        let six_dof = SixDofMotion {
            translate: motion.translate,
            rotate: motion.rotate,
        };
        if self
            .view
            .apply_six_dof(six_dof, seconds, self.spacemouse.settings)
        {
            changed = true;
        }
        if changed {
            context.request_repaint();
        }
        changed
    }

    /// The puck's settings, as the About card shows them.
    #[must_use]
    pub const fn spacemouse_settings(&self) -> SixDofSettings {
        self.spacemouse.settings
    }

    pub fn set_spacemouse_settings(&mut self, settings: SixDofSettings) {
        self.spacemouse.settings = settings;
    }

    /// What the reader knows about the device; `None` when there is no
    /// reader at all.
    #[must_use]
    pub fn spacemouse_status(&self) -> Option<DeviceStatus> {
        self.spacemouse.status()
    }

    /// The 3D mouse card inside About: whether a puck was found, and the
    /// few settings worth a control.
    pub(crate) fn spacemouse_section(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(
            RichText::new("3D MOUSE")
                .small()
                .strong()
                .color(theme::muted()),
        );
        let status = self.spacemouse.status();
        let (text, colour) = match &status {
            None => (
                "3D mouse support is unavailable on this system".to_owned(),
                theme::muted(),
            ),
            Some(status @ DeviceStatus::Connected { .. }) => (status.describe(), theme::good()),
            Some(status @ DeviceStatus::Searching) => (status.describe(), theme::muted()),
            Some(status) => (status.describe(), theme::warn()),
        };
        status_line(ui, &text, colour);
        if cfg!(target_os = "linux") && matches!(status, Some(DeviceStatus::AccessDenied { .. })) {
            ui.label(
                RichText::new(
                    "Linux gives the raw HID node to root only. Grant it to users with a udev \
                     rule in /etc/udev/rules.d/70-spacemouse.rules (see the README), then \
                     replug the device.",
                )
                .small()
                .color(theme::muted()),
            );
        }
        ui.add_space(4.0);
        let settings = &mut self.spacemouse.settings;
        let slider = ui.add(
            egui::Slider::new(
                &mut settings.sensitivity,
                SixDofSettings::MIN_SENSITIVITY..=SixDofSettings::MAX_SENSITIVITY,
            )
            .logarithmic(true)
            .fixed_decimals(2)
            .text("Sensitivity"),
        );
        slider.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, "3D mouse sensitivity")
        });
        slider.on_hover_text("How far the view moves for a given push of the cap");
        ui.checkbox(&mut settings.object_mode, "The cap moves the model")
            .on_hover_text(
                "On, the model follows the cap as if held in the hand. Off, the cap flies the \
                 camera through the scene instead, which reverses every axis.",
            );
        ui.checkbox(
            &mut settings.roll_enabled,
            "Twisting the cap sideways rolls the view",
        )
        .on_hover_text("Rotation about the viewing axis. Off keeps the horizon level.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_to_sane_values() {
        let navigation = SpaceMouseNavigation::default();
        assert!(navigation.status().is_none());
        assert!(!navigation.is_connected());
        assert!(navigation.take_motion().is_none());
        let settings = navigation.settings;
        assert_eq!(settings, SixDofSettings::default());
        assert_eq!(settings.sensitivity, 1.0);
        assert!(settings.rotate_rate > 0.0 && settings.rotate_rate < 4.0);
        assert!(settings.pan_rate > 0.0 && settings.pan_rate < 4.0);
        assert!(settings.zoom_rate > 0.0 && settings.zoom_rate < 4.0);
        assert_eq!(settings.invert_translate, [false; 3]);
        assert_eq!(settings.invert_rotate, [false; 3]);
        assert!(!settings.roll_enabled);
        assert!(settings.object_mode);
        assert_eq!(settings.bounded_sensitivity(), 1.0);
    }

    #[test]
    fn a_document_without_a_device_ignores_the_frame_loop() {
        let context = egui::Context::default();
        let mut app = KernelLabApp::default();
        let before = app.view_parameters();
        app.poll_spacemouse(&context);
        assert_eq!(app.view_parameters(), before);
        assert!(app.spacemouse_status().is_none());
    }

    #[test]
    fn motion_applied_by_hand_steers_the_model_view_only() {
        let context = egui::Context::default();
        let mut app = KernelLabApp::default();
        let before = app.view_parameters();
        let twist = Motion {
            rotate: [0.0, 1.0, 0.0],
            ..Motion::default()
        };
        assert!(app.apply_spacemouse_motion(twist, 1.0 / 60.0, &context));
        let (yaw, pitch, zoom) = app.view_parameters();
        assert_ne!(yaw, before.0);
        assert_eq!(pitch, before.1);
        assert_eq!(zoom, before.2);
        // A still cap changes nothing, and says so.
        assert!(!app.apply_spacemouse_motion(Motion::default(), 1.0 / 60.0, &context));
    }

    #[test]
    fn the_buttons_reset_and_bookmark_the_view() {
        let context = egui::Context::default();
        let mut app = KernelLabApp::default();
        let home = app.view_parameters();
        let twist = Motion {
            rotate: [0.4, 1.0, 0.0],
            ..Motion::default()
        };
        app.apply_spacemouse_motion(twist, 0.1, &context);
        let turned = app.view_parameters();
        assert_ne!(turned, home);

        // Button 2 pressed once bookmarks the turned view without moving.
        let bookmark = Motion {
            buttons_pressed: 0b10,
            ..Motion::default()
        };
        assert!(app.apply_spacemouse_motion(bookmark, 0.1, &context));
        assert_eq!(app.view_parameters(), turned);

        // Button 1 goes home; button 2 then returns to the bookmark, and
        // again flips back.
        let reset = Motion {
            buttons_pressed: 0b01,
            ..Motion::default()
        };
        assert!(app.apply_spacemouse_motion(reset, 0.1, &context));
        assert_eq!(app.view_parameters(), home);
        app.apply_spacemouse_motion(bookmark, 0.1, &context);
        assert_eq!(app.view_parameters(), turned);
        app.apply_spacemouse_motion(bookmark, 0.1, &context);
        assert_eq!(app.view_parameters(), home);
    }
}
