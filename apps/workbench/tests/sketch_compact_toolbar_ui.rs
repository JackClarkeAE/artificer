use artificer_sketch_ui::sketch_toolbar::{
    CHEVRON_CELL_INSET, CHEVRON_CELL_WIDTH, CONSTRAINT_DIVIDER_WIDTH, FAMILY_GAP,
    PRIMARY_CELL_SIZE, PRIMARY_CELL_WIDTH, ROW_GAP,
};
use artificer_workbench::{KernelLabApp, WorkbenchMode};
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};

fn harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1040.0, 700.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context))
}

fn click_at(harness: &mut Harness<'static, KernelLabApp>, position: egui::Pos2) {
    harness.hover_at(position);
    harness.step();
    harness.event(egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.event(egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.step();
}

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    let center = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
    click_at(harness, center);
}

fn replace_tool_input(harness: &mut Harness<'static, KernelLabApp>, label: &str, value: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .click();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(value);
    harness.run();
}

fn enter_xy_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "XY Plane");
    click_button(harness, "Sketch mode");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    // No palette to raise: the sketch workspace has none. The active tool is
    // the lit ribbon tile and the canvas instruction line.
}

fn choose_variant(harness: &mut Harness<'static, KernelLabApp>, chooser: &str, variant: &str) {
    click_button(harness, chooser);
    click_button(harness, variant);
    assert!(
        harness.query_all_by_label(variant).count() >= 1,
        "{variant} must remain reachable as the family's primary action after being chosen"
    );
    assert_eq!(
        harness.state().active_sketch_tool_label(),
        variant,
        "choosing a variant must actually drive the active tool, not just close the menu"
    );
}

fn canvas_point(harness: &Harness<'static, KernelLabApp>, offset: egui::Vec2) -> egui::Pos2 {
    harness.get_by_label("Sketch viewport").rect().center() + offset
}

/// Click the sketch canvas at `offset` from the current viewport centre.
///
/// Resolved at click time because the parameter panel reserves width only
/// while the armed tool has input fields, and the canvas is centre-anchored:
/// a tool switch that changes panel visibility shifts the whole drawing
/// sideways. A screen position cached across such a switch aims at where the
/// geometry used to draw, not where it is.
fn click_canvas(harness: &mut Harness<'static, KernelLabApp>, offset: egui::Vec2) {
    let position = canvas_point(harness, offset);
    click_at(harness, position);
}

#[test]
fn compact_dropdown_variants_drive_the_active_tool_palette_at_minimum_size() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);

    // The palette that used to name the active tool is gone; the tile itself
    // carries it, and the canvas says what to do next.
    assert_eq!(
        harness.state().active_sketch_tool_label(),
        "Select sketch geometry"
    );
    assert!(harness.query_all_by_label("Select sketch geometry").count() >= 1);

    choose_variant(
        &mut harness,
        "Choose rectangle type; current default: Two-point rectangle.",
        "Centre-point rectangle",
    );
    choose_variant(
        &mut harness,
        "Choose circle type; current default: Centre-point circle.",
        "Two-point diameter circle",
    );
    choose_variant(
        &mut harness,
        "Choose polygon type; current default: Outer-diameter polygon.",
        "Inner-diameter polygon",
    );

    assert!(
        harness
            .query_by_role_and_label(Role::TextInput, "Sides")
            .is_some_and(|node| node
                .value()
                .as_deref()
                .is_some_and(|value| value.trim() == "6")),
        "the active polygon palette should expose its default six-side field"
    );

    choose_variant(
        &mut harness,
        "Choose slot type; current default: Two-point centre-to-centre slot.",
        "Centre-to-outer-point slot",
    );
    choose_variant(
        &mut harness,
        "Choose arc type; current default: Centre-start-end arc.",
        "Three-point arc",
    );
}

#[test]
fn expanded_compact_ribbon_is_unclipped_at_1040_by_700() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    let viewport_top = harness.get_by_label("Sketch viewport").rect().top();

    // Six columns of drawing tools in two rows; the constraints stand in
    // their own column beyond a divider, Relation above Dimension.
    let rows = [
        [
            "Select sketch geometry",
            "Sketch point",
            "Single line",
            "Two-point rectangle",
            "Centre-point circle",
            "Centre-start-end arc",
        ],
        [
            "Outer-diameter polygon",
            "Two-point centre-to-centre slot",
            "Trim curve span",
            "2D fillet",
            "Equal-distance chamfer",
            "Rectangular sketch pattern",
        ],
    ];
    for (row_index, label) in ["Horizontal relation", "Sketch dimension"]
        .into_iter()
        .enumerate()
    {
        let row_first = harness
            .get_by_role_and_label(Role::Button, rows[row_index][0])
            .rect();
        let rect = harness.get_by_role_and_label(Role::Button, label).rect();
        assert_eq!(rect.center().y, row_first.center().y, "{label} row drifted");
        assert_eq!(
            rect.left(),
            row_first.left()
                + 6.0 * PRIMARY_CELL_WIDTH
                + 5.0 * FAMILY_GAP
                + CONSTRAINT_DIVIDER_WIDTH,
            "{label} sits beyond the divider"
        );
        assert!(rect.max.x <= 1040.0, "{label} is clipped: {rect:?}");
    }
    for (row_index, row) in rows.into_iter().enumerate() {
        let first = harness.get_by_role_and_label(Role::Button, row[0]).rect();
        for (column, label) in row.into_iter().enumerate() {
            let rect = harness.get_by_role_and_label(Role::Button, label).rect();
            assert!(rect.is_positive(), "{label} must have a hit target");
            // A family with variants yields the chooser column; one without owns
            // the full tile. Both are complete — neither is squeezed by the
            // ribbon running out of width, which is what this guards.
            let split_width = PRIMARY_CELL_WIDTH - CHEVRON_CELL_WIDTH - CHEVRON_CELL_INSET;
            assert!(
                ((rect.width() - PRIMARY_CELL_WIDTH).abs() <= 0.1
                    || (rect.width() - split_width).abs() <= 0.1)
                    && (rect.height() - PRIMARY_CELL_SIZE).abs() <= 0.1,
                "{label} must remain a complete tile: {rect:?}"
            );
            assert_eq!(rect.center().y, first.center().y, "{label} row drifted");
            assert_eq!(
                rect.left(),
                first.left() + column as f32 * (PRIMARY_CELL_WIDTH + FAMILY_GAP),
                "{label} column drifted"
            );
            assert!(
                rect.min.x >= 0.0
                    && rect.max.x <= 1040.0
                    && rect.min.y >= 0.0
                    && rect.max.y <= viewport_top - 4.0,
                "{label} is clipped or lacks bottom ribbon padding: {rect:?}; viewport top {viewport_top}"
            );
        }
        if row_index == 1 {
            let first_row_y = harness
                .get_by_role_and_label(Role::Button, rows[0][0])
                .rect()
                .center()
                .y;
            assert_eq!(first.center().y - first_row_y, PRIMARY_CELL_SIZE + ROW_GAP);
        }
    }

    for (primary_label, chooser_label) in [
        (
            "Single line",
            "Choose line type; current default: Single line.",
        ),
        (
            "Two-point rectangle",
            "Choose rectangle type; current default: Two-point rectangle.",
        ),
        (
            "Centre-point circle",
            "Choose circle type; current default: Centre-point circle.",
        ),
        (
            "Centre-start-end arc",
            "Choose arc type; current default: Centre-start-end arc.",
        ),
        (
            "Outer-diameter polygon",
            "Choose polygon type; current default: Outer-diameter polygon.",
        ),
        (
            "Two-point centre-to-centre slot",
            "Choose slot type; current default: Two-point centre-to-centre slot.",
        ),
        (
            "Equal-distance chamfer",
            "Choose chamfer type; current default: Equal-distance chamfer.",
        ),
        (
            "Rectangular sketch pattern",
            "Choose pattern type; current default: Rectangular sketch pattern.",
        ),
    ] {
        let primary = harness
            .get_by_role_and_label(Role::Button, primary_label)
            .rect();
        let chooser = harness
            .get_by_role_and_label(Role::Button, chooser_label)
            .rect();
        assert_eq!(
            chooser.size(),
            egui::vec2(
                CHEVRON_CELL_WIDTH,
                PRIMARY_CELL_SIZE - CHEVRON_CELL_INSET * 2.0
            )
        );
        // The chooser sits beside the primary, not on top of it. It used to be a
        // square in the corner overlapping the icon, so the invariant that
        // matters is that these two rects are disjoint.
        assert!(
            chooser.left() >= primary.right(),
            "{chooser_label} overlaps its own tool button"
        );
        assert_eq!(chooser.top(), primary.top() + CHEVRON_CELL_INSET);
    }
}

#[test]
fn compound_rectangle_commits_at_its_final_click_and_keeps_the_toolbar_live() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    choose_variant(
        &mut harness,
        "Choose rectangle type; current default: Two-point rectangle.",
        "Centre-point rectangle",
    );

    // A half-finished draft never locks the toolbar or stages an operation.
    let centre = canvas_point(&harness, egui::vec2(-30.0, -20.0));
    let corner = canvas_point(&harness, egui::vec2(70.0, 45.0));
    click_at(&mut harness, centre);
    assert!(harness.state().pending_operation_label().is_none());
    assert_eq!(harness.state().sketch_entity_count(), 0);
    assert_eq!(harness.state().sketch_revision(), 0);
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Centre-point rectangle")
            .accesskit_node()
            .is_disabled(),
        "drawing must not lock the toolbar; the next tool is one click away"
    );
    // Finishing and leaving a sketch are ribbon commands in COMPLETE now, not a
    // square tick and cross on a rail. What still has to hold is that both stay
    // pressable while a draft is in progress.
    for label in ["Finish sketch", "Exit sketch"] {
        let rect = harness.get_by_role_and_label(Role::Button, label).rect();
        assert!(rect.is_positive(), "{label} must have a hit region");
        assert!(
            rect.width() >= 24.0 && rect.height() >= 24.0,
            "{label} is below the minimum hit target: {rect:?}"
        );
    }

    // Escape abandons the draft without consuming a revision.
    harness.key_press(egui::Key::Escape);
    harness.step();
    assert_eq!(harness.state().sketch_entity_count(), 0);
    assert_eq!(harness.state().sketch_revision(), 0);
    assert!(harness.state().pending_operation_label().is_none());

    // The completed stroke commits as its final click lands.
    click_at(&mut harness, centre);
    click_at(&mut harness, corner);
    assert_eq!(harness.state().sketch_entity_count(), 4);
    assert_eq!(harness.state().sketch_revision(), 1);
    assert!(harness.state().pending_operation_label().is_none());
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Centre-point rectangle")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn retained_tool_editors_are_accessible_for_fillet_and_both_chamfer_modes() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);

    click_button(&mut harness, "2D fillet");
    replace_tool_input(&mut harness, "Fillet radius", "2.25");
    assert_eq!(
        harness
            .get_by_role_and_label(Role::TextInput, "Fillet radius")
            .value()
            .as_deref(),
        Some("2.25")
    );
    replace_tool_input(&mut harness, "Fillet radius", "invalid");
    assert!(harness.query_all_by_label("Enter a number").count() >= 1);
    harness.key_press(egui::Key::Escape);
    harness.step();
    assert_eq!(
        harness
            .get_by_role_and_label(Role::TextInput, "Fillet radius")
            .value()
            .as_deref(),
        Some("2.25"),
        "Escape should restore the last valid radius without cancelling the sketch"
    );
    assert!(harness.state().pending_operation_label().is_none());

    click_button(&mut harness, "Equal-distance chamfer");
    replace_tool_input(&mut harness, "Chamfer distance", "0.75");
    assert_eq!(
        harness
            .get_by_role_and_label(Role::TextInput, "Chamfer distance")
            .value()
            .as_deref(),
        Some("0.75")
    );

    choose_variant(
        &mut harness,
        "Choose chamfer type; current default: Equal-distance chamfer.",
        "Two-distance chamfer",
    );
    replace_tool_input(&mut harness, "First distance", "0.5");
    replace_tool_input(&mut harness, "Second distance", "1.25");
    for (label, expected) in [("First distance", "0.5"), ("Second distance", "1.25")] {
        assert_eq!(
            harness
                .get_by_role_and_label(Role::TextInput, label)
                .value()
                .as_deref(),
            Some(expected)
        );
    }
    harness
        .get_by_role_and_label(Role::TextInput, "First distance")
        .focus();
    harness.run();
    assert!(
        harness
            .get_by_role_and_label(Role::TextInput, "First distance")
            .is_focused()
    );
    harness.key_press(egui::Key::Tab);
    harness.step();
    assert!(
        harness
            .get_by_role_and_label(Role::TextInput, "Second distance")
            .is_focused(),
        "Tab should follow descriptor input order"
    );
    harness.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Tab);
    harness.step();
    assert!(
        harness
            .get_by_role_and_label(Role::TextInput, "First distance")
            .is_focused(),
        "Shift-Tab should traverse the typed controls in reverse"
    );
    harness.key_press(egui::Key::Enter);
    harness.step();
    assert!(
        harness.state().pending_operation_label().is_none(),
        "editor Enter must be isolated from the universal operation gate"
    );
}

#[test]
fn analytic_fillet_and_maximum_rectangular_pattern_commit_as_complete_strokes() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);

    // The drawn corner sits at the view centre, so every canvas anchor below
    // is an offset from the centre resolved when the click lands — Select has
    // no input fields, so arming it drops the parameter panel and re-centres
    // the drawing under the seed click.
    click_button(&mut harness, "Single line");
    click_canvas(&mut harness, egui::vec2(-100.0, 0.0));
    click_canvas(&mut harness, egui::Vec2::ZERO);
    click_canvas(&mut harness, egui::Vec2::ZERO);
    click_canvas(&mut harness, egui::vec2(0.0, -100.0));
    assert_eq!(harness.state().sketch_entity_count(), 2);
    assert_eq!(harness.state().sketch_revision(), 2);

    // Picking both corner legs completes the fillet stroke, which commits:
    // two trimmed lines plus the arc replace the sharp corner.
    click_button(&mut harness, "2D fillet");
    replace_tool_input(&mut harness, "Fillet radius", "0.4");
    click_canvas(&mut harness, egui::vec2(-50.0, 0.0));
    click_canvas(&mut harness, egui::vec2(0.0, -50.0));
    assert!(harness.state().pending_operation_label().is_none());
    assert_eq!(harness.state().sketch_entity_count(), 3);
    assert_eq!(harness.state().sketch_revision(), 3);

    // Seed the pattern from the vertical leg explicitly.
    click_button(&mut harness, "Select sketch geometry");
    click_canvas(&mut harness, egui::vec2(0.0, -50.0));
    click_button(&mut harness, "Rectangular sketch pattern");
    replace_tool_input(&mut harness, "First count", "16");
    replace_tool_input(&mut harness, "First spacing", "0.25");
    harness
        .get_by_role_and_label(Role::CheckBox, "Second direction")
        .click();
    harness.run();
    replace_tool_input(&mut harness, "Second count", "16");
    replace_tool_input(&mut harness, "Second spacing", "-0.25");
    click_canvas(&mut harness, egui::vec2(80.0, 0.0));

    // The bounded 16x16 placement commits all 255 generated instances.
    assert!(harness.state().pending_operation_label().is_none());
    assert_eq!(harness.state().sketch_pending_entity_count(), 0);
    assert_eq!(harness.state().sketch_entity_count(), 258);
    assert_eq!(harness.state().sketch_revision(), 4);
}

#[test]
fn a_drawn_circle_leaves_every_other_sketch_tool_immediately_pickable() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);

    // Draw a circle the way the canvas does: centre, then rim.
    click_button(&mut harness, "Centre-point circle");
    let centre = canvas_point(&harness, egui::vec2(-60.0, 0.0));
    let rim = canvas_point(&harness, egui::vec2(-20.0, 0.0));
    click_at(&mut harness, centre);
    click_at(&mut harness, rim);
    assert_eq!(harness.state().sketch_entity_count(), 1);
    assert_eq!(harness.state().sketch_revision(), 1);

    // No tick stands between the finished circle and the next shape: the
    // whole palette stays live and the next tool is one click away.
    assert!(harness.state().pending_operation_label().is_none());
    for label in ["Two-point rectangle", "Single line", "Centre-point circle"] {
        assert!(
            !harness
                .get_by_role_and_label(Role::Button, label)
                .accesskit_node()
                .is_disabled(),
            "{label} must stay pickable straight after a drawn circle"
        );
    }

    click_button(&mut harness, "Two-point rectangle");
    let first = canvas_point(&harness, egui::vec2(20.0, -40.0));
    let opposite = canvas_point(&harness, egui::vec2(90.0, 40.0));
    click_at(&mut harness, first);
    assert!(harness.state().sketch_creation_draft_active());
    click_at(&mut harness, opposite);
    assert_eq!(harness.state().sketch_entity_count(), 2);
    assert_eq!(harness.state().sketch_revision(), 2);
    assert!(harness.state().pending_operation_label().is_none());
}
