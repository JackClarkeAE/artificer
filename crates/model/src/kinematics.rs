//! Where every component actually is once the joints are driven.
//!
//! The document stores each component's *assembled* pose: where it sits
//! when every joint is at zero. A joint says how that pose is allowed to
//! change. This module turns the two into the third thing a viewport, an
//! interference study and an export all need — the world pose of every
//! component at a given set of driver values.
//!
//! ## What a joint moves
//!
//! A revolute joint carries an origin and an axis in the assembled world
//! frame, which is where the user picked them. Driving it by θ rotates the
//! child about that world line, and the child's whole subtree with it:
//! turning a hinge carries the door, and the handle on the door.
//!
//! So each component gets a *motion* — the rigid transform between where
//! it was assembled and where the drivers have put it — and its world pose
//! is that motion applied to its assembled pose. Every motion is the
//! identity at zero, which is what makes the assembled document the thing
//! the drivers move away from rather than a separate configuration nobody
//! authored.
//!
//! ## What it refuses
//!
//! A driver for a joint that is not there, is not revolute, is disabled,
//! or already has one; a driver outside the joint's own limits; and a
//! cycle. The document's own editing rules already prevent cycles and
//! second parents, but a document that arrived from disk has not been
//! through those rules, and a solver that loops forever on one is worse
//! than a solver that names it.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::assembly::{JointKind, JointParent};
use crate::components::{CanonicalQuaternion, ComponentTranslation, RigidComponentPose};
use crate::{ComponentInstanceId, JointId, ModelDocument};

/// One revolute joint held at an angle, in radians.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JointDriver {
    pub joint: JointId,
    pub radians: f64,
}

impl JointDriver {
    #[must_use]
    pub const fn new(joint: JointId, radians: f64) -> Self {
        Self { joint, radians }
    }
}

/// Why a set of drivers could not be posed.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum KinematicsError {
    #[error("no joint {0:?} in this document")]
    UnknownJoint(JointId),
    #[error("joint {0:?} is fixed and cannot be driven")]
    JointIsFixed(JointId),
    #[error("joint {0:?} is disabled and cannot be driven")]
    JointIsDisabled(JointId),
    #[error("joint {0:?} was given more than one driver")]
    DuplicateDriver(JointId),
    #[error("the driver for joint {joint:?} is outside the joint's own limits")]
    OutsideLimits { joint: JointId },
    #[error("the driver for joint {0:?} is not a finite angle")]
    NonFiniteDriver(JointId),
    #[error("the joints below component {0:?} form a cycle")]
    Cycle(ComponentInstanceId),
    #[error("the pose of component {0:?} left the supported model range")]
    OutOfRange(ComponentInstanceId),
}

/// Every component's world pose at one set of driver values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Kinematics {
    poses: BTreeMap<ComponentInstanceId, RigidComponentPose>,
}

impl Kinematics {
    /// The world pose of one component, or `None` for a component this
    /// document does not have.
    #[must_use]
    pub fn pose(&self, component: ComponentInstanceId) -> Option<RigidComponentPose> {
        self.poses.get(&component).copied()
    }

    /// Every component and where it is, in stable component order.
    pub fn poses(&self) -> impl Iterator<Item = (ComponentInstanceId, RigidComponentPose)> + '_ {
        self.poses.iter().map(|(id, pose)| (*id, *pose))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.poses.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.poses.is_empty()
    }
}

/// Poses every component of a document at the given driver values.
///
/// Joints with no driver rest at zero, which is the assembled pose the
/// document stores. A component with no parent joint is a root and stands
/// where it was put, grounded or not: grounding is what forbids it a
/// parent, not what pins it.
pub fn solve(
    document: &ModelDocument,
    drivers: &[JointDriver],
) -> Result<Kinematics, KinematicsError> {
    let angles = driver_angles(document, drivers)?;

    // The parent of each component, and the motion its own joint adds.
    let mut parents: BTreeMap<ComponentInstanceId, Option<ComponentInstanceId>> = BTreeMap::new();
    let mut local: BTreeMap<ComponentInstanceId, Rigid> = BTreeMap::new();
    for joint in document.joints() {
        // A disabled joint is still a structural edge — the child follows
        // its parent — it simply cannot be driven, so it contributes the
        // identity the way a fixed joint does.
        let motion = match joint.kind {
            JointKind::Revolute { origin, axis, .. } if joint.enabled => {
                let radians = angles.get(&joint.id).copied().unwrap_or(0.0);
                Rigid::rotation_about(
                    [origin.x(), origin.y(), origin.z()],
                    [axis.x(), axis.y(), axis.z()],
                    radians,
                )
            }
            _ => Rigid::IDENTITY,
        };
        parents.insert(
            joint.child,
            match joint.parent {
                JointParent::World => None,
                JointParent::Component(parent) => Some(parent),
            },
        );
        local.insert(joint.child, motion);
    }

    let mut motions: BTreeMap<ComponentInstanceId, Rigid> = BTreeMap::new();
    let mut poses = BTreeMap::new();
    for component in document.component_instances() {
        let motion = motion_of(component.id, &parents, &local, &mut motions)?;
        poses.insert(
            component.id,
            motion
                .apply_to(component.pose)
                .ok_or(KinematicsError::OutOfRange(component.id))?,
        );
    }
    Ok(Kinematics { poses })
}

/// The driver angle per joint, checked against the document and the
/// joints' own limits.
fn driver_angles(
    document: &ModelDocument,
    drivers: &[JointDriver],
) -> Result<BTreeMap<JointId, f64>, KinematicsError> {
    let mut angles = BTreeMap::new();
    for driver in drivers {
        let joint = document
            .joint(driver.joint)
            .ok_or(KinematicsError::UnknownJoint(driver.joint))?;
        let JointKind::Revolute { limits, .. } = joint.kind else {
            return Err(KinematicsError::JointIsFixed(driver.joint));
        };
        if !joint.enabled {
            return Err(KinematicsError::JointIsDisabled(driver.joint));
        }
        if !driver.radians.is_finite() {
            return Err(KinematicsError::NonFiniteDriver(driver.joint));
        }
        // A limit is a promise the model makes about the mechanism, so a
        // driver past it is a caller error rather than something to clamp
        // quietly into range: a sweep that silently stopped at the stop
        // would report clearances the mechanism never reaches.
        if let Some(limits) = limits
            && (driver.radians < limits.min_radians() || driver.radians > limits.max_radians())
        {
            return Err(KinematicsError::OutsideLimits {
                joint: driver.joint,
            });
        }
        if angles.insert(driver.joint, driver.radians).is_some() {
            return Err(KinematicsError::DuplicateDriver(driver.joint));
        }
    }
    Ok(angles)
}

/// The accumulated motion of one component, memoised up the chain.
fn motion_of(
    component: ComponentInstanceId,
    parents: &BTreeMap<ComponentInstanceId, Option<ComponentInstanceId>>,
    local: &BTreeMap<ComponentInstanceId, Rigid>,
    motions: &mut BTreeMap<ComponentInstanceId, Rigid>,
) -> Result<Rigid, KinematicsError> {
    if let Some(known) = motions.get(&component) {
        return Ok(*known);
    }
    // The chain from this component up to its root, refusing a cycle
    // rather than walking one forever.
    let mut chain = Vec::new();
    let mut seen = BTreeSet::new();
    let mut cursor = Some(component);
    let mut inherited = Rigid::IDENTITY;
    while let Some(current) = cursor {
        if !seen.insert(current) {
            return Err(KinematicsError::Cycle(component));
        }
        if let Some(known) = motions.get(&current) {
            inherited = *known;
            break;
        }
        let Some(parent) = parents.get(&current) else {
            // No parent joint: this component is a root and does not move.
            break;
        };
        chain.push(current);
        cursor = *parent;
    }
    // Down again, so each component's motion is its parent's with its own
    // joint applied after it.
    for current in chain.into_iter().rev() {
        let step = local.get(&current).copied().unwrap_or(Rigid::IDENTITY);
        inherited = inherited.then(step);
        motions.insert(current, inherited);
    }
    let motion = motions.get(&component).copied().unwrap_or(inherited);
    motions.insert(component, motion);
    Ok(motion)
}

/// A rigid transform, as the solver composes them: rotate, then translate.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rigid {
    /// Unit quaternion `[w, x, y, z]`, not sign-canonicalized: composing
    /// canonical quaternions is not closed under the canonical sign, and
    /// flipping one mid-chain would flip the rotation it stands for.
    rotation: [f64; 4],
    translation: [f64; 3],
}

impl Rigid {
    const IDENTITY: Self = Self {
        rotation: [1.0, 0.0, 0.0, 0.0],
        translation: [0.0, 0.0, 0.0],
    };

    /// A turn of `radians` about the world line through `origin` along
    /// `axis`. A zero-length or non-finite axis cannot turn anything and
    /// gives the identity, which is what the joint's own validation has
    /// already made unreachable.
    fn rotation_about(origin: [f64; 3], axis: [f64; 3], radians: f64) -> Self {
        let norm = axis[0].hypot(axis[1]).hypot(axis[2]);
        if !norm.is_finite() || norm <= f64::EPSILON || !radians.is_finite() {
            return Self::IDENTITY;
        }
        let half = radians / 2.0;
        let scale = half.sin() / norm;
        let rotation = [
            half.cos(),
            axis[0] * scale,
            axis[1] * scale,
            axis[2] * scale,
        ];
        // Turning about a line, not about the origin: the point on the
        // line has to come back to itself.
        let turned = rotate(rotation, origin);
        Self {
            rotation,
            translation: [
                origin[0] - turned[0],
                origin[1] - turned[1],
                origin[2] - turned[2],
            ],
        }
    }

    /// `self` first, then `next`.
    fn then(self, next: Self) -> Self {
        let translation = rotate(next.rotation, self.translation);
        Self {
            rotation: multiply(next.rotation, self.rotation),
            translation: [
                translation[0] + next.translation[0],
                translation[1] + next.translation[1],
                translation[2] + next.translation[2],
            ],
        }
    }

    /// The world pose a component assembled at `pose` ends up in.
    fn apply_to(self, pose: RigidComponentPose) -> Option<RigidComponentPose> {
        let assembled = [
            pose.translation.x(),
            pose.translation.y(),
            pose.translation.z(),
        ];
        let moved = rotate(self.rotation, assembled);
        let rotation = multiply(
            self.rotation,
            [
                pose.rotation.w(),
                pose.rotation.x(),
                pose.rotation.y(),
                pose.rotation.z(),
            ],
        );
        Some(RigidComponentPose::new(
            ComponentTranslation::new(
                moved[0] + self.translation[0],
                moved[1] + self.translation[1],
                moved[2] + self.translation[2],
            )
            .ok()?,
            CanonicalQuaternion::new(rotation[0], rotation[1], rotation[2], rotation[3]).ok()?,
        ))
    }
}

/// Hamilton product, `left` applied after `right`.
fn multiply(left: [f64; 4], right: [f64; 4]) -> [f64; 4] {
    let [lw, lx, ly, lz] = left;
    let [rw, rx, ry, rz] = right;
    [
        lw * rw - lx * rx - ly * ry - lz * rz,
        lw * rx + lx * rw + ly * rz - lz * ry,
        lw * ry - lx * rz + ly * rw + lz * rx,
        lw * rz + lx * ry - ly * rx + lz * rw,
    ]
}

/// `v + 2w(q × v) + 2(q × (q × v))`, for a unit quaternion.
fn rotate(rotation: [f64; 4], point: [f64; 3]) -> [f64; 3] {
    let [w, x, y, z] = rotation;
    let axis = [x, y, z];
    let first = cross(axis, point);
    let second = cross(axis, first);
    [
        2.0f64.mul_add(w.mul_add(first[0], second[0]), point[0]),
        2.0f64.mul_add(w.mul_add(first[1], second[1]), point[1]),
        2.0f64.mul_add(w.mul_add(first[2], second[2]), point[2]),
    ]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1].mul_add(b[2], -(a[2] * b[1])),
        a[2].mul_add(b[0], -(a[0] * b[2])),
        a[0].mul_add(b[1], -(a[1] * b[0])),
    ]
}
