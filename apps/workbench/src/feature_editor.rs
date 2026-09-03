//! Editing a committed feature's own numbers.
//!
//! The kernel commands behind Hole, Rib, Pattern and the edge finishes all
//! carry their dimensions as ordinary fields, and the document layer has
//! always been able to swap one action for another and rebuild from there.
//! What was missing is the part in between: a description of which numbers a
//! given command exposes, and how to put an edited one back.
//!
//! That description lives here rather than in the panel so it can be tested
//! without a UI. The panel walks [`editable_scalars`] to draw a row per
//! number and calls [`with_scalar`] to build the replacement command; nothing
//! about which fields exist is encoded in the widget code.

use artificer_model::ReplayAction;
use artificer_model::persistent::TargetedKernel;
use artificer_protocol::KernelCommand;

/// How a scalar is entered and what bounds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarKind {
    /// A length in millimetres. Must stay strictly positive.
    Length,
    /// A whole count. Must stay at least one.
    Count,
}

/// One editable number on a committed feature.
#[derive(Clone, Debug, PartialEq)]
pub struct EditableScalar {
    /// Field name as the panel labels it.
    pub label: &'static str,
    pub value: f64,
    pub kind: ScalarKind,
}

impl EditableScalar {
    const fn length(label: &'static str, value: f64) -> Self {
        Self {
            label,
            value,
            kind: ScalarKind::Length,
        }
    }

    const fn count(label: &'static str, value: f64) -> Self {
        Self {
            label,
            value,
            kind: ScalarKind::Count,
        }
    }
}

/// The numbers this command lets a user change after the fact.
///
/// Order is the order the panel draws them in, and it is also the index
/// [`with_scalar`] takes, so the two must be read together. A command with no
/// entry here is one whose shape is not a handful of independent scalars —
/// a sketch profile, a mirror plane — and it returns nothing rather than
/// exposing a number that would not mean anything on its own.
#[must_use]
pub fn editable_scalars(command: &KernelCommand) -> Vec<EditableScalar> {
    match command {
        KernelCommand::DrillHole {
            diameter, depth, ..
        } => vec![
            EditableScalar::length("Diameter", *diameter),
            EditableScalar::length("Depth", *depth),
        ],
        KernelCommand::AddRib {
            thickness, height, ..
        } => vec![
            EditableScalar::length("Thickness", *thickness),
            EditableScalar::length("Height", *height),
        ],
        KernelCommand::ShellSnapshot { wall, .. } => vec![EditableScalar::length("Wall", *wall)],
        KernelCommand::LinearPatternSnapshot { spacing, count, .. } => vec![
            EditableScalar::length("Spacing", *spacing),
            EditableScalar::count("Count", f64::from(*count)),
        ],
        KernelCommand::FinishEdge { distance, .. }
        | KernelCommand::FinishEdges { distance, .. } => {
            vec![EditableScalar::length("Distance", *distance)]
        }
        KernelCommand::MakeCuboid {
            size_x,
            size_y,
            size_z,
            ..
        } => vec![
            EditableScalar::length("Length X", *size_x),
            EditableScalar::length("Length Y", *size_y),
            EditableScalar::length("Length Z", *size_z),
        ],
        KernelCommand::ExtrudePlanarProfile { distance, .. }
        | KernelCommand::ExtrudeFaceProfile { distance, .. }
        | KernelCommand::ExtrudeFacePlanarProfile { distance, .. }
        | KernelCommand::ExtrudePolygon { distance, .. } => {
            vec![EditableScalar::length("Distance", *distance)]
        }
        // A loft's offset is signed and may be anything but zero-collapsing,
        // so only the distance is offered as a plain length here; the draft
        // itself is edited through the sketch-region recipe.
        KernelCommand::LoftPlanarProfileOffset { distance, .. } => {
            vec![EditableScalar::length("Distance", *distance)]
        }
        _ => Vec::new(),
    }
}

/// The largest count a pattern may be edited to.
///
/// Every copy is a separate solid the kernel reconstructs and validates, so
/// a mistyped count is a long rebuild rather than a wrong number. This is a
/// guard against the typo, not a statement about what the kernel can carry.
pub const MAX_PATTERN_COUNT: u16 = 512;

/// Returns `command` with scalar `index` set to `value`, or `None` when the
/// index does not name a scalar on this command or the value is out of range.
///
/// Rejecting here rather than clamping is deliberate: a silently clamped
/// dimension is a number the user did not ask for, sitting in a feature
/// history that claims to replay exactly.
#[must_use]
pub fn with_scalar(command: &KernelCommand, index: usize, value: f64) -> Option<KernelCommand> {
    let scalars = editable_scalars(command);
    let target = scalars.get(index)?;
    if !value.is_finite() {
        return None;
    }
    match target.kind {
        ScalarKind::Length if value <= 0.0 => return None,
        ScalarKind::Count if !(1.0..=f64::from(MAX_PATTERN_COUNT)).contains(&value) => return None,
        _ => {}
    }

    let mut edited = command.clone();
    match (&mut edited, index) {
        (KernelCommand::DrillHole { diameter, .. }, 0) => *diameter = value,
        (KernelCommand::DrillHole { depth, .. }, 1) => *depth = value,
        (KernelCommand::AddRib { thickness, .. }, 0) => *thickness = value,
        (KernelCommand::AddRib { height, .. }, 1) => *height = value,
        (KernelCommand::ShellSnapshot { wall, .. }, 0) => *wall = value,
        (KernelCommand::LinearPatternSnapshot { spacing, .. }, 0) => *spacing = value,
        (KernelCommand::LinearPatternSnapshot { count, .. }, 1) => {
            // Already range-checked above, so the rounding cannot saturate.
            *count = value.round() as u16;
        }
        (KernelCommand::FinishEdge { distance, .. }, 0)
        | (KernelCommand::FinishEdges { distance, .. }, 0) => *distance = value,
        (KernelCommand::MakeCuboid { size_x, .. }, 0) => *size_x = value,
        (KernelCommand::MakeCuboid { size_y, .. }, 1) => *size_y = value,
        (KernelCommand::MakeCuboid { size_z, .. }, 2) => *size_z = value,
        (KernelCommand::ExtrudePlanarProfile { distance, .. }, 0)
        | (KernelCommand::ExtrudeFaceProfile { distance, .. }, 0)
        | (KernelCommand::ExtrudeFacePlanarProfile { distance, .. }, 0)
        | (KernelCommand::ExtrudePolygon { distance, .. }, 0) => *distance = value,
        (
            KernelCommand::LoftPlanarProfileOffset {
                distance, offset, ..
            },
            0,
        ) => {
            // Keep the draft angle: the offset scales with the height.
            *offset *= value / *distance;
            *distance = value;
        }
        _ => return None,
    }
    Some(edited)
}

/// The editable numbers on a committed feature's replay action.
///
/// Only the two actions that carry a plain kernel command expose anything.
/// A parameterized recipe already has a named parameter driving it and must
/// be edited there instead, and a sketch or Boolean recipe's shape lives in
/// its sketch or its operands rather than in a scalar field.
#[must_use]
pub fn action_scalars(action: &ReplayAction) -> Vec<EditableScalar> {
    match action {
        ReplayAction::Kernel(command) => editable_scalars(command),
        ReplayAction::TargetedKernel(targeted) => editable_scalars(targeted.command_template()),
        _ => Vec::new(),
    }
}

/// Returns `action` with scalar `index` set to `value`.
///
/// The persistent target recipe is carried across untouched: editing a hole's
/// diameter must not disturb which face it is drilled into, and rebuilding
/// the `TargetedKernel` through its own constructor keeps the target-kind
/// invariant checked rather than assumed.
#[must_use]
pub fn with_action_scalar(action: &ReplayAction, index: usize, value: f64) -> Option<ReplayAction> {
    match action {
        ReplayAction::Kernel(command) => {
            with_scalar(command, index, value).map(ReplayAction::Kernel)
        }
        ReplayAction::TargetedKernel(targeted) => {
            let edited = with_scalar(targeted.command_template(), index, value)?;
            let targets = targeted.targets().cloned().collect();
            TargetedKernel::new_many(edited, targets)
                .ok()
                .map(ReplayAction::TargetedKernel)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use artificer_protocol::{
        EdgeFinishKind, EntityId, EntityKind, EntityRef, PlanarFrame3, Point2, Point3, SnapshotId,
        Vector3,
    };

    fn face() -> EntityRef {
        EntityRef {
            snapshot: SnapshotId::ZERO,
            entity: EntityId(1),
            kind: EntityKind::Face,
        }
    }

    fn edge() -> EntityRef {
        EntityRef {
            kind: EntityKind::Edge,
            ..face()
        }
    }

    fn frame() -> PlanarFrame3 {
        PlanarFrame3::new(
            Point3::new(0.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        )
    }

    fn hole(diameter: f64, depth: f64) -> KernelCommand {
        KernelCommand::DrillHole {
            target_face: face(),
            frame: frame(),
            center: Point2::new(0.0, 0.0),
            diameter,
            depth,
        }
    }

    #[test]
    fn a_hole_exposes_its_diameter_and_depth_in_that_order() {
        let scalars = editable_scalars(&hole(1.0, 1000.0));
        assert_eq!(
            scalars,
            vec![
                EditableScalar::length("Diameter", 1.0),
                EditableScalar::length("Depth", 1000.0),
            ]
        );
    }

    #[test]
    fn editing_a_hole_changes_only_the_field_asked_for() {
        let edited = with_scalar(&hole(1.0, 1000.0), 0, 6.5).expect("diameter is editable");
        let KernelCommand::DrillHole {
            diameter,
            depth,
            center,
            ..
        } = edited
        else {
            panic!("editing a hole must leave it a hole");
        };
        assert!((diameter - 6.5).abs() < 1e-12);
        assert!((depth - 1000.0).abs() < 1e-12, "depth must not move");
        assert_eq!(center, Point2::new(0.0, 0.0), "position must not move");
    }

    #[test]
    fn a_dimension_that_is_not_a_dimension_is_refused_rather_than_clamped() {
        // A clamped value is a number the user did not type sitting in a
        // history that claims to replay exactly.
        for bad in [0.0, -3.0, f64::NAN, f64::INFINITY] {
            assert!(
                with_scalar(&hole(1.0, 10.0), 0, bad).is_none(),
                "{bad} is not a diameter"
            );
        }
    }

    #[test]
    fn an_index_past_the_end_edits_nothing() {
        assert!(with_scalar(&hole(1.0, 10.0), 2, 5.0).is_none());
        assert!(with_scalar(&hole(1.0, 10.0), usize::MAX, 5.0).is_none());
    }

    #[test]
    fn a_pattern_count_is_whole_and_bounded() {
        let pattern = KernelCommand::LinearPatternSnapshot {
            direction: Vector3::new(1.0, 0.0, 0.0),
            spacing: 5.0,
            count: 3,
        };
        assert_eq!(
            editable_scalars(&pattern),
            vec![
                EditableScalar::length("Spacing", 5.0),
                EditableScalar::count("Count", 3.0),
            ]
        );

        let KernelCommand::LinearPatternSnapshot { count, spacing, .. } =
            with_scalar(&pattern, 1, 7.4).expect("a count rounds to a whole number")
        else {
            panic!("editing a pattern must leave it a pattern");
        };
        assert_eq!(count, 7);
        assert!((spacing - 5.0).abs() < 1e-12);

        assert!(with_scalar(&pattern, 1, 0.0).is_none(), "zero copies");
        assert!(
            with_scalar(&pattern, 1, f64::from(MAX_PATTERN_COUNT) + 1.0).is_none(),
            "a mistyped count must not start a runaway rebuild"
        );
        assert!(with_scalar(&pattern, 1, f64::from(MAX_PATTERN_COUNT)).is_some());
    }

    #[test]
    fn a_rib_and_an_edge_finish_expose_their_own_dimensions() {
        let rib = KernelCommand::AddRib {
            target_face: face(),
            frame: frame(),
            start: Point2::new(-0.75, 0.0),
            end: Point2::new(0.75, 0.0),
            thickness: 0.5,
            height: 1.0,
        };
        assert_eq!(
            editable_scalars(&rib)
                .iter()
                .map(|s| s.label)
                .collect::<Vec<_>>(),
            vec!["Thickness", "Height"]
        );
        let KernelCommand::AddRib { height, start, .. } =
            with_scalar(&rib, 1, 4.0).expect("height is editable")
        else {
            panic!("editing a rib must leave it a rib");
        };
        assert!((height - 4.0).abs() < 1e-12);
        assert_eq!(start, Point2::new(-0.75, 0.0), "the centre line must hold");

        let fillet = KernelCommand::FinishEdge {
            target_edge: edge(),
            kind: EdgeFinishKind::Fillet,
            distance: 1.0,
        };
        assert_eq!(editable_scalars(&fillet).len(), 1);
        let KernelCommand::FinishEdge { distance, kind, .. } =
            with_scalar(&fillet, 0, 2.5).expect("a fillet radius is editable")
        else {
            panic!("editing a fillet must leave it a fillet");
        };
        assert!((distance - 2.5).abs() < 1e-12);
        assert_eq!(kind, EdgeFinishKind::Fillet, "the kind must not flip");
    }

    #[test]
    fn a_command_whose_shape_is_not_scalars_offers_none() {
        let mirror = KernelCommand::MirrorSnapshot {
            plane_origin: Point3::new(0.0, 0.0, 0.0),
            plane_normal: Vector3::new(1.0, 0.0, 0.0),
        };
        assert!(editable_scalars(&mirror).is_empty());
        assert!(with_scalar(&mirror, 0, 5.0).is_none());
    }

    #[test]
    fn every_advertised_scalar_can_actually_be_written_back() {
        // The panel indexes `with_scalar` by position in `editable_scalars`,
        // so a label with no matching write arm would draw a field that
        // silently does nothing.
        let commands = [
            hole(1.0, 10.0),
            KernelCommand::AddRib {
                target_face: face(),
                frame: frame(),
                start: Point2::new(-1.0, 0.0),
                end: Point2::new(1.0, 0.0),
                thickness: 0.5,
                height: 1.0,
            },
            KernelCommand::LinearPatternSnapshot {
                direction: Vector3::new(1.0, 0.0, 0.0),
                spacing: 5.0,
                count: 3,
            },
            KernelCommand::FinishEdge {
                target_edge: edge(),
                kind: EdgeFinishKind::Chamfer,
                distance: 1.0,
            },
            KernelCommand::FinishEdges {
                target_edges: vec![edge()],
                kind: EdgeFinishKind::Fillet,
                distance: 1.0,
            },
            KernelCommand::MakeCuboid {
                origin: Point3::new(0.0, 0.0, 0.0),
                size_x: 2.0,
                size_y: 3.0,
                size_z: 4.0,
            },
        ];
        for command in &commands {
            let scalars = editable_scalars(command);
            assert!(!scalars.is_empty(), "{command:?} advertises nothing");
            for (index, scalar) in scalars.iter().enumerate() {
                let probe = match scalar.kind {
                    ScalarKind::Length => 3.25,
                    ScalarKind::Count => 4.0,
                };
                let edited = with_scalar(command, index, probe).unwrap_or_else(|| {
                    panic!(
                        "{command:?} advertises {} but cannot write it",
                        scalar.label
                    )
                });
                assert_eq!(
                    editable_scalars(&edited)[index].value,
                    probe,
                    "{} did not take the value it was given",
                    scalar.label
                );
            }
        }
    }
}
