//! In-app updates, backed by Velopack.
//!
//! The workbench ships as a Velopack package on Windows and Linux, and the
//! GitHub releases page is the update feed itself — there is no server to run
//! and no build metadata to keep in step by hand.
//!
//! Two rules shape everything here. Nothing blocks the UI thread: every check
//! and download runs on a worker and reports back through a channel, because a
//! network stall must never freeze a viewport mid-orbit. And nothing installs
//! itself behind the user's back: applying an update terminates the process,
//! so the restart is always an explicit click, never a consequence of opening
//! the app.
//!
//! A build that Velopack did not install — `cargo run`, the macOS bundle, a
//! binary pulled out of a zip — has no locator to find, so [`UpdateManager`]
//! refuses to construct. That is the [`UpdateStatus::Unmanaged`] state rather
//! than an error: it is the normal case in development and in every test, and
//! it is the reason nothing in the test suites ever reaches the network.

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;

use egui::{Frame, Margin, RichText, Stroke};
use velopack::sources::GithubSource;
use velopack::{UpdateCheck, UpdateInfo, UpdateManager};

use crate::KernelLabApp;
use crate::theme;

/// The repository whose releases are the update feed.
const RELEASES_REPOSITORY: &str = "https://github.com/JackClarkeAE/artificer";

/// Where a build that cannot update itself sends the user instead.
const RELEASES_PAGE: &str = "https://github.com/JackClarkeAE/artificer/releases";

/// What the updater knows right now. Every state is one the About card can
/// draw, so the card is a total function over this enum rather than a pile of
/// booleans that can disagree with each other.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateStatus {
    /// Not installed by Velopack, so self-updating is not available.
    Unmanaged,
    /// Managed, and nothing has been asked of the feed yet.
    Idle,
    /// A check is in flight.
    Checking,
    /// The feed answered, and this is already the newest release.
    UpToDate,
    /// A newer release exists and has not been downloaded yet.
    Available { version: String, bytes: u64 },
    /// The package is downloading. `percent` is Velopack's own 0–100 progress.
    Downloading { percent: i16 },
    /// The package is on disk and will install on the next restart.
    Ready { version: String },
    /// The last check or download failed, in the words the updater used.
    Failed { reason: String },
}

/// A message from a worker thread back to the UI thread.
enum UpdateEvent {
    Checked(Result<Option<Box<UpdateInfo>>, String>),
    Progress(i16),
    Downloaded(Result<(), String>),
}

/// Owns the Velopack manager, the worker channel, and the one status the UI
/// draws from.
pub struct UpdateService {
    /// `None` when this build was not installed by Velopack. Every operation
    /// is a no-op in that case, which is what keeps tests offline.
    manager: Option<UpdateManager>,
    status: UpdateStatus,
    version: String,
    pending: Option<Box<UpdateInfo>>,
    sender: Sender<UpdateEvent>,
    receiver: Receiver<UpdateEvent>,
    checked_at_startup: bool,
}

impl Default for UpdateService {
    fn default() -> Self {
        Self::new()
    }
}

impl UpdateService {
    /// Locates the installed app. This does not touch the network: it only
    /// looks for the Velopack install layout around the running executable.
    #[must_use]
    pub fn new() -> Self {
        let source = GithubSource::new(RELEASES_REPOSITORY, None, false);
        let manager = UpdateManager::new(source, None, None).ok();
        let version = manager.as_ref().map_or_else(
            || env!("CARGO_PKG_VERSION").to_owned(),
            UpdateManager::get_current_version_as_string,
        );
        let status = if manager.is_some() {
            UpdateStatus::Idle
        } else {
            UpdateStatus::Unmanaged
        };
        let (sender, receiver) = channel();
        Self {
            manager,
            status,
            version,
            pending: None,
            sender,
            receiver,
            checked_at_startup: false,
        }
    }

    /// The running version, whether or not this build can update itself.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    #[must_use]
    pub const fn status(&self) -> &UpdateStatus {
        &self.status
    }

    /// Forces a status without touching the network, so the integration suites
    /// can draw every state the card has. Nothing in the product calls this.
    pub fn set_status_for_tests(&mut self, status: UpdateStatus) {
        self.status = status;
    }

    const fn busy(&self) -> bool {
        matches!(
            self.status,
            UpdateStatus::Checking | UpdateStatus::Downloading { .. }
        )
    }

    /// Drains worker messages, and performs the one automatic check this app
    /// ever makes: a single silent check on the first frame after launch.
    pub fn poll(&mut self, context: &egui::Context) {
        if !self.checked_at_startup && self.manager.is_some() {
            self.checked_at_startup = true;
            self.start_check(context);
        }
        loop {
            match self.receiver.try_recv() {
                Ok(UpdateEvent::Checked(Ok(Some(update)))) => {
                    self.status = UpdateStatus::Available {
                        version: update.TargetFullRelease.Version.clone(),
                        bytes: update.TargetFullRelease.Size,
                    };
                    self.pending = Some(update);
                }
                Ok(UpdateEvent::Checked(Ok(None))) => {
                    self.pending = None;
                    self.status = UpdateStatus::UpToDate;
                }
                Ok(UpdateEvent::Checked(Err(reason)) | UpdateEvent::Downloaded(Err(reason))) => {
                    self.status = UpdateStatus::Failed { reason };
                }
                Ok(UpdateEvent::Progress(percent)) => {
                    // Progress that arrives after a failure or a cancellation
                    // must not resurrect the download state.
                    if matches!(self.status, UpdateStatus::Downloading { .. }) {
                        self.status = UpdateStatus::Downloading { percent };
                    }
                }
                Ok(UpdateEvent::Downloaded(Ok(()))) => {
                    self.status = UpdateStatus::Ready {
                        version: self.pending.as_ref().map_or_else(String::new, |update| {
                            update.TargetFullRelease.Version.clone()
                        }),
                    };
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    fn start_check(&mut self, context: &egui::Context) {
        let Some(manager) = self.manager.clone() else {
            return;
        };
        if self.busy() {
            return;
        }
        self.status = UpdateStatus::Checking;
        let events = self.sender.clone();
        let context = context.clone();
        thread::spawn(move || {
            let outcome = manager
                .check_for_updates()
                .map(|check| match check {
                    UpdateCheck::UpdateAvailable(update) => Some(update),
                    UpdateCheck::NoUpdateAvailable | UpdateCheck::RemoteIsEmpty => None,
                })
                .map_err(|error| error.to_string());
            let _ = events.send(UpdateEvent::Checked(outcome));
            // The UI thread is asleep between inputs, so a result nobody asks
            // for is a result nobody sees.
            context.request_repaint();
        });
    }

    fn start_download(&mut self, context: &egui::Context) {
        let (Some(manager), Some(update)) = (self.manager.clone(), self.pending.clone()) else {
            return;
        };
        if self.busy() {
            return;
        }
        self.status = UpdateStatus::Downloading { percent: 0 };

        // Velopack reports progress by blocking sends into a channel, and the
        // thread that owns the download is inside that call for its duration,
        // so a second thread forwards progress on to the UI.
        let (progress_sender, progress_receiver) = channel::<i16>();
        let progress_events = self.sender.clone();
        let progress_context = context.clone();
        thread::spawn(move || {
            for percent in progress_receiver {
                if progress_events
                    .send(UpdateEvent::Progress(percent))
                    .is_err()
                {
                    break;
                }
                progress_context.request_repaint();
            }
        });

        let events = self.sender.clone();
        let context = context.clone();
        thread::spawn(move || {
            let outcome = manager
                .download_updates(&update, Some(progress_sender))
                .map_err(|error| error.to_string());
            let _ = events.send(UpdateEvent::Downloaded(outcome));
            context.request_repaint();
        });
    }

    /// Installs the downloaded package. This does not return on success: the
    /// process is replaced, which is why only an explicit click reaches here.
    fn apply(&mut self) {
        let (Some(manager), Some(update)) = (self.manager.as_ref(), self.pending.as_ref()) else {
            return;
        };
        if let Err(error) = manager.apply_updates_and_restart(&**update) {
            self.status = UpdateStatus::Failed {
                reason: error.to_string(),
            };
        }
    }
}

/// A download size in the units a person reads, not bytes.
fn readable_size(bytes: u64) -> String {
    let kilobytes = bytes.div_ceil(1024);
    if kilobytes < 1024 {
        format!("{kilobytes} KB")
    } else {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a release package is megabytes; the tenth of a MB it prints is exact"
        )]
        let megabytes = kilobytes as f64 / 1024.0;
        format!("{megabytes:.1} MB")
    }
}

impl KernelLabApp {
    /// The updater, for tests that need to draw a state the network would
    /// otherwise have to produce.
    pub fn updates_mut(&mut self) -> &mut UpdateService {
        &mut self.updates
    }

    /// A quiet indicator in the header, shown only once there is something to
    /// act on. An updater that advertises itself while idle is noise.
    pub(crate) fn update_header_button(&mut self, ui: &mut egui::Ui) {
        let label = match self.updates.status() {
            UpdateStatus::Available { .. } => "Update available",
            UpdateStatus::Downloading { .. } => "Updating…",
            UpdateStatus::Ready { .. } => "Update ready",
            _ => return,
        };
        let button = ui.button(RichText::new(label).color(theme::accent()));
        button.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        if button
            .on_hover_text("Open About Artificer to install the new version")
            .clicked()
        {
            self.about_open = true;
        }
    }

    /// Version, provenance, and the whole update flow, in one small window.
    pub(crate) fn about_window(&mut self, context: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut open = self.about_open;
        egui::Window::new("ABOUT ARTIFICER")
            .id(egui::Id::new("about_window"))
            // Off the centre of the viewport, like every other window here: a
            // window over the middle intercepts the drags that orbit a model.
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-18.0, 52.0))
            .default_width(340.0)
            .resizable(false)
            .open(&mut open)
            .frame(
                Frame::new()
                    .fill(theme::panel().gamma_multiply(0.98))
                    .stroke(Stroke::new(1.0, theme::border()))
                    .corner_radius(6)
                    .inner_margin(Margin::same(10)),
            )
            .show(context, |ui| {
                ui.label(RichText::new("ARTIFICER").color(theme::accent()).strong());
                ui.label(
                    RichText::new(format!("Version {}", self.updates.version()))
                        .color(theme::text()),
                );
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);
                self.update_section(ui);
            });
        self.about_open = open;
    }

    fn update_section(&mut self, ui: &mut egui::Ui) {
        let context = ui.ctx().clone();
        match self.updates.status().clone() {
            UpdateStatus::Unmanaged => {
                ui.label(
                    RichText::new(
                        "This copy was not installed by the Artificer installer, so it cannot update itself.",
                    )
                    .small()
                    .color(theme::muted()),
                );
                ui.add_space(4.0);
                if ui.button("Open releases page").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(RELEASES_PAGE));
                }
            }
            UpdateStatus::Idle | UpdateStatus::UpToDate | UpdateStatus::Failed { .. } => {
                let message = match self.updates.status() {
                    UpdateStatus::UpToDate => "Artificer is up to date.".to_owned(),
                    UpdateStatus::Failed { reason } => format!("Update check failed: {reason}"),
                    _ => "Artificer checks GitHub for a new release when it starts.".to_owned(),
                };
                ui.label(RichText::new(message).small().color(theme::muted()));
                ui.add_space(4.0);
                if ui.button("Check for updates").clicked() {
                    self.updates.start_check(&context);
                }
            }
            UpdateStatus::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Checking for updates…")
                            .small()
                            .color(theme::muted()),
                    );
                });
            }
            UpdateStatus::Available { version, bytes } => {
                ui.label(
                    RichText::new(format!("Version {version} is available.")).color(theme::text()),
                );
                ui.label(
                    RichText::new(format!("Download size {}", readable_size(bytes)))
                        .small()
                        .color(theme::muted()),
                );
                ui.add_space(4.0);
                if ui.button("Download update").clicked() {
                    self.updates.start_download(&context);
                }
            }
            UpdateStatus::Downloading { percent } => {
                let fraction = f32::from(percent) / 100.0;
                ui.add(egui::ProgressBar::new(fraction).text(format!("Downloading {percent}%")));
            }
            UpdateStatus::Ready { version } => {
                ui.label(
                    RichText::new(format!("Version {version} is ready to install."))
                        .color(theme::text()),
                );
                ui.label(
                    RichText::new(
                        "Artificer will close and reopen to install it. Save your work first.",
                    )
                    .small()
                    .color(theme::muted()),
                );
                ui.add_space(4.0);
                let restart = ui.button("Restart and install");
                restart.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Restart and install")
                });
                if restart.clicked() {
                    self.updates.apply();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_the_installer_did_not_produce_reports_its_own_version_and_cannot_update() {
        let service = UpdateService::new();
        assert_eq!(service.status(), &UpdateStatus::Unmanaged);
        assert_eq!(service.version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn polling_an_unmanaged_build_never_starts_a_check() {
        let context = egui::Context::default();
        let mut service = UpdateService::new();
        service.poll(&context);
        // No manager means no worker, so the status cannot leave Unmanaged —
        // this is what keeps every test suite off the network.
        assert_eq!(service.status(), &UpdateStatus::Unmanaged);
    }

    #[test]
    fn download_sizes_read_as_sizes() {
        assert_eq!(readable_size(700 * 1024), "700 KB");
        assert_eq!(readable_size(6 * 1024 * 1024), "6.0 MB");
    }
}
