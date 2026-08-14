//! The model-workspace command registry.
//!
//! Every ribbon command is a row in one table carrying where it lives (tab and
//! group), how it is drawn (icon, size, label), and what it is called (visible
//! label, accessible name, tooltip). The ribbon is a renderer over this table.
//!
//! This mirrors [`crate::sketch_toolbar::SketchToolRegistry`], which has always
//! worked this way — the sketch ribbon's icon grid, split buttons and captions
//! all fall out of its descriptors. The model half of the workbench previously
//! had no such table, so its commands could only be laid out by hand: labels
//! were literals at the call site, there was no icon field to draw, and ten
//! commands ended up hidden inside two dropdowns named with an ellipsis.
//!
//! Enablement deliberately stays out of the table. Whether a command can run is
//! a question about live workbench state, so it is answered by
//! `KernelLabApp::command_availability`; the table only says a command exists
//! and how to present it.

use crate::command_icons::CommandIcon;

/// A top-level ribbon tab.
///
/// Tabs are the taxonomy level the workbench used to lack: with one row for
/// every model command, the row had to eat itself — hence `Boolean...` and
/// single-letter transform buttons. Model and Sketch follow the active
/// workspace; View is available from both.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum RibbonTab {
    Model,
    Sketch,
    View,
}

impl RibbonTab {
    pub const ALL: [Self; 3] = [Self::Model, Self::Sketch, Self::View];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Model => "Model",
            Self::Sketch => "Sketch",
            Self::View => "View",
        }
    }

    /// Model and Sketch keep the names the workspace buttons they replaced
    /// already had: it is the same control doing the same job, now drawn as a
    /// tab, and renaming it would break every script and habit that names it.
    /// View is a ribbon tab only, and says so.
    pub const fn accessible_name(self) -> &'static str {
        match self {
            Self::Model => "Model mode",
            Self::Sketch => "Sketch mode",
            Self::View => "View ribbon tab",
        }
    }
}

/// A captioned cluster of commands inside a tab.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub enum RibbonGroupId {
    Create,
    Solid,
    Features,
    Boolean,
    Modify,
    SketchTools,
    Complete,
    SketchSolid,
    SketchView,
    Select,
    Camera,
    Display,
    Motion,
    Panels,
    Appearance,
}

impl RibbonGroupId {
    pub const fn caption(self) -> &'static str {
        match self {
            Self::Create => "CREATE",
            Self::Solid | Self::SketchSolid => "SOLID",
            Self::Features => "FEATURES",
            Self::Boolean => "BOOLEAN",
            Self::Modify => "MODIFY",
            Self::SketchTools => "SKETCH",
            Self::Complete => "COMPLETE",
            Self::SketchView | Self::Display => "VIEW",
            Self::Select => "SELECT",
            Self::Camera => "CAMERA",
            Self::Motion => "MOTION",
            Self::Panels => "PANELS",
            Self::Appearance => "APPEARANCE",
        }
    }

    /// A stable id for `egui::Ui::push_id`, kept separate from the caption so
    /// two groups may share a caption without colliding.
    pub const fn stable_key(self) -> &'static str {
        match self {
            Self::Create => "group_create",
            Self::Solid => "group_solid",
            Self::Features => "group_features",
            Self::Boolean => "group_boolean",
            Self::Modify => "group_modify",
            Self::SketchTools => "group_sketch_tools",
            Self::Complete => "group_complete",
            Self::SketchSolid => "group_sketch_solid",
            Self::SketchView => "group_sketch_view",
            Self::Select => "group_select",
            Self::Camera => "group_camera",
            Self::Display => "group_display",
            Self::Motion => "group_motion",
            Self::Panels => "group_panels",
            Self::Appearance => "group_appearance",
        }
    }
}

/// The visual weight of a command button.
///
/// The reference this UI was measured against uses exactly two: primary
/// commands get a large icon with the name underneath, secondary commands get a
/// small icon with the name beside it. One size for everything says nothing
/// about what matters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandSize {
    Large,
    Small,
}

/// Every command the model and view ribbons offer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ModelCommand {
    NewSketch,
    ConstructionPlane,
    Extrude,
    Revolve,
    Hole,
    Rib,
    Mirror,
    Pattern,
    Chamfer,
    Fillet,
    Combine,
    Subtract,
    Intersect,
    Move,
    Rotate,
    Scale,
    Select,
    Measure,
    Orbit,
    FrameVisible,
    Home,
    ToggleEdges,
    ToggleShaded,
    PlayMotion,
    ShowBrowser,
    ShowProperties,
    ShowHistory,
    ToggleTheme,
    FinishSketch,
    FrameSketch,
    ToggleSnap,
}

/// Static presentation metadata for one command.
#[derive(Clone, Copy, Debug)]
pub struct CommandDescriptor {
    pub command: ModelCommand,
    pub stable_key: &'static str,
    pub tab: RibbonTab,
    pub group: RibbonGroupId,
    pub icon: CommandIcon,
    pub size: CommandSize,
    /// Drawn under (large) or beside (small) the icon.
    pub label: &'static str,
    /// What assistive technology and the UI tests call this command. Held
    /// separate from `label` so the visible text can be shortened without
    /// silently renaming a control somebody has learned or scripted.
    pub accessible_name: &'static str,
    pub tooltip: &'static str,
    pub shortcut: Option<&'static str>,
}

/// One table row. The argument count is the point: every command answers the
/// same questions in the same order, which is what stops a command from
/// quietly shipping without an icon, a name, or a description.
#[allow(clippy::too_many_arguments)]
const fn command(
    command: ModelCommand,
    stable_key: &'static str,
    tab: RibbonTab,
    group: RibbonGroupId,
    icon: CommandIcon,
    size: CommandSize,
    label: &'static str,
    accessible_name: &'static str,
    tooltip: &'static str,
    shortcut: Option<&'static str>,
) -> CommandDescriptor {
    CommandDescriptor {
        command,
        stable_key,
        tab,
        group,
        icon,
        size,
        label,
        accessible_name,
        tooltip,
        shortcut,
    }
}

/// The registry, in ribbon order. Groups render in the order they first appear
/// here, and commands render in the order they are listed within a group.
pub const COMMANDS: &[CommandDescriptor] = &[
    // ---- Model tab -------------------------------------------------------
    command(
        ModelCommand::NewSketch,
        "model.sketch",
        RibbonTab::Model,
        RibbonGroupId::Create,
        CommandIcon::Sketch,
        CommandSize::Large,
        "Sketch",
        // Overridden per state by `KernelLabApp::command_accessible_name`:
        // this is the name when nothing is selected and no sketch exists.
        "Create sketch",
        "Start or reopen a sketch. With a planar face selected the sketch is placed on that face.",
        None,
    ),
    command(
        ModelCommand::ConstructionPlane,
        "model.plane",
        RibbonTab::Model,
        RibbonGroupId::Create,
        CommandIcon::Plane,
        CommandSize::Large,
        "Plane",
        "Plane",
        "Create a construction plane from the selected faces.",
        None,
    ),
    command(
        ModelCommand::Extrude,
        "model.extrude",
        RibbonTab::Model,
        RibbonGroupId::Solid,
        CommandIcon::Extrude,
        CommandSize::Large,
        "Extrude",
        "Extrude",
        "Extrude the active sketch, or push and pull the selected planar face.",
        None,
    ),
    command(
        ModelCommand::Revolve,
        "model.revolve",
        RibbonTab::Model,
        RibbonGroupId::Solid,
        CommandIcon::Revolve,
        CommandSize::Large,
        "Revolve",
        "Revolve",
        "Revolve a closed profile about a centreline.",
        None,
    ),
    command(
        ModelCommand::Hole,
        "model.hole",
        RibbonTab::Model,
        RibbonGroupId::Features,
        CommandIcon::Hole,
        CommandSize::Small,
        "Hole",
        "Hole",
        "Place an exact hole on the selected planar face.",
        None,
    ),
    command(
        ModelCommand::Rib,
        "model.rib",
        RibbonTab::Model,
        RibbonGroupId::Features,
        CommandIcon::Rib,
        CommandSize::Small,
        "Rib",
        "Rib",
        "Thicken an open profile into a stiffening rib.",
        None,
    ),
    command(
        ModelCommand::Mirror,
        "model.mirror",
        RibbonTab::Model,
        RibbonGroupId::Features,
        CommandIcon::Mirror,
        CommandSize::Small,
        "Mirror",
        "Mirror",
        "Mirror the active body about a plane.",
        None,
    ),
    command(
        ModelCommand::Pattern,
        "model.pattern",
        RibbonTab::Model,
        RibbonGroupId::Features,
        CommandIcon::Pattern,
        CommandSize::Small,
        "Pattern",
        "Pattern",
        "Repeat the active body along a direction.",
        None,
    ),
    command(
        ModelCommand::Chamfer,
        "model.chamfer",
        RibbonTab::Model,
        RibbonGroupId::Features,
        CommandIcon::Chamfer,
        CommandSize::Small,
        "Chamfer",
        "Chamfer",
        "Chamfer the selected edges by an exact distance.",
        None,
    ),
    command(
        ModelCommand::Fillet,
        "model.fillet",
        RibbonTab::Model,
        RibbonGroupId::Features,
        CommandIcon::Fillet,
        CommandSize::Small,
        "Fillet",
        "Fillet",
        "Round the selected edges by an exact radius.",
        None,
    ),
    command(
        ModelCommand::Combine,
        "model.boolean.union",
        RibbonTab::Model,
        RibbonGroupId::Boolean,
        CommandIcon::Combine,
        CommandSize::Small,
        "Combine",
        "Combine",
        "Add the tool bodies to the active body. Click the tools in the viewport, then confirm.",
        None,
    ),
    command(
        ModelCommand::Subtract,
        "model.boolean.difference",
        RibbonTab::Model,
        RibbonGroupId::Boolean,
        CommandIcon::Subtract,
        CommandSize::Small,
        "Subtract",
        "Subtract",
        "Remove the tool bodies from the active body. Click the tools in the viewport, then confirm.",
        None,
    ),
    command(
        ModelCommand::Intersect,
        "model.boolean.intersection",
        RibbonTab::Model,
        RibbonGroupId::Boolean,
        CommandIcon::Intersect,
        CommandSize::Small,
        "Intersect",
        "Intersect",
        "Keep only what the active body and the tool bodies share. Click the tools in the viewport, then confirm.",
        None,
    ),
    command(
        ModelCommand::Select,
        "view.select",
        RibbonTab::Model,
        RibbonGroupId::Select,
        CommandIcon::Select,
        CommandSize::Small,
        "Select",
        "V  Select",
        "Select vertices, edges and faces · keyboard V",
        Some("V"),
    ),
    command(
        ModelCommand::Measure,
        "view.measure",
        RibbonTab::Model,
        RibbonGroupId::Select,
        CommandIcon::Measure,
        CommandSize::Small,
        "Measure",
        "I  Measure",
        "Measure between picked entities · keyboard I",
        Some("I"),
    ),
    command(
        ModelCommand::Orbit,
        "view.orbit",
        RibbonTab::Model,
        RibbonGroupId::Select,
        CommandIcon::Orbit,
        CommandSize::Small,
        "Orbit",
        "O  Orbit",
        "Orbit the camera · keyboard O",
        Some("O"),
    ),
    command(
        ModelCommand::Move,
        "model.move",
        RibbonTab::Model,
        RibbonGroupId::Modify,
        CommandIcon::Move,
        CommandSize::Small,
        "Move",
        "M  Move",
        "Move transform-preview tool · keyboard M",
        Some("M"),
    ),
    command(
        ModelCommand::Rotate,
        "model.rotate",
        RibbonTab::Model,
        RibbonGroupId::Modify,
        CommandIcon::Rotate,
        CommandSize::Small,
        "Rotate",
        "R  Rotate",
        "Rotate transform-preview tool · keyboard R",
        Some("R"),
    ),
    command(
        ModelCommand::Scale,
        "model.scale",
        RibbonTab::Model,
        RibbonGroupId::Modify,
        CommandIcon::Scale,
        CommandSize::Small,
        "Scale",
        "S  Scale",
        "Scale transform-preview tool · keyboard S",
        Some("S"),
    ),
    // ---- Sketch tab ------------------------------------------------------
    // The tool grid is not a registry command: it is the sketch crate's own
    // registry-driven toolbar, rendered whole into this group.
    command(
        ModelCommand::FinishSketch,
        "sketch.finish",
        RibbonTab::Sketch,
        RibbonGroupId::Complete,
        CommandIcon::Finish,
        CommandSize::Large,
        "Finish",
        "Finish sketch command",
        "Publish the sketch and return to the model workspace.",
        None,
    ),
    command(
        ModelCommand::Extrude,
        "sketch.extrude",
        RibbonTab::Sketch,
        RibbonGroupId::SketchSolid,
        CommandIcon::Extrude,
        CommandSize::Large,
        "Extrude",
        "Extrude",
        "Extrude the active sketch.",
        None,
    ),
    command(
        ModelCommand::FrameSketch,
        "sketch.frame",
        RibbonTab::Sketch,
        RibbonGroupId::SketchView,
        CommandIcon::Frame,
        CommandSize::Small,
        "Frame sketch",
        "Frame sketch",
        "Fit the sketch to the canvas.",
        None,
    ),
    command(
        ModelCommand::ToggleSnap,
        "sketch.snap",
        RibbonTab::Sketch,
        RibbonGroupId::SketchView,
        CommandIcon::Snap,
        CommandSize::Small,
        "Snap",
        "Snap",
        "Snap new points to the grid and to existing endpoints.",
        None,
    ),
    // ---- View tab --------------------------------------------------------
    command(
        ModelCommand::FrameVisible,
        "view.frame",
        RibbonTab::View,
        RibbonGroupId::Camera,
        CommandIcon::Frame,
        CommandSize::Large,
        "Frame",
        "Frame",
        "Frame the selection, or every visible body when nothing is selected.",
        Some("F"),
    ),
    command(
        ModelCommand::Home,
        "view.home",
        RibbonTab::View,
        RibbonGroupId::Camera,
        CommandIcon::Home,
        CommandSize::Small,
        "Home",
        "Home",
        "Reset the camera to the home view.",
        None,
    ),
    command(
        ModelCommand::ToggleEdges,
        "view.edges",
        RibbonTab::View,
        RibbonGroupId::Display,
        CommandIcon::Edges,
        CommandSize::Small,
        "Edges",
        "Edges",
        "Toggle the diagnostic source-edge overlay.",
        None,
    ),
    command(
        ModelCommand::ToggleShaded,
        "view.shaded",
        RibbonTab::View,
        RibbonGroupId::Display,
        CommandIcon::Shaded,
        CommandSize::Small,
        "Shaded",
        "Shaded",
        "Toggle shaded display; diagnostic mode retains face roles and labels.",
        None,
    ),
    command(
        ModelCommand::PlayMotion,
        "view.motion",
        RibbonTab::View,
        RibbonGroupId::Motion,
        CommandIcon::Play,
        CommandSize::Large,
        "Play",
        "Play motion",
        "Play the authored motion temporarily; stopping restores the authored pose.",
        None,
    ),
    command(
        ModelCommand::ShowBrowser,
        "view.browser",
        RibbonTab::View,
        RibbonGroupId::Panels,
        CommandIcon::Browser,
        CommandSize::Small,
        "Browser",
        "Show browser panel",
        "Show the feature and body browser down the left of the workspace.",
        None,
    ),
    command(
        ModelCommand::ShowProperties,
        "view.properties",
        RibbonTab::View,
        RibbonGroupId::Panels,
        CommandIcon::Properties,
        CommandSize::Small,
        "Properties",
        "Show properties panel",
        "Show the properties palette down the right of the workspace.",
        None,
    ),
    command(
        ModelCommand::ShowHistory,
        "view.history",
        RibbonTab::View,
        RibbonGroupId::Panels,
        CommandIcon::History,
        CommandSize::Small,
        "History",
        "Show history panel",
        "Show the parametric history strip along the bottom of the workspace.",
        None,
    ),
    command(
        ModelCommand::ToggleTheme,
        "view.theme",
        RibbonTab::View,
        RibbonGroupId::Appearance,
        CommandIcon::Theme,
        CommandSize::Large,
        "Theme",
        "Switch theme",
        "Switch between the light and dark workbench themes.",
        None,
    ),
];

/// The commands of one tab, grouped in registry order.
#[must_use]
pub fn groups_for_tab(tab: RibbonTab) -> Vec<(RibbonGroupId, Vec<&'static CommandDescriptor>)> {
    let mut groups: Vec<(RibbonGroupId, Vec<&'static CommandDescriptor>)> = Vec::new();
    for descriptor in COMMANDS.iter().filter(|entry| entry.tab == tab) {
        match groups.last_mut() {
            Some((group, members)) if *group == descriptor.group => members.push(descriptor),
            _ => groups.push((descriptor.group, vec![descriptor])),
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_command_has_a_unique_stable_key() {
        let mut keys = BTreeSet::new();
        for descriptor in COMMANDS {
            assert!(
                keys.insert(descriptor.stable_key),
                "duplicate stable key {}",
                descriptor.stable_key
            );
        }
    }

    /// kittest's `get_by_role_and_label` panics on an ambiguous match, and a
    /// screen reader has the same problem in slower motion. A name that appears
    /// twice in one tab is not a name.
    #[test]
    fn accessible_names_are_unambiguous_within_a_tab() {
        for tab in RibbonTab::ALL {
            let mut names = BTreeSet::new();
            for (_, members) in groups_for_tab(tab) {
                for descriptor in members {
                    assert!(
                        names.insert(descriptor.accessible_name),
                        "{} appears twice in the {} tab",
                        descriptor.accessible_name,
                        tab.label()
                    );
                }
            }
        }
    }

    #[test]
    fn every_command_is_named_and_described() {
        for descriptor in COMMANDS {
            assert!(!descriptor.label.is_empty(), "{}", descriptor.stable_key);
            assert!(
                !descriptor.accessible_name.is_empty(),
                "{}",
                descriptor.stable_key
            );
            assert!(
                descriptor.tooltip.len() > 20,
                "{} needs a tooltip that explains it, not a restatement of its label",
                descriptor.stable_key
            );
        }
    }

    /// Groups are rendered in first-appearance order, so a group whose members
    /// are not contiguous in the table would silently render twice.
    #[test]
    fn each_group_appears_once_per_tab() {
        for tab in RibbonTab::ALL {
            let groups = groups_for_tab(tab);
            let unique = groups
                .iter()
                .map(|(group, _)| *group)
                .collect::<BTreeSet<_>>();
            assert_eq!(
                groups.len(),
                unique.len(),
                "a group is split across the {} tab",
                tab.label()
            );
        }
    }

    #[test]
    fn every_tab_has_commands() {
        for tab in RibbonTab::ALL {
            assert!(
                !groups_for_tab(tab).is_empty(),
                "{} has no commands; an empty tab is worse than no tab",
                tab.label()
            );
        }
    }

    /// The ten commands that used to be reachable only by opening a dropdown
    /// labelled with an ellipsis.
    #[test]
    fn every_solid_feature_and_boolean_is_on_the_surface() {
        let surfaced = COMMANDS
            .iter()
            .map(|descriptor| descriptor.accessible_name)
            .collect::<BTreeSet<_>>();
        for name in [
            "Revolve",
            "Hole",
            "Rib",
            "Mirror",
            "Pattern",
            "Chamfer",
            "Fillet",
            "Combine",
            "Subtract",
            "Intersect",
        ] {
            assert!(surfaced.contains(name), "{name} is still hidden in a menu");
        }
    }
}
