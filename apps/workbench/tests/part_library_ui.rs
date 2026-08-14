use artificer_catalog::CatalogStore;
use artificer_model::CURRENT_DOCUMENT_VERSION;
use artificer_workbench::{
    KernelLabApp,
    part_library::{
        ALUMINIUM_EXTRUSION_20X20_KEY, PartInsertionEligibility, ResolvedExtrusionDimensions,
    },
};
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static CATALOG_TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

fn harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context))
}

fn catalog_harness(root: PathBuf) -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(move |creation_context| {
            KernelLabApp::new_paused_with_catalog_root(creation_context, root)
        })
}

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    harness.get_by_role_and_label(Role::Button, label).click();
    harness.run();
}

/// Save, Open and Export live in the File menu, so reaching one means opening
/// the menu first — the same trip the user makes.
fn click_file_menu_item(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    click_button(harness, "File menu");
    click_button(harness, label);
}

fn press_key(harness: &mut Harness<'static, KernelLabApp>, key: egui::Key) {
    harness.key_down(key);
    harness.step();
    harness.key_up(key);
    harness.step();
}

fn enter_length(harness: &mut Harness<'static, KernelLabApp>, value: &str) {
    let input = harness.get_by_role_and_label(Role::TextInput, "Length (mm)");
    input.click();
    input.type_text(value);
    harness.run();
}

fn replace_length(harness: &mut Harness<'static, KernelLabApp>, value: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, "Length (mm)")
        .click();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::TextInput, "Length (mm)")
        .type_text(value);
    harness.run();
}

#[test]
fn unresolved_parameter_blocks_add_without_mutating_the_document() {
    let mut harness = harness();
    harness.run();
    let snapshot = harness.state().displayed_snapshot_id();
    let digest = harness.state().displayed_semantic_digest();
    let attempts = harness.state().transaction_attempt_count();
    let features = harness.state().document_feature_count();

    click_button(&mut harness, "Library");

    assert!(harness.state().part_library_open());
    assert_eq!(
        harness.state().part_library_eligibility(),
        PartInsertionEligibility::MissingLength
    );
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Add to current workspace")
            .accesskit_node()
            .is_disabled()
    );
    assert!(
        harness
            .query_all_by_label(
                "Length is required. Enter a value in millimetres before adding this part."
            )
            .next()
            .is_some()
    );
    assert!(harness.state().staged_part_insertion().is_none());
    assert!(harness.state().committed_part_insertions().is_empty());
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().displayed_semantic_digest(), digest);
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert_eq!(harness.state().document_feature_count(), features);
}

#[test]
fn add_stages_then_tick_commits_separate_parameterized_intents() {
    let mut harness = harness();
    harness.run();
    let snapshot = harness.state().displayed_snapshot_id();
    let digest = harness.state().displayed_semantic_digest();
    let attempts = harness.state().transaction_attempt_count();
    let features = harness.state().document_feature_count();
    let bodies = harness.state().body_count();

    click_button(&mut harness, "Library");
    enter_length(&mut harness, "455");
    assert!(matches!(
        harness.state().part_library_eligibility(),
        PartInsertionEligibility::Ready {
            length_mm: 455.0,
            ..
        }
    ));

    click_button(&mut harness, "Add to current workspace");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Insert library component")
    );
    assert!(harness.state().committed_part_insertions().is_empty());
    let first_staging_id = harness
        .state()
        .staged_part_insertion()
        .expect("first insertion should be staged")
        .staging_id;

    click_button(&mut harness, "Confirm operation");
    assert!(!harness.state().operation_confirmation_pending());
    assert!(harness.state().staged_part_insertion().is_none());
    assert_eq!(harness.state().committed_part_insertions().len(), 1);
    let first = &harness.state().committed_part_insertions()[0];
    assert_eq!(first.definition_key, ALUMINIUM_EXTRUSION_20X20_KEY);
    assert_eq!(first.staging_id, first_staging_id);
    assert_eq!(
        first.resolved_dimensions_mm(),
        Some(ResolvedExtrusionDimensions {
            width_mm: 20.0,
            height_mm: 20.0,
            length_mm: 455.0,
        })
    );
    assert_ne!(harness.state().displayed_snapshot_id(), snapshot);
    assert_ne!(harness.state().displayed_semantic_digest(), digest);
    assert_eq!(harness.state().transaction_attempt_count(), attempts + 1);
    assert_eq!(harness.state().document_feature_count(), features + 1);
    assert_eq!(harness.state().component_instance_count(), 1);
    assert_eq!(harness.state().body_count(), bodies + 1);
    assert!((harness.state().displayed_measures().unwrap().volume - 182_000.0).abs() <= 1.0e-8);
    assert!(
        harness
            .query_all_by_label("◇  20 × 20 Aluminium Extrusion · component 1")
            .next()
            .is_some()
    );
    click_button(&mut harness, "Hide Body 2");
    assert!(!harness.state().body_visible(1));
    click_button(&mut harness, "Show Body 2");
    assert!(harness.state().body_visible(1));

    click_button(&mut harness, "Add to current workspace");
    let second_staging_id = harness
        .state()
        .staged_part_insertion()
        .expect("second insertion should be staged")
        .staging_id;
    assert_ne!(first_staging_id, second_staging_id);
    click_button(&mut harness, "Confirm operation");

    assert_eq!(harness.state().committed_part_insertions().len(), 2);
    assert_eq!(
        harness.state().committed_part_insertions()[0].resolved_dimensions_mm(),
        harness.state().committed_part_insertions()[1].resolved_dimensions_mm()
    );
    assert_eq!(harness.state().transaction_attempt_count(), attempts + 2);
    assert_eq!(harness.state().document_feature_count(), features + 2);
    assert_eq!(harness.state().component_instance_count(), 2);
    assert_eq!(harness.state().body_count(), bodies + 2);
    let variants = harness.state().component_variant_bindings();
    assert_eq!(variants.len(), 2);
    assert_ne!(variants[0].0, variants[1].0);
    assert_eq!(variants[0].1, variants[1].1);
}

#[test]
fn red_x_cancels_staged_insertion_and_keeps_parameter_value_for_retry() {
    let mut harness = harness();
    harness.run();
    let attempts = harness.state().transaction_attempt_count();
    let features = harness.state().document_feature_count();
    let bodies = harness.state().body_count();

    click_button(&mut harness, "Library");
    enter_length(&mut harness, "310");
    click_button(&mut harness, "Add to current workspace");
    assert!(harness.state().staged_part_insertion().is_some());

    click_button(&mut harness, "Cancel operation");
    assert!(!harness.state().operation_confirmation_pending());
    assert!(harness.state().staged_part_insertion().is_none());
    assert!(harness.state().committed_part_insertions().is_empty());
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert_eq!(harness.state().document_feature_count(), features);
    assert_eq!(harness.state().component_instance_count(), 0);
    assert_eq!(harness.state().body_count(), bodies);
    assert!(matches!(
        harness.state().part_library_eligibility(),
        PartInsertionEligibility::Ready {
            length_mm: 310.0,
            ..
        }
    ));

    click_button(&mut harness, "Add to current workspace");
    press_key(&mut harness, egui::Key::Enter);
    assert_eq!(harness.state().committed_part_insertions().len(), 1);
    assert_eq!(
        harness.state().committed_part_insertions()[0].length_mm(),
        Some(310.0)
    );
    assert_eq!(harness.state().transaction_attempt_count(), attempts + 1);
    assert_eq!(harness.state().document_feature_count(), features + 1);
    assert_eq!(harness.state().component_instance_count(), 1);
    assert_eq!(harness.state().body_count(), bodies + 1);
    assert!((harness.state().displayed_measures().unwrap().volume - 124_000.0).abs() <= 1.0e-8);
}

#[test]
fn different_lengths_create_distinct_reproducible_variants() {
    let mut harness = harness();
    harness.run();
    click_button(&mut harness, "Library");

    enter_length(&mut harness, "310");
    click_button(&mut harness, "Add to current workspace");
    click_button(&mut harness, "Confirm operation");
    let first_snapshot = harness.state().displayed_snapshot_id();
    assert!((harness.state().displayed_measures().unwrap().volume - 124_000.0).abs() <= 1.0e-8);

    replace_length(&mut harness, "455");
    click_button(&mut harness, "Add to current workspace");
    click_button(&mut harness, "Confirm operation");
    assert_ne!(harness.state().displayed_snapshot_id(), first_snapshot);
    assert!((harness.state().displayed_measures().unwrap().volume - 182_000.0).abs() <= 1.0e-8);

    let variants = harness.state().component_variant_bindings();
    assert_eq!(variants.len(), 2);
    assert_ne!(variants[0].0, variants[1].0);
    assert_ne!(variants[0].1, variants[1].1);
}

#[test]
fn persistent_store_selection_pins_digest_and_inserts_through_exact_revision() {
    let catalog = TemporaryCatalogRoot::new();
    let mut harness = catalog_harness(catalog.path.clone());
    harness.run();
    assert!(harness.state().persistent_catalog_active());
    assert_eq!(harness.state().catalog_entry_count(), 1);

    click_button(&mut harness, "Library");
    enter_length(&mut harness, "125");
    click_button(&mut harness, "Add to current workspace");
    let staged = harness
        .state()
        .staged_part_insertion()
        .expect("store-backed intent should be staged");
    assert_eq!(staged.definition_digest.len(), 64);
    click_button(&mut harness, "Confirm operation");

    assert_eq!(harness.state().component_instance_count(), 1);
    assert!((harness.state().displayed_measures().unwrap().volume - 50_000.0).abs() <= 1.0e-8);
    drop(harness);

    let reopened = CatalogStore::open(&catalog.path).expect("catalog should reopen");
    assert_eq!(reopened.index_snapshot().unwrap().len(), 1);
}

#[test]
fn component_variants_survive_fresh_process_save_load_and_replay() {
    let mut source = harness();
    source.run();
    click_button(&mut source, "Library");

    enter_length(&mut source, "310");
    click_button(&mut source, "Add to current workspace");
    click_button(&mut source, "Confirm operation");
    click_button(&mut source, "Hide Body 2");

    replace_length(&mut source, "455");
    click_button(&mut source, "Add to current workspace");
    click_button(&mut source, "Confirm operation");
    let saved_variants = source.state().component_variant_bindings();
    let saved = source.state().native_document_json().unwrap();
    let native: serde_json::Value =
        serde_json::from_str(&saved).expect("saved component document should be valid JSON");
    assert_eq!(native["version"], CURRENT_DOCUMENT_VERSION);

    let mut restored = harness();
    restored.run();
    restored
        .state_mut()
        .load_native_document_json(&saved)
        .expect("a fresh app should replay the complete native document");
    restored.run();

    assert_eq!(restored.state().native_document_json().unwrap(), saved);
    assert_eq!(restored.state().component_instance_count(), 2);
    assert_eq!(
        restored.state().component_variant_bindings(),
        saved_variants
    );
    assert_eq!(restored.state().body_count(), 3);
    assert!(!restored.state().body_visible(1));
    assert!(restored.state().body_visible(2));
    assert!((restored.state().displayed_measures().unwrap().volume - 182_000.0).abs() <= 1.0e-8);
    assert_eq!(
        restored.state().feature_timeline_entries(),
        ["Origin", "Base body", "Component 1", "Component 2"]
    );
}

#[test]
fn tampered_document_replay_fails_atomically_without_replacing_workspace() {
    let mut harness = harness();
    harness.run();
    click_button(&mut harness, "Library");
    enter_length(&mut harness, "310");
    click_button(&mut harness, "Add to current workspace");
    click_button(&mut harness, "Confirm operation");

    let retained_json = harness.state().native_document_json().unwrap();
    let retained_snapshot = harness.state().displayed_snapshot_id();
    let retained_bindings = harness.state().component_variant_bindings();
    let retained_bodies = harness.state().body_count();
    let mut tampered: serde_json::Value = serde_json::from_str(&retained_json).unwrap();
    let component_feature = tampered["state"]["features"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap();
    component_feature["action"]["command"]["distance"] = serde_json::json!(311.0);
    let tampered_json = serde_json::to_string_pretty(&tampered).unwrap();

    let error = harness
        .state_mut()
        .load_native_document_json(&tampered_json)
        .expect_err("changed geometry must not match clean persisted provenance");
    assert!(error.contains("provenance"), "unexpected error: {error}");
    assert_eq!(
        harness.state().native_document_json().unwrap(),
        retained_json
    );
    assert_eq!(harness.state().displayed_snapshot_id(), retained_snapshot);
    assert_eq!(
        harness.state().component_variant_bindings(),
        retained_bindings
    );
    assert_eq!(harness.state().body_count(), retained_bodies);
}

#[test]
fn rolled_back_history_reloads_at_saved_cursor_and_can_roll_forward() {
    let mut source = harness();
    source.run();
    click_button(&mut source, "Library");
    enter_length(&mut source, "310");
    click_button(&mut source, "Add to current workspace");
    click_button(&mut source, "Confirm operation");
    replace_length(&mut source, "455");
    click_button(&mut source, "Add to current workspace");
    click_button(&mut source, "Confirm operation");
    assert_eq!(source.state().history_position(), 4);

    click_button(&mut source, "Step history backward");
    assert_eq!(source.state().history_position(), 3);
    assert_eq!(source.state().body_count(), 2);
    assert!((source.state().displayed_measures().unwrap().volume - 124_000.0).abs() <= 1.0e-8);
    let saved = source.state().native_document_json().unwrap();

    let mut restored = harness();
    restored.run();
    restored
        .state_mut()
        .load_native_document_json(&saved)
        .expect("the loader should privately replay future history caches");
    restored.run();
    assert_eq!(restored.state().history_position(), 3);
    assert_eq!(restored.state().body_count(), 2);
    assert!((restored.state().displayed_measures().unwrap().volume - 124_000.0).abs() <= 1.0e-8);

    click_button(&mut restored, "Step history forward");
    assert_eq!(restored.state().history_position(), 4);
    assert_eq!(restored.state().body_count(), 3);
    click_button(
        &mut restored,
        "◇  20 × 20 Aluminium Extrusion · component 2",
    );
    assert!((restored.state().displayed_measures().unwrap().volume - 182_000.0).abs() <= 1.0e-8);
}

#[test]
fn save_and_open_buttons_use_native_file_and_universal_confirmation_gate() {
    let files = TemporaryCatalogRoot::new();
    let document_path = files.path.join("assembly.artificer.json");
    let mut harness = harness();
    harness.state_mut().set_document_path(&document_path);
    harness.run();

    click_button(&mut harness, "Library");
    enter_length(&mut harness, "310");
    click_button(&mut harness, "Add to current workspace");
    click_button(&mut harness, "Confirm operation");
    click_file_menu_item(&mut harness, "Save document");
    assert!(document_path.is_file());

    replace_length(&mut harness, "455");
    click_button(&mut harness, "Add to current workspace");
    click_button(&mut harness, "Confirm operation");
    assert_eq!(harness.state().component_instance_count(), 2);

    click_file_menu_item(&mut harness, "Open saved document");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Open saved document")
    );
    assert_eq!(harness.state().component_instance_count(), 2);
    click_button(&mut harness, "Cancel operation");
    assert_eq!(harness.state().component_instance_count(), 2);

    click_file_menu_item(&mut harness, "Open saved document");
    click_button(&mut harness, "Confirm operation");
    assert_eq!(harness.state().component_instance_count(), 1);
    assert_eq!(harness.state().body_count(), 2);
    assert!((harness.state().displayed_measures().unwrap().volume - 124_000.0).abs() <= 1.0e-8);
}

struct TemporaryCatalogRoot {
    path: PathBuf,
}

impl TemporaryCatalogRoot {
    fn new() -> Self {
        let unique = CATALOG_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        Self {
            path: std::env::temp_dir().join(format!(
                "artificer-ui-catalog-{}-{unique}",
                std::process::id()
            )),
        }
    }
}

impl Drop for TemporaryCatalogRoot {
    fn drop(&mut self) {
        if self.path.starts_with(std::env::temp_dir()) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
