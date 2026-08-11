use artificer_sketch_ui::sketch_toolbar;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};
use sketch_toolbar::{
    CHEVRON_CELL_WIDTH, FAMILY_GAP, PRIMARY_CELL_SIZE, ROW_GAP, SKETCH_TOOLBAR_HEIGHT,
    SKETCH_TOOLBAR_WIDTH, SketchOperationGate, SketchToolCapabilities, SketchToolbarOutput,
    SketchToolbarState, TOOLBAR_BOTTOM_PADDING, TOOLBAR_TOP_PADDING, ToolFamily, ToolVariant,
    render_sketch_toolbar,
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
fn compact_toolbar_is_a_uniform_six_by_two_grid_with_contained_variant_choosers() {
    let mut fixture = ToolbarHarness::new();
    fixture.run();

    for label in [
        "Select sketch geometry",
        "Sketch point",
        "Single line",
        "Two-point rectangle",
        "Centre-point circle",
        "Centre-start-end arc",
        "Outer-diameter polygon",
        "Two-point centre-to-centre slot",
        "Trim curve span",
        "2D fillet",
        "Equal-distance chamfer",
        "Rectangular sketch pattern",
    ] {
        let node = fixture.harness.get_by_role_and_label(Role::Button, label);
        assert_eq!(node.rect().width(), PRIMARY_CELL_SIZE, "{label}");
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
        assert!(node.rect().width() < PRIMARY_CELL_SIZE, "{label}");
        assert_eq!(node.rect().width(), CHEVRON_CELL_WIDTH, "{label}");
        assert_eq!(node.rect().height(), CHEVRON_CELL_WIDTH, "{label}");
    }

    let output = fixture.output.borrow();
    let output = output.as_ref().expect("toolbar output");
    let bounds = output.bounds.expect("toolbar bounds");
    assert_eq!(bounds.width(), SKETCH_TOOLBAR_WIDTH);
    assert_eq!(bounds.height(), PRIMARY_CELL_SIZE * 2.0 + ROW_GAP);
    assert_eq!(
        SKETCH_TOOLBAR_HEIGHT - bounds.height(),
        TOOLBAR_TOP_PADDING + TOOLBAR_BOTTOM_PADDING
    );

    let rows = [
        [
            ToolFamily::Select,
            ToolFamily::Point,
            ToolFamily::Line,
            ToolFamily::Rectangle,
            ToolFamily::Circle,
            ToolFamily::Arc,
        ],
        [
            ToolFamily::Polygon,
            ToolFamily::Slot,
            ToolFamily::Trim,
            ToolFamily::Fillet,
            ToolFamily::Chamfer,
            ToolFamily::Pattern,
        ],
    ];
    for (row_index, row) in rows.into_iter().enumerate() {
        let first = output.controls[row[0] as usize]
            .expect("first row control")
            .primary;
        for (column, family) in row.into_iter().enumerate() {
            let layout = output.controls[family as usize].expect("family layout");
            assert_eq!(
                layout.primary.size(),
                egui::vec2(PRIMARY_CELL_SIZE, PRIMARY_CELL_SIZE)
            );
            assert_eq!(layout.primary.center().y, first.center().y);
            assert_eq!(
                layout.primary.left(),
                first.left() + column as f32 * (PRIMARY_CELL_SIZE + FAMILY_GAP)
            );
            if let Some(chooser) = layout.chooser {
                assert!(layout.primary.contains_rect(chooser));
                assert!(chooser.right() < layout.primary.right());
                assert!(chooser.bottom() < layout.primary.bottom());
            }
        }
        if row_index == 1 {
            let first_row_y = output.controls[ToolFamily::Select as usize]
                .expect("select layout")
                .primary
                .center()
                .y;
            assert_eq!(first.center().y - first_row_y, PRIMARY_CELL_SIZE + ROW_GAP);
        }
    }
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
