//! The update surface, driven through the UI a user actually touches.
//!
//! Nothing here reaches the network, and it cannot: a test binary is not a
//! Velopack installation, so the updater has no manager and every check is a
//! no-op. The states the network would otherwise produce are set directly, so
//! each one is drawn and asserted without a release feed existing anywhere.

use artificer_workbench::{KernelLabApp, update::UpdateStatus};
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};

fn harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context))
}

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    harness.get_by_role_and_label(Role::Button, label).click();
    harness.run();
}

fn open_about(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "File menu");
    click_button(harness, "About Artificer");
}

fn set_status(harness: &mut Harness<'static, KernelLabApp>, status: UpdateStatus) {
    harness
        .state_mut()
        .updates_mut()
        .set_status_for_tests(status);
    harness.run();
}

#[test]
fn about_reports_the_running_version_and_a_build_that_cannot_update_itself() {
    let mut harness = harness();
    open_about(&mut harness);

    assert!(
        harness
            .query_by_label(&format!("Version {}", env!("CARGO_PKG_VERSION")))
            .is_some(),
        "About must name the version that is running"
    );
    // A test binary was not produced by the installer, so the honest offer is
    // the releases page rather than a button that cannot work.
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Open releases page")
            .is_some(),
        "an unmanaged build must send the user to the releases page"
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Check for updates")
            .is_none(),
        "an unmanaged build must not offer a check it cannot perform"
    );
}

#[test]
fn the_header_stays_quiet_until_there_is_an_update_to_act_on() {
    let mut harness = harness();
    for quiet in [
        UpdateStatus::Unmanaged,
        UpdateStatus::Idle,
        UpdateStatus::Checking,
        UpdateStatus::UpToDate,
    ] {
        set_status(&mut harness, quiet.clone());
        assert!(
            harness
                .query_by_role_and_label(Role::Button, "Update available")
                .is_none()
                && harness
                    .query_by_role_and_label(Role::Button, "Update ready")
                    .is_none(),
            "the header must not advertise an update in {quiet:?}"
        );
    }

    set_status(
        &mut harness,
        UpdateStatus::Available {
            version: "0.3.0".to_owned(),
            bytes: 24 * 1024 * 1024,
        },
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Update available")
            .is_some(),
        "a waiting update must be visible without opening a menu"
    );
}

#[test]
fn the_header_indicator_opens_the_about_window() {
    let mut harness = harness();
    set_status(
        &mut harness,
        UpdateStatus::Available {
            version: "0.3.0".to_owned(),
            bytes: 24 * 1024 * 1024,
        },
    );
    click_button(&mut harness, "Update available");

    assert!(
        harness
            .query_by_label("Version 0.3.0 is available.")
            .is_some(),
        "the indicator must lead to the version it is advertising"
    );
    assert!(
        harness.query_by_label("Download size 24.0 MB").is_some(),
        "a download must say how large it is before it starts"
    );
}

#[test]
fn a_downloaded_update_installs_only_on_an_explicit_restart() {
    let mut harness = harness();
    open_about(&mut harness);
    set_status(
        &mut harness,
        UpdateStatus::Ready {
            version: "0.3.0".to_owned(),
        },
    );

    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Restart and install")
            .is_some(),
        "installing must be a click, never a consequence of launching"
    );
    // Restarting closes the app and reopens it, so the warning is part of the
    // contract, not decoration.
    assert!(
        harness
            .query_by_label("Artificer will close and reopen to install it. Save your work first.")
            .is_some(),
        "the restart must warn about unsaved work before it happens"
    );
}

#[test]
fn a_failed_check_says_why_and_offers_another_try() {
    let mut harness = harness();
    open_about(&mut harness);
    set_status(
        &mut harness,
        UpdateStatus::Failed {
            reason: "network unreachable".to_owned(),
        },
    );

    assert!(
        harness
            .query_by_label("Update check failed: network unreachable")
            .is_some(),
        "a failure must be reported in the updater's own words"
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Check for updates")
            .is_some(),
        "a failure must leave a way to try again"
    );
}
