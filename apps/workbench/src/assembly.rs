//! Deterministic rigid-component placement and editor conversion helpers.
//!
//! This module is intentionally independent of egui and renderer state.  The
//! workbench can feed its display-preview arrays through [`MoveRotatePreview`]
//! and commit the resulting [`RigidComponentPose`] through the model document.
//! No helper in this module permits component scale.

use std::f64::consts::{PI, TAU};
use std::fmt;

use artificer_model::{CanonicalQuaternion, ComponentTranslation, RigidComponentPose};
use artificer_protocol::{Aabb3, Point3};

/// Default world-space gap used when placing another component along +X.
pub const DEFAULT_COMPONENT_INSERTION_CLEARANCE_MM: f64 = 10.0;

const POSE_EPSILON: f64 = 1.0e-10;
const SCALE_EPSILON: f64 = 1.0e-12;

/// XYZ translation fields in canonical model millimetres.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TranslationMm3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl TranslationMm3 {
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0);

    #[must_use]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    #[must_use]
    pub const fn from_array(values: [f64; 3]) -> Self {
        Self::new(values[0], values[1], values[2])
    }

    #[must_use]
    pub const fn to_array(self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }

    fn is_finite(self) -> bool {
        self.to_array().into_iter().all(f64::is_finite)
    }
}

/// Fixed-axis X, then Y, then Z rotation fields in radians.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RotationRadians3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl RotationRadians3 {
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0);

    #[must_use]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    #[must_use]
    pub const fn from_array(values: [f64; 3]) -> Self {
        Self::new(values[0], values[1], values[2])
    }

    #[must_use]
    pub const fn to_array(self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }

    fn is_finite(self) -> bool {
        self.to_array().into_iter().all(f64::is_finite)
    }

    fn normalized(self) -> Self {
        Self::new(
            normalize_radians(self.x),
            normalize_radians(self.y),
            normalize_radians(self.z),
        )
    }
}

/// Fixed-axis X, then Y, then Z rotation fields shown to users in degrees.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RotationDegrees3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl RotationDegrees3 {
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0);

    #[must_use]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    #[must_use]
    pub fn to_radians(self) -> RotationRadians3 {
        RotationRadians3::new(
            self.x.to_radians(),
            self.y.to_radians(),
            self.z.to_radians(),
        )
    }

    #[must_use]
    pub fn from_radians(value: RotationRadians3) -> Self {
        Self::new(
            value.x.to_degrees(),
            value.y.to_degrees(),
            value.z.to_degrees(),
        )
    }

    fn is_finite(self) -> bool {
        [self.x, self.y, self.z].into_iter().all(f64::is_finite)
    }
}

/// Typed, scale-free counterpart of the workbench's move/rotate preview.
///
/// Translation is a world-space delta. Rotation is a fixed-axis world-space
/// delta applied X, then Y, then Z around the supplied preview pivot.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MoveRotatePreview {
    pub translation_mm: TranslationMm3,
    pub rotation_radians: RotationRadians3,
}

impl MoveRotatePreview {
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            translation_mm: TranslationMm3::ZERO,
            rotation_radians: RotationRadians3::ZERO,
        }
    }

    /// Converts the workbench preview's raw parts while explicitly rejecting
    /// scale. This is the intended bridge from `DisplayTransform`.
    pub fn from_display_parts(
        translation: [f64; 3],
        rotation: [f64; 3],
        uniform_scale: f64,
    ) -> Result<Self, AssemblyPlacementError> {
        if !uniform_scale.is_finite() || (uniform_scale - 1.0).abs() > SCALE_EPSILON {
            return Err(AssemblyPlacementError::ComponentScaleUnsupported);
        }
        let preview = Self {
            translation_mm: TranslationMm3::from_array(translation),
            rotation_radians: RotationRadians3::from_array(rotation),
        };
        preview.validate()?;
        Ok(preview)
    }

    #[must_use]
    pub const fn display_parts(self) -> ([f64; 3], [f64; 3], f64) {
        (
            self.translation_mm.to_array(),
            self.rotation_radians.to_array(),
            1.0,
        )
    }

    #[must_use]
    pub fn is_identity(self) -> bool {
        self.translation_mm
            .to_array()
            .into_iter()
            .all(|value| value.abs() <= POSE_EPSILON)
            && self
                .rotation_radians
                .to_array()
                .into_iter()
                .all(|value| normalize_radians(value).abs() <= POSE_EPSILON)
    }

    fn validate(self) -> Result<(), AssemblyPlacementError> {
        if !self.translation_mm.is_finite() {
            return Err(AssemblyPlacementError::InvalidTranslation);
        }
        if !self.rotation_radians.is_finite() {
            return Err(AssemblyPlacementError::InvalidRotation);
        }
        Ok(())
    }
}

/// Absolute XYZ fields for the component Properties panel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ComponentPoseFields {
    pub translation_mm: TranslationMm3,
    pub rotation_degrees: RotationDegrees3,
}

impl ComponentPoseFields {
    /// Decomposes a canonical occurrence pose into stable UI fields.
    #[must_use]
    pub fn from_pose(pose: RigidComponentPose) -> Self {
        Self {
            translation_mm: translation_from_pose(pose),
            rotation_degrees: RotationDegrees3::from_radians(euler_from_quaternion(
                quaternion_from_pose(pose),
            )),
        }
    }

    /// Converts absolute UI fields to a canonical, scale-free occurrence pose.
    pub fn to_pose(self) -> Result<RigidComponentPose, AssemblyPlacementError> {
        if !self.translation_mm.is_finite() {
            return Err(AssemblyPlacementError::InvalidTranslation);
        }
        if !self.rotation_degrees.is_finite() {
            return Err(AssemblyPlacementError::InvalidRotation);
        }
        rigid_pose(
            self.translation_mm,
            quaternion_from_euler(self.rotation_degrees.to_radians()),
        )
    }

    /// Commits absolute fields while preserving a grounded component as an
    /// immutable no-op and rejecting every actual pose change.
    pub fn commit(
        self,
        current: RigidComponentPose,
        grounded: bool,
    ) -> Result<RigidComponentPose, AssemblyPlacementError> {
        let candidate = self.to_pose()?;
        reject_grounded_change(current, candidate, grounded)
    }
}

/// Chooses an identity-rotation initial pose with the new local bounds laid on
/// Y=0/Z=0 and placed after every occupied world AABB along +X.
///
/// Because separation is established on one axis, the result cannot overlap
/// any supplied occupied AABB even when their Y/Z extents differ. Reordering
/// `occupied_world_bounds` cannot change the result.
pub fn deterministic_initial_pose(
    new_local_bounds: Aabb3,
    occupied_world_bounds: &[Aabb3],
    clearance_mm: f64,
) -> Result<RigidComponentPose, AssemblyPlacementError> {
    validate_solid_bounds(new_local_bounds)?;
    if !clearance_mm.is_finite() || clearance_mm < 0.0 {
        return Err(AssemblyPlacementError::InvalidClearance);
    }
    for bounds in occupied_world_bounds {
        validate_solid_bounds(*bounds)?;
    }

    let target_min_x = occupied_world_bounds
        .iter()
        .map(|bounds| bounds.max.x)
        .reduce(f64::max)
        .map_or(0.0, |maximum| (maximum + clearance_mm).max(0.0));
    if !target_min_x.is_finite() {
        return Err(AssemblyPlacementError::InvalidTranslation);
    }
    rigid_pose(
        TranslationMm3::new(
            target_min_x - new_local_bounds.min.x,
            -new_local_bounds.min.y,
            -new_local_bounds.min.z,
        ),
        [1.0, 0.0, 0.0, 0.0],
    )
}

/// Applies a world-space move/rotate preview to a retained component pose.
///
/// The delta rotates around `world_pivot`, matching the viewport preview:
/// `p' = pivot + R_delta * (p - pivot) + translation_delta`.
pub fn compose_move_rotate_preview(
    current: RigidComponentPose,
    preview: MoveRotatePreview,
    world_pivot: Point3,
    grounded: bool,
) -> Result<RigidComponentPose, AssemblyPlacementError> {
    preview.validate()?;
    validate_point(world_pivot)?;
    if preview.is_identity() {
        return Ok(current);
    }
    if grounded {
        return Err(AssemblyPlacementError::GroundedComponent);
    }

    let delta_rotation = quaternion_from_euler(preview.rotation_radians);
    let current_rotation = quaternion_from_pose(current);
    let next_rotation = quaternion_multiply(delta_rotation, current_rotation);
    let current_translation = translation_from_pose(current).to_array();
    let pivot = [world_pivot.x, world_pivot.y, world_pivot.z];
    let rotated_translation = quaternion_rotate(delta_rotation, current_translation);
    let rotated_pivot = quaternion_rotate(delta_rotation, pivot);
    let delta = preview.translation_mm.to_array();
    let next_translation = std::array::from_fn(|index| {
        pivot[index] + delta[index] - rotated_pivot[index] + rotated_translation[index]
    });

    rigid_pose(TranslationMm3::from_array(next_translation), next_rotation)
}

/// Computes the move/rotate preview which maps `current` to `target` around a
/// known world pivot. Applying the returned preview reproduces `target` within
/// canonical floating-point tolerance.
pub fn move_rotate_preview_between(
    current: RigidComponentPose,
    target: RigidComponentPose,
    world_pivot: Point3,
) -> Result<MoveRotatePreview, AssemblyPlacementError> {
    validate_point(world_pivot)?;
    let current_rotation = quaternion_from_pose(current);
    let target_rotation = quaternion_from_pose(target);
    let delta_rotation =
        quaternion_multiply(target_rotation, quaternion_conjugate(current_rotation));
    let pivot = [world_pivot.x, world_pivot.y, world_pivot.z];
    let current_translation = translation_from_pose(current).to_array();
    let target_translation = translation_from_pose(target).to_array();
    let rotated_pivot = quaternion_rotate(delta_rotation, pivot);
    let rotated_translation = quaternion_rotate(delta_rotation, current_translation);
    let delta_translation = std::array::from_fn(|index| {
        target_translation[index] - pivot[index] + rotated_pivot[index] - rotated_translation[index]
    });
    let preview = MoveRotatePreview {
        translation_mm: TranslationMm3::from_array(delta_translation),
        rotation_radians: euler_from_quaternion(delta_rotation),
    };
    preview.validate()?;
    Ok(preview)
}

/// Transforms a local component AABB into a conservative world-space AABB.
pub fn component_world_bounds(
    local_bounds: Aabb3,
    pose: RigidComponentPose,
) -> Result<Aabb3, AssemblyPlacementError> {
    validate_solid_bounds(local_bounds)?;
    let rotation = quaternion_from_pose(pose);
    let translation = translation_from_pose(pose).to_array();
    let corners: [[f64; 3]; 8] = [
        [local_bounds.min.x, local_bounds.min.y, local_bounds.min.z],
        [local_bounds.min.x, local_bounds.min.y, local_bounds.max.z],
        [local_bounds.min.x, local_bounds.max.y, local_bounds.min.z],
        [local_bounds.min.x, local_bounds.max.y, local_bounds.max.z],
        [local_bounds.max.x, local_bounds.min.y, local_bounds.min.z],
        [local_bounds.max.x, local_bounds.min.y, local_bounds.max.z],
        [local_bounds.max.x, local_bounds.max.y, local_bounds.min.z],
        [local_bounds.max.x, local_bounds.max.y, local_bounds.max.z],
    ]
    .map(|corner| {
        let rotated = quaternion_rotate(rotation, corner);
        std::array::from_fn::<_, 3, _>(|index| rotated[index] + translation[index])
    });
    if corners
        .iter()
        .flatten()
        .copied()
        .any(|value| !value.is_finite())
    {
        return Err(AssemblyPlacementError::InvalidBounds);
    }
    let min: [f64; 3] = std::array::from_fn(|axis| {
        corners
            .iter()
            .map(|corner| corner[axis])
            .fold(f64::INFINITY, f64::min)
    });
    let max: [f64; 3] = std::array::from_fn(|axis| {
        corners
            .iter()
            .map(|corner| corner[axis])
            .fold(f64::NEG_INFINITY, f64::max)
    });
    Ok(Aabb3::new(
        Point3::new(min[0], min[1], min[2]),
        Point3::new(max[0], max[1], max[2]),
    ))
}

/// Structured rejection for assembly pose authoring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssemblyPlacementError {
    InvalidBounds,
    InvalidClearance,
    InvalidTranslation,
    InvalidRotation,
    ComponentScaleUnsupported,
    GroundedComponent,
}

impl fmt::Display for AssemblyPlacementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidBounds => {
                "component bounds must be finite, ordered, and three-dimensional"
            }
            Self::InvalidClearance => {
                "component placement clearance must be finite and non-negative"
            }
            Self::InvalidTranslation => {
                "component translation is invalid or outside the model range"
            }
            Self::InvalidRotation => "component rotation must be finite",
            Self::ComponentScaleUnsupported => {
                "component occurrences support move and rotate, not scale"
            }
            Self::GroundedComponent => "grounded components cannot be moved or rotated",
        })
    }
}

impl std::error::Error for AssemblyPlacementError {}

fn rigid_pose(
    translation: TranslationMm3,
    quaternion: [f64; 4],
) -> Result<RigidComponentPose, AssemblyPlacementError> {
    let translation = ComponentTranslation::new(translation.x, translation.y, translation.z)
        .map_err(|_| AssemblyPlacementError::InvalidTranslation)?;
    let rotation =
        CanonicalQuaternion::new(quaternion[0], quaternion[1], quaternion[2], quaternion[3])
            .map_err(|_| AssemblyPlacementError::InvalidRotation)?;
    Ok(RigidComponentPose::new(translation, rotation))
}

fn reject_grounded_change(
    current: RigidComponentPose,
    candidate: RigidComponentPose,
    grounded: bool,
) -> Result<RigidComponentPose, AssemblyPlacementError> {
    if grounded && !poses_equivalent(current, candidate) {
        Err(AssemblyPlacementError::GroundedComponent)
    } else if grounded {
        Ok(current)
    } else {
        Ok(candidate)
    }
}

fn poses_equivalent(left: RigidComponentPose, right: RigidComponentPose) -> bool {
    let left_translation = translation_from_pose(left).to_array();
    let right_translation = translation_from_pose(right).to_array();
    let translations_match = left_translation
        .into_iter()
        .zip(right_translation)
        .all(|(left, right)| (left - right).abs() <= POSE_EPSILON);
    let left_rotation = quaternion_from_pose(left);
    let right_rotation = quaternion_from_pose(right);
    let dot = left_rotation
        .into_iter()
        .zip(right_rotation)
        .map(|(left, right)| left * right)
        .sum::<f64>()
        .abs();
    translations_match && (1.0 - dot).abs() <= POSE_EPSILON
}

fn translation_from_pose(pose: RigidComponentPose) -> TranslationMm3 {
    TranslationMm3::new(
        pose.translation.x(),
        pose.translation.y(),
        pose.translation.z(),
    )
}

fn quaternion_from_pose(pose: RigidComponentPose) -> [f64; 4] {
    [
        pose.rotation.w(),
        pose.rotation.x(),
        pose.rotation.y(),
        pose.rotation.z(),
    ]
}

fn quaternion_from_euler(rotation: RotationRadians3) -> [f64; 4] {
    let rotation = rotation.normalized();
    let (sin_x, cos_x) = (rotation.x * 0.5).sin_cos();
    let (sin_y, cos_y) = (rotation.y * 0.5).sin_cos();
    let (sin_z, cos_z) = (rotation.z * 0.5).sin_cos();
    // Fixed-axis X then Y then Z: qz * qy * qx.
    [
        cos_z * cos_y * cos_x + sin_z * sin_y * sin_x,
        cos_z * cos_y * sin_x - sin_z * sin_y * cos_x,
        cos_z * sin_y * cos_x + sin_z * cos_y * sin_x,
        sin_z * cos_y * cos_x - cos_z * sin_y * sin_x,
    ]
}

fn euler_from_quaternion(quaternion: [f64; 4]) -> RotationRadians3 {
    let [w, x, y, z] = normalize_quaternion(quaternion);
    let sin_y = (2.0 * (w * y - z * x)).clamp(-1.0, 1.0);
    let y_angle = sin_y.asin();
    let (x_angle, z_angle) = if y_angle.cos().abs() > POSE_EPSILON {
        (
            (2.0 * (w * x + y * z)).atan2(1.0 - 2.0 * (x * x + y * y)),
            (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z)),
        )
    } else {
        // At gimbal lock only a combined X/Z angle is observable. Canonicalize
        // X to zero and retain the equivalent Z orientation.
        (
            0.0,
            (-2.0 * (x * y - w * z)).atan2(1.0 - 2.0 * (x * x + z * z)),
        )
    };
    RotationRadians3::new(x_angle, y_angle, z_angle).normalized()
}

fn quaternion_multiply(left: [f64; 4], right: [f64; 4]) -> [f64; 4] {
    let [lw, lx, ly, lz] = left;
    let [rw, rx, ry, rz] = right;
    normalize_quaternion([
        lw * rw - lx * rx - ly * ry - lz * rz,
        lw * rx + lx * rw + ly * rz - lz * ry,
        lw * ry - lx * rz + ly * rw + lz * rx,
        lw * rz + lx * ry - ly * rx + lz * rw,
    ])
}

fn quaternion_conjugate(quaternion: [f64; 4]) -> [f64; 4] {
    let [w, x, y, z] = normalize_quaternion(quaternion);
    [w, -x, -y, -z]
}

fn normalize_quaternion(quaternion: [f64; 4]) -> [f64; 4] {
    let norm = quaternion
        .into_iter()
        .fold(0.0_f64, |sum, value| value.mul_add(value, sum))
        .sqrt();
    if !norm.is_finite() || norm <= f64::EPSILON {
        return [1.0, 0.0, 0.0, 0.0];
    }
    quaternion.map(|value| value / norm)
}

fn quaternion_rotate(quaternion: [f64; 4], vector: [f64; 3]) -> [f64; 3] {
    let [w, x, y, z] = normalize_quaternion(quaternion);
    let twice_cross = [
        2.0 * (y * vector[2] - z * vector[1]),
        2.0 * (z * vector[0] - x * vector[2]),
        2.0 * (x * vector[1] - y * vector[0]),
    ];
    let second_cross = [
        y * twice_cross[2] - z * twice_cross[1],
        z * twice_cross[0] - x * twice_cross[2],
        x * twice_cross[1] - y * twice_cross[0],
    ];
    std::array::from_fn(|index| vector[index] + w * twice_cross[index] + second_cross[index])
}

fn validate_solid_bounds(bounds: Aabb3) -> Result<(), AssemblyPlacementError> {
    let minimum = [bounds.min.x, bounds.min.y, bounds.min.z];
    let maximum = [bounds.max.x, bounds.max.y, bounds.max.z];
    if minimum
        .into_iter()
        .chain(maximum)
        .any(|value| !value.is_finite())
        || minimum
            .into_iter()
            .zip(maximum)
            .any(|(minimum, maximum)| minimum >= maximum)
    {
        Err(AssemblyPlacementError::InvalidBounds)
    } else {
        Ok(())
    }
}

fn validate_point(point: Point3) -> Result<(), AssemblyPlacementError> {
    if [point.x, point.y, point.z].into_iter().all(f64::is_finite) {
        Ok(())
    } else {
        Err(AssemblyPlacementError::InvalidTranslation)
    }
}

fn normalize_radians(angle: f64) -> f64 {
    (angle + PI).rem_euclid(TAU) - PI
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(min: [f64; 3], max: [f64; 3]) -> Aabb3 {
        Aabb3::new(
            Point3::new(min[0], min[1], min[2]),
            Point3::new(max[0], max[1], max[2]),
        )
    }

    fn assert_near(left: f64, right: f64) {
        assert!((left - right).abs() <= 1.0e-9, "{left} != {right}");
    }

    fn assert_pose_near(left: RigidComponentPose, right: RigidComponentPose) {
        for (left, right) in translation_from_pose(left)
            .to_array()
            .into_iter()
            .zip(translation_from_pose(right).to_array())
        {
            assert_near(left, right);
        }
        let dot = quaternion_from_pose(left)
            .into_iter()
            .zip(quaternion_from_pose(right))
            .map(|(left, right)| left * right)
            .sum::<f64>()
            .abs();
        assert_near(dot, 1.0);
    }

    #[test]
    fn first_component_is_laid_on_the_world_origin_planes() {
        let pose = deterministic_initial_pose(
            bounds([-10.0, -2.0, 4.0], [10.0, 18.0, 459.0]),
            &[],
            DEFAULT_COMPONENT_INSERTION_CLEARANCE_MM,
        )
        .expect("first placement should succeed");
        assert_eq!(
            translation_from_pose(pose),
            TranslationMm3::new(10.0, 2.0, -4.0)
        );
        assert_eq!(pose.rotation, CanonicalQuaternion::identity());
    }

    #[test]
    fn repeated_equal_components_are_deterministic_and_non_overlapping() {
        let local = bounds([0.0, 0.0, 0.0], [20.0, 20.0, 455.0]);
        let mut occupied = Vec::new();
        let mut minimum_x = Vec::new();
        for _ in 0..3 {
            let pose = deterministic_initial_pose(local, &occupied, 10.0)
                .expect("placement should succeed");
            let world = component_world_bounds(local, pose).expect("bounds should transform");
            minimum_x.push(world.min.x);
            occupied.push(world);
        }
        assert_eq!(minimum_x, vec![0.0, 30.0, 60.0]);
        for pair in occupied.windows(2) {
            assert_near(pair[1].min.x - pair[0].max.x, 10.0);
        }

        occupied.reverse();
        let fourth = deterministic_initial_pose(local, &occupied, 10.0)
            .expect("input order must not matter");
        assert_near(translation_from_pose(fourth).x, 90.0);
    }

    #[test]
    fn invalid_bounds_clearance_and_overflow_fail_closed() {
        let valid = bounds([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        assert_eq!(
            deterministic_initial_pose(bounds([0.0, 0.0, 0.0], [0.0, 1.0, 1.0]), &[], 1.0,),
            Err(AssemblyPlacementError::InvalidBounds)
        );
        assert_eq!(
            deterministic_initial_pose(valid, &[], -1.0),
            Err(AssemblyPlacementError::InvalidClearance)
        );
        assert_eq!(
            deterministic_initial_pose(
                valid,
                &[bounds([0.0, 0.0, 0.0], [f64::MAX, 1.0, 1.0])],
                f64::MAX,
            ),
            Err(AssemblyPlacementError::InvalidTranslation)
        );
    }

    #[test]
    fn typed_absolute_fields_round_trip_a_rigid_pose() {
        let fields = ComponentPoseFields {
            translation_mm: TranslationMm3::new(10.0, -20.0, 30.0),
            rotation_degrees: RotationDegrees3::new(25.0, -35.0, 70.0),
        };
        let pose = fields.to_pose().expect("fields should create a pose");
        let decoded = ComponentPoseFields::from_pose(pose);
        assert_near(decoded.translation_mm.x, 10.0);
        assert_near(decoded.rotation_degrees.x, 25.0);
        assert_near(decoded.rotation_degrees.y, -35.0);
        assert_near(decoded.rotation_degrees.z, 70.0);
        assert_pose_near(decoded.to_pose().unwrap(), pose);
    }

    #[test]
    fn gimbal_lock_uses_a_stable_equivalent_field_representation() {
        let fields = ComponentPoseFields {
            translation_mm: TranslationMm3::ZERO,
            rotation_degrees: RotationDegrees3::new(35.0, 90.0, -20.0),
        };
        let pose = fields.to_pose().expect("gimbal pose should be valid");
        let canonical = ComponentPoseFields::from_pose(pose);
        assert_near(canonical.rotation_degrees.x, 0.0);
        assert_pose_near(canonical.to_pose().unwrap(), pose);
    }

    #[test]
    fn preview_composition_matches_rotation_about_world_pivot() {
        let current = ComponentPoseFields {
            translation_mm: TranslationMm3::new(10.0, 0.0, 0.0),
            rotation_degrees: RotationDegrees3::ZERO,
        }
        .to_pose()
        .unwrap();
        let preview = MoveRotatePreview::from_display_parts(
            [5.0, 0.0, 0.0],
            [0.0, 0.0, 90.0_f64.to_radians()],
            1.0,
        )
        .unwrap();
        let result =
            compose_move_rotate_preview(current, preview, Point3::new(0.0, 0.0, 0.0), false)
                .expect("preview should compose");
        let translation = translation_from_pose(result);
        assert_near(translation.x, 5.0);
        assert_near(translation.y, 10.0);
        assert_near(translation.z, 0.0);
        let fields = ComponentPoseFields::from_pose(result);
        assert_near(fields.rotation_degrees.z, 90.0);
    }

    #[test]
    fn preview_between_is_the_inverse_of_preview_composition() {
        let current = ComponentPoseFields {
            translation_mm: TranslationMm3::new(4.0, -3.0, 2.0),
            rotation_degrees: RotationDegrees3::new(10.0, 20.0, 30.0),
        }
        .to_pose()
        .unwrap();
        let target = ComponentPoseFields {
            translation_mm: TranslationMm3::new(-8.0, 14.0, 5.0),
            rotation_degrees: RotationDegrees3::new(-25.0, 40.0, 80.0),
        }
        .to_pose()
        .unwrap();
        let pivot = Point3::new(3.0, 7.0, -2.0);
        let preview =
            move_rotate_preview_between(current, target, pivot).expect("preview should derive");
        let recomposed = compose_move_rotate_preview(current, preview, pivot, false)
            .expect("derived preview should compose");
        assert_pose_near(recomposed, target);
    }

    #[test]
    fn grounded_components_allow_no_op_but_reject_move_rotate_and_field_edits() {
        let current = RigidComponentPose::identity();
        assert_eq!(
            compose_move_rotate_preview(
                current,
                MoveRotatePreview::identity(),
                Point3::new(0.0, 0.0, 0.0),
                true,
            ),
            Ok(current)
        );
        assert_eq!(
            compose_move_rotate_preview(
                current,
                MoveRotatePreview::from_display_parts([1.0, 0.0, 0.0], [0.0; 3], 1.0).unwrap(),
                Point3::new(0.0, 0.0, 0.0),
                true,
            ),
            Err(AssemblyPlacementError::GroundedComponent)
        );
        let unchanged = ComponentPoseFields::from_pose(current);
        assert_eq!(unchanged.commit(current, true), Ok(current));
        let changed = ComponentPoseFields {
            translation_mm: TranslationMm3::new(0.0, 0.0, 1.0),
            ..unchanged
        };
        assert_eq!(
            changed.commit(current, true),
            Err(AssemblyPlacementError::GroundedComponent)
        );
    }

    #[test]
    fn preview_bridge_rejects_scale_and_nonfinite_values() {
        assert_eq!(
            MoveRotatePreview::from_display_parts([0.0; 3], [0.0; 3], 2.0),
            Err(AssemblyPlacementError::ComponentScaleUnsupported)
        );
        assert_eq!(
            MoveRotatePreview::from_display_parts([f64::NAN, 0.0, 0.0], [0.0; 3], 1.0),
            Err(AssemblyPlacementError::InvalidTranslation)
        );
        assert_eq!(
            MoveRotatePreview::from_display_parts([0.0; 3], [0.0, f64::INFINITY, 0.0], 1.0),
            Err(AssemblyPlacementError::InvalidRotation)
        );
    }

    #[test]
    fn rotated_component_world_bounds_are_conservative() {
        let local = bounds([0.0, 0.0, 0.0], [2.0, 4.0, 6.0]);
        let pose = ComponentPoseFields {
            translation_mm: TranslationMm3::new(10.0, 20.0, 30.0),
            rotation_degrees: RotationDegrees3::new(0.0, 0.0, 90.0),
        }
        .to_pose()
        .unwrap();
        let world = component_world_bounds(local, pose).expect("world bounds should resolve");
        assert_near(world.min.x, 6.0);
        assert_near(world.max.x, 10.0);
        assert_near(world.min.y, 20.0);
        assert_near(world.max.y, 22.0);
        assert_near(world.min.z, 30.0);
        assert_near(world.max.z, 36.0);
    }
}
