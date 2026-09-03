use artificer_sketch_ui::sketch_toolbar;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};
use sketch_toolbar::{
    CHEVRON_CELL_INSET, CHEVRON_CELL_WIDTH, CONSTRAINT_BLOCK_ORIGIN, CONSTRAINT_COLUMNS,
    CONSTRAINT_LABEL_ROOM, CONSTRAINT_TOOLS, DRAWING_COLUMNS, DRAWING_FAMILIES, FAMILY_GAP,
    GENERATOR_BLOCK_ORIGIN, GENERATOR_COLUMNS, GENERATOR_FAMILIES, PRIMARY_CELL_SIZE,
    PRIMARY_CELL_WIDTH, ROW_GAP, SKETCH_TOOLBAR_HEIGHT, SKETCH_TOOLBAR_WIDTH, SketchOperationGate,
    SketchToolCapabilities, SketchToolbarOutput, SketchToolbarState, TILE_ICON_COLUMN,
    TILE_LABEL_ROOM, TOOLBAR_BOTTOM_PADDING, TOOLBAR_ROWS, TOOLBAR_TOP_PADDING, ToolFamily,
    ToolVariant, render_sketch_toolbar,
};

struct ToolbarHarness {
    harness: Harness<'static>,
    active: Rc<Cell<ToolVariant>>,
    gate: Rc<Cell<SketchOperationGate>>,
    capabilities: Rc<RefCell<SketchToolCapabilities>>,
    output: Rc<RefCell<Option<SketchToolbarOutput>>>,
}

impl ToolbarHarness {
    fn new() -> Self {
        Self::with_pixels_per_point(1.0)
    }

    fn with_pixels_per_point(pixels_per_point: f32) -> Self {
        let active = Rc::new(Cell::new(ToolVariant::Select));
        let gate = Rc::new(Cell::new(SketchOperationGate::Ready));
        let capabilities = Rc::new(RefCell::new(SketchToolCapabilities::default()));
        let output = Rc::new(RefCell::new(None));
        let toolbar_state = Rc::new(RefCell::new(SketchToolbarState::default()));

        let app_active = Rc::clone(&active);
        let app_gate = Rc::clone(&gate);
        let app_capabilities = Rc::clone(&capabilities);
        let app_output = Rc::clone(&output);
        let app_state = Rc::clone(&toolbar_state);
        let harness = Harness::builder()
            .with_size([1040.0, 700.0])
            .with_pixels_per_point(pixels_per_point)
            .with_theme(egui::Theme::Dark)
            .with_os(egui::os::OperatingSystem::Nix)
            .build_ui(move |ui| {
                let active_variant = app_active.get();
                let rendered = render_sketch_toolbar(
                    ui,
                    &mut app_state.borrow_mut(),
                    active_variant,
                    app_gate.get(),
                    &app_capabilities.borrow(),
                );
                if let Some(chosen) = rendered.chosen {
                    app_active.set(chosen);
                }
                *app_output.borrow_mut() = Some(rendered);
            });

        Self {
            harness,
            active,
            gate,
            capabilities,
            output,
        }
    }

    fn run(&mut self) {
        self.harness.run();
    }
}

#[test]
fn compact_toolbar_is_three_blocks_of_named_tiles_parted_by_dividers() {
    let mut fixture = ToolbarHarness::new();
    fixture.run();

    // A family without variants has no chooser, so its primary owns the whole
    // tile. One with variants gives the chooser a column and stops short of it.
    for label in [
        "Select sketch geometry",
        "Sketch point",
        "Trim curve span",
        "2D fillet",
    ] {
        let node = fixture.harness.get_by_role_and_label(Role::Button, label);
        assert_eq!(node.rect().width(), PRIMARY_CELL_WIDTH, "{label}");
        assert_eq!(node.rect().height(), PRIMARY_CELL_SIZE, "{label}");
    }
    // A constraint gets the same tile as a drawing tool — the whole one,
    // because it has no variants to choose between.
    for variant in CONSTRAINT_TOOLS {
        let label = variant.descriptor().accessible_name;
        let node = fixture.harness.get_by_role_and_label(Role::Button, label);
        assert_eq!(node.rect().width(), PRIMARY_CELL_WIDTH, "{label}");
        assert_eq!(node.rect().height(), PRIMARY_CELL_SIZE, "{label}");
    }
    for label in [
        "Single line",
        "Two-point rectangle",
        "Centre-point circle",
        "Centre-start-end arc",
        "Outer-diameter polygon",
        "Two-point centre-to-centre slot",
        "Equal-distance chamfer",
        "Rectangular sketch pattern",
    ] {
        let node = fixture.harness.get_by_role_and_label(Role::Button, label);
        assert_eq!(
            node.rect().width(),
            PRIMARY_CELL_WIDTH - CHEVRON_CELL_WIDTH - CHEVRON_CELL_INSET,
            "{label}"
        );
        assert_eq!(node.rect().height(), PRIMARY_CELL_SIZE, "{label}");
    }

    for label in [
        "Choose line type; current default: Single line.",
        "Choose rectangle type; current default: Two-point rectangle.",
        "Choose circle type; current default: Centre-point circle.",
        "Choose arc type; current default: Centre-start-end arc.",
        "Choose polygon type; current default: Outer-diameter polygon.",
        "Choose slot type; current default: Two-point centre-to-centre slot.",
        "Choose chamfer type; current default: Equal-distance chamfer.",
        "Choose pattern type; current default: Rectangular sketch pattern.",
    ] {
        let node = fixture.harness.get_by_role_and_label(Role::Button, label);
        assert!(node.rect().width() < PRIMARY_CELL_WIDTH, "{label}");
        assert_eq!(node.rect().width(), CHEVRON_CELL_WIDTH, "{label}");
        // Full height, not a square in the corner. The chooser used to be a
        // 12 px box sitting on top of the icon it belonged to, which is how a
        // chevron and a glyph came to occupy the same pixels.
        assert_eq!(
            node.rect().height(),
            PRIMARY_CELL_SIZE - CHEVRON_CELL_INSET * 2.0,
            "{label}"
        );
    }

    let output = fixture.output.borrow();
    let output = output.as_ref().expect("toolbar output");
    let bounds = output.bounds.expect("toolbar bounds");
    assert_eq!(bounds.width(), SKETCH_TOOLBAR_WIDTH);
    assert_eq!(
        bounds.height(),
        PRIMARY_CELL_SIZE * TOOLBAR_ROWS as f32 + ROW_GAP * (TOOLBAR_ROWS as f32 - 1.0)
    );
    assert_eq!(
        SKETCH_TOOLBAR_HEIGHT - bounds.height(),
        TOOLBAR_TOP_PADDING + TOOLBAR_BOTTOM_PADDING
    );

    // Four columns of drawing tools over three rows, then a divider and the
    // generators' single column, then a second divider and four columns of
    // constraints over the same three rows.
    let origin = output.controls[DRAWING_FAMILIES[0] as usize]
        .expect("first tile")
        .primary;
    let row_top = |row: usize| origin.top() + row as f32 * (PRIMARY_CELL_SIZE + ROW_GAP);
    for (index, family) in DRAWING_FAMILIES.iter().enumerate() {
        let layout = output.controls[*family as usize].expect("family layout");
        assert_eq!(
            layout.primary.size(),
            egui::vec2(PRIMARY_CELL_WIDTH, PRIMARY_CELL_SIZE),
            "{family:?} must be a whole tile"
        );
        assert_eq!(
            layout.primary.top(),
            row_top(index / DRAWING_COLUMNS),
            "{family:?} row drifted"
        );
        assert_eq!(
            layout.primary.left(),
            origin.left() + (index % DRAWING_COLUMNS) as f32 * (PRIMARY_CELL_WIDTH + FAMILY_GAP),
            "{family:?} column drifted"
        );
        if let Some(chooser) = layout.chooser {
            assert!(layout.primary.contains_rect(chooser));
            // The chooser is a column at the tile's trailing edge. Asserting
            // where it *starts* is the point: it must clear the icon column
            // and the label, because the two overlapping is the bug this
            // layout replaced.
            assert!(
                chooser.left() >= layout.primary.left() + TILE_ICON_COLUMN,
                "chooser for {family:?} overlaps the icon column"
            );
            assert!(chooser.right() <= layout.primary.right());
        }
    }

    for (index, family) in GENERATOR_FAMILIES.iter().enumerate() {
        let layout = output.controls[*family as usize].expect("generator layout");
        assert_eq!(
            layout.primary.size(),
            egui::vec2(PRIMARY_CELL_WIDTH, PRIMARY_CELL_SIZE),
            "{family:?} must be a whole tile"
        );
        // The block is one column wide today, so the row is the index and the
        // column arithmetic would be a modulo of one.
        assert_eq!(GENERATOR_COLUMNS, 1);
        assert_eq!(
            layout.primary.top(),
            row_top(index),
            "{family:?} row drifted"
        );
        assert_eq!(
            layout.primary.left(),
            origin.left() + GENERATOR_BLOCK_ORIGIN,
            "{family:?} column drifted"
        );
    }

    let block_left = origin.left() + CONSTRAINT_BLOCK_ORIGIN;
    for (index, variant) in CONSTRAINT_TOOLS.iter().enumerate() {
        let tile = output.constraints[index].expect("constraint tile");
        assert_eq!(
            tile.size(),
            egui::vec2(PRIMARY_CELL_WIDTH, PRIMARY_CELL_SIZE),
            "{variant:?} must be a whole tile"
        );
        assert_eq!(
            tile.top(),
            row_top(index / CONSTRAINT_COLUMNS),
            "{variant:?} row drifted"
        );
        assert_eq!(
            tile.left(),
            block_left + (index % CONSTRAINT_COLUMNS) as f32 * (PRIMARY_CELL_WIDTH + FAMILY_GAP),
            "{variant:?} column drifted"
        );
        assert!(tile.right() <= bounds.right());
    }
    // A full constraint row reaches the toolbar's trailing edge: the block is
    // as wide as the columns it claims, not a stub of them.
    assert_eq!(
        output.constraints[CONSTRAINT_COLUMNS - 1]
            .expect("last tile of the first constraint row")
            .right(),
        bounds.right()
    );
}

#[test]
fn chooser_is_separately_focusable_and_arrow_down_escape_round_trip() {
    let mut fixture = ToolbarHarness::new();
    fixture.run();
    let chooser_label = "Choose arc type; current default: Centre-start-end arc.";
    fixture
        .harness
        .get_by_role_and_label(Role::Button, chooser_label)
        .focus();
    fixture.run();
    assert!(
        fixture
            .harness
            .get_by_role_and_label(Role::Button, chooser_label)
            .is_focused()
    );

    fixture.harness.key_press(egui::Key::ArrowDown);
    fixture.run();
    fixture
        .harness
        .get_by_role_and_label(Role::Button, "Three-point arc");
    assert!(
        fixture
            .output
            .borrow()
            .as_ref()
            .expect("toolbar output")
            .menu_open
    );

    fixture.harness.key_press(egui::Key::Escape);
    fixture.run();
    let output = fixture.output.borrow();
    let output = output.as_ref().expect("toolbar output");
    assert!(!output.menu_open);
}

#[test]
fn contained_controls_and_cell_strokes_land_on_retina_pixels() {
    let mut fixture = ToolbarHarness::with_pixels_per_point(2.0);
    fixture.run();
    let output = fixture.output.borrow();
    let output = output.as_ref().expect("toolbar output");

    let assert_physical_pixel = |logical: f32, description: &str| {
        let physical = logical * 2.0;
        assert!(
            (physical - physical.round()).abs() <= f32::EPSILON,
            "{description} is not aligned at 2 px/point: {logical} pt"
        );
    };
    for family in ToolFamily::ALL {
        let layout = output.controls[family as usize].expect("family layout");
        for (value, description) in [
            (layout.primary.left() + 0.5, "left cell stroke"),
            (layout.primary.right() - 0.5, "right cell stroke"),
            (layout.primary.top() + 0.5, "top cell stroke"),
            (layout.primary.bottom() - 0.5, "bottom cell stroke"),
        ] {
            assert_physical_pixel(value, description);
        }
        if let Some(chooser) = layout.chooser {
            for (value, description) in [
                (chooser.left(), "chooser left"),
                (chooser.right(), "chooser right"),
                (chooser.top(), "chooser top"),
                (chooser.bottom(), "chooser bottom"),
                (chooser.center().x, "chevron centre x"),
                (chooser.center().y, "chevron centre y"),
            ] {
                assert_physical_pixel(value, description);
            }
            assert!(layout.primary.contains_rect(chooser));
        }
    }
}

#[test]
fn choosing_a_non_default_variant_updates_primary_action_and_preference() {
    let mut fixture = ToolbarHarness::new();
    fixture.run();
    fixture
        .harness
        .get_by_role_and_label(
            Role::Button,
            "Choose circle type; current default: Centre-point circle.",
        )
        .click();
    fixture.run();
    fixture
        .harness
        .get_by_role_and_label(Role::Button, "Two-point diameter circle")
        .click();
    fixture.run();

    assert_eq!(fixture.active.get(), ToolVariant::TwoPointCircle);
    fixture
        .harness
        .get_by_role_and_label(Role::Button, "Two-point diameter circle");
    fixture.harness.get_by_role_and_label(
        Role::Button,
        "Choose circle type; current default: Two-point diameter circle.",
    );
}

#[test]
fn pending_confirmation_disables_both_halves_of_every_selector() {
    let mut fixture = ToolbarHarness::new();
    fixture.gate.set(SketchOperationGate::AwaitingConfirmation);
    fixture.run();

    let primary = fixture
        .harness
        .get_by_role_and_label(Role::Button, "Centre-point circle");
    let chooser = fixture.harness.get_by_role_and_label(
        Role::Button,
        "Choose circle type; current default: Centre-point circle.",
    );
    assert!(primary.accesskit_node().is_disabled());
    assert!(chooser.accesskit_node().is_disabled());

    primary.click();
    fixture.run();
    assert_eq!(fixture.active.get(), ToolVariant::Select);

    fixture
        .capabilities
        .borrow_mut()
        .disable(ToolVariant::Fillet);
    fixture.gate.set(SketchOperationGate::Ready);
    fixture.run();
    assert!(
        fixture
            .harness
            .get_by_role_and_label(Role::Button, "2D fillet")
            .accesskit_node()
            .is_disabled()
    );
}

/// The tile geometry leaves the label a fixed column, and the label has to fit
/// it: the constants can only assert the arithmetic, not what the text
/// actually measures. This is the measurement, at the size the tile draws.
#[test]
fn every_tile_label_fits_the_column_the_tile_geometry_leaves_it() {
    /// One label, what it measures, and the room its own tile leaves it.
    type MeasuredLabel = (&'static str, f32, f32);
    let measured: Rc<RefCell<Vec<MeasuredLabel>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&measured);
    let mut harness = Harness::builder()
        .with_size([1040.0, 700.0])
        .with_pixels_per_point(1.0)
        .with_theme(egui::Theme::Dark)
        .build_ui(move |ui| {
            let mut sink = sink.borrow_mut();
            sink.clear();
            for family in ToolFamily::ALL {
                for variant in family.variants() {
                    let label = variant.tile_label();
                    let width = ui
                        .painter()
                        .layout_no_wrap(
                            label.to_owned(),
                            egui::FontId::proportional(sketch_toolbar::TILE_LABEL_TEXT_SIZE),
                            egui::Color32::WHITE,
                        )
                        .rect
                        .width();
                    // A drawing family spends the chooser column and a
                    // constraint does not, so each label is held to the room
                    // its own tile actually leaves.
                    let room = if CONSTRAINT_TOOLS.contains(variant) {
                        CONSTRAINT_LABEL_ROOM
                    } else {
                        TILE_LABEL_ROOM
                    };
                    sink.push((label, width, room));
                }
            }
        });
    harness.run();

    for (label, width, room) in measured.borrow().iter() {
        assert!(
            width <= room,
            "{label} measures {width} pt and its tile leaves {room} pt"
        );
    }
}
