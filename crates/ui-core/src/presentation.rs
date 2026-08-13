//! Dependency-free presentation state and transform math for the kernel lab.
//!
//! This module deliberately knows nothing about egui or the renderer. Keeping
//! interaction state here makes transforms and animation deterministic and
//! inexpensive to test without opening a window.

use std::f64::consts::{PI, TAU};

use artificer_protocol::{
    Aabb3, PlanarFrame3, Point3, RotationQuaternion, SimilarityTransform3,
    Vector3 as ProtocolVector3,
};

const MIN_DISPLAY_SCALE: f64 = 0.01;
const MAX_DISPLAY_SCALE: f64 = 100.0;
const MIN_VIEW_ZOOM: f64 = 0.02;
const MAX_VIEW_ZOOM: f64 = 200.0;
const MAX_FRAME_DELTA_SECONDS: f64 = 0.25;
const MAX_ABS_SPEED_RPM: f64 = 120.0;
const FPS_SMOOTHING_WEIGHT: f64 = 0.1;
pub const FACE_CAMERA_TRANSITION_SECONDS: f64 = 0.34;
const ORIENTATION_EPSILON: f64 = 1.0e-10;
const END_ON_AXIS_RATIO: f64 = 0.08;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActiveTool {
    #[default]
    Select,
    Measure,
    Orbit,
    Move,
    Rotate,
    Scale,
}

/// Named orthographic faces exposed by the model-view cube.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StandardView {
    Front,
    Back,
    Left,
    Right,
    Top,
    Bottom,
}

impl StandardView {
    pub const ALL: [Self; 6] = [
        Self::Front,
        Self::Back,
        Self::Left,
        Self::Right,
        Self::Top,
        Self::Bottom,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Front => "FRONT",
            Self::Back => "BACK",
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
            Self::Top => "TOP",
            Self::Bottom => "BOTTOM",
        }
    }

    pub const fn outward_normal(self) -> ProtocolVector3 {
        match self {
            Self::Front => ProtocolVector3::new(0.0, -1.0, 0.0),
            Self::Back => ProtocolVector3::new(0.0, 1.0, 0.0),
            Self::Left => ProtocolVector3::new(-1.0, 0.0, 0.0),
            Self::Right => ProtocolVector3::new(1.0, 0.0, 0.0),
            Self::Top => ProtocolVector3::new(0.0, 0.0, 1.0),
            Self::Bottom => ProtocolVector3::new(0.0, 0.0, -1.0),
        }
    }

    const fn preferred_up(self) -> ProtocolVector3 {
        match self {
            Self::Top => ProtocolVector3::new(0.0, 1.0, 0.0),
            Self::Bottom => ProtocolVector3::new(0.0, -1.0, 0.0),
            Self::Front | Self::Back | Self::Left | Self::Right => {
                ProtocolVector3::new(0.0, 0.0, 1.0)
            }
        }
    }
}

impl ActiveTool {
    pub const ALL: [Self; 6] = [
        Self::Select,
        Self::Measure,
        Self::Orbit,
        Self::Move,
        Self::Rotate,
        Self::Scale,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Measure => "Measure",
            Self::Orbit => "Orbit",
            Self::Move => "Move",
            Self::Rotate => "Rotate",
            Self::Scale => "Scale",
        }
    }

    pub const fn shortcut(self) -> &'static str {
        match self {
            Self::Select => "V",
            Self::Measure => "I",
            Self::Orbit => "O",
            Self::Move => "M",
            Self::Rotate => "R",
            Self::Scale => "S",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayTransform {
    pub translation: [f64; 3],
    /// Fixed-axis X, then Y, then Z Euler rotations in radians.
    pub rotation: [f64; 3],
    pub scale: f64,
}

impl Default for DisplayTransform {
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            rotation: [0.0; 3],
            scale: 1.0,
        }
    }
}

impl DisplayTransform {
    /// Display-scale bounds, asserted on by the shell crate's tests.
    pub const MIN_SCALE: f64 = MIN_DISPLAY_SCALE;
    pub const MAX_SCALE: f64 = MAX_DISPLAY_SCALE;

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn is_identity(self) -> bool {
        self.translation == [0.0; 3] && self.rotation == [0.0; 3] && self.scale == 1.0
    }

    pub fn set_scale(&mut self, scale: f64) {
        self.scale = bounded_scale(scale);
    }

    pub fn scale_by(&mut self, factor: f64) {
        if factor.is_finite() && factor > 0.0 {
            self.set_scale(bounded_scale(self.scale) * factor);
        }
    }

    pub fn translate_by(&mut self, delta: [f64; 3]) {
        for (value, delta) in self.translation.iter_mut().zip(delta) {
            if delta.is_finite() {
                *value += delta;
            }
        }
    }

    pub fn rotate_by(&mut self, delta_radians: [f64; 3]) {
        for (angle, delta) in self.rotation.iter_mut().zip(delta_radians) {
            if delta.is_finite() {
                *angle = normalize_angle(*angle + delta);
            }
        }
    }

    /// Applies scale and Euler rotation around the supplied model bounds'
    /// center, then applies world-space translation.
    #[cfg(test)]
    pub fn transform_point(self, point: Point3, bounds: Aabb3) -> Point3 {
        self.transform_point_about(point, bounds_center(bounds))
    }

    pub fn transform_point_about(self, point: Point3, pivot: Point3) -> Point3 {
        let scale = bounded_scale(self.scale);
        let mut relative = [
            (point.x - pivot.x) * scale,
            (point.y - pivot.y) * scale,
            (point.z - pivot.z) * scale,
        ];

        relative = rotate_euler(relative, self.rotation);

        Point3::new(
            pivot.x + relative[0] + finite_or_zero(self.translation[0]),
            pivot.y + relative[1] + finite_or_zero(self.translation[1]),
            pivot.z + relative[2] + finite_or_zero(self.translation[2]),
        )
    }

    /// The direction counterpart of [`Self::present_point`]: preview rotation
    /// first, turntable motion second, with translation and the positive
    /// uniform scale dropped because neither turns a direction.
    pub fn present_direction(self, direction: ProtocolVector3, phase: f64) -> [f64; 3] {
        let previewed = rotate_euler([direction.x, direction.y, direction.z], self.rotation);
        rotate_z(previewed, finite_or_zero(phase))
    }

    /// Applies preview first and turntable motion second. Motion is never
    /// folded into a committable transform.
    pub fn present_point(self, point: Point3, pivot: Point3, phase: f64) -> Point3 {
        let previewed = self.transform_point_about(point, pivot);
        let motion_pivot = Point3::new(
            pivot.x + finite_or_zero(self.translation[0]),
            pivot.y + finite_or_zero(self.translation[1]),
            pivot.z + finite_or_zero(self.translation[2]),
        );
        let relative = [
            previewed.x - motion_pivot.x,
            previewed.y - motion_pivot.y,
            previewed.z - motion_pivot.z,
        ];
        let rotated = rotate_z(relative, finite_or_zero(phase));
        Point3::new(
            motion_pivot.x + rotated[0],
            motion_pivot.y + rotated[1],
            motion_pivot.z + rotated[2],
        )
    }

    /// Converts the pivoted fixed-axis UI preview into the kernel's canonical
    /// world-origin similarity: `p' = sR(p) + t`.
    pub fn kernel_similarity(self, pivot: Point3) -> SimilarityTransform3 {
        let scale = bounded_scale(self.scale);
        let rotated_pivot = rotate_euler([pivot.x, pivot.y, pivot.z], self.rotation);
        SimilarityTransform3 {
            translation: ProtocolVector3::new(
                pivot.x + finite_or_zero(self.translation[0]) - rotated_pivot[0] * scale,
                pivot.y + finite_or_zero(self.translation[1]) - rotated_pivot[1] * scale,
                pivot.z + finite_or_zero(self.translation[2]) - rotated_pivot[2] * scale,
            ),
            rotation: euler_quaternion(self.rotation),
            uniform_scale: scale,
        }
    }

    pub fn transformed_bounds(self, bounds: Aabb3, pivot: Point3) -> Aabb3 {
        let corners = [
            Point3::new(bounds.min.x, bounds.min.y, bounds.min.z),
            Point3::new(bounds.min.x, bounds.min.y, bounds.max.z),
            Point3::new(bounds.min.x, bounds.max.y, bounds.min.z),
            Point3::new(bounds.min.x, bounds.max.y, bounds.max.z),
            Point3::new(bounds.max.x, bounds.min.y, bounds.min.z),
            Point3::new(bounds.max.x, bounds.min.y, bounds.max.z),
            Point3::new(bounds.max.x, bounds.max.y, bounds.min.z),
            Point3::new(bounds.max.x, bounds.max.y, bounds.max.z),
        ]
        .map(|point| self.transform_point_about(point, pivot));
        let mut min = corners[0];
        let mut max = corners[0];
        for point in corners.into_iter().skip(1) {
            min.x = min.x.min(point.x);
            min.y = min.y.min(point.y);
            min.z = min.z.min(point.z);
            max.x = max.x.max(point.x);
            max.y = max.y.max(point.y);
            max.z = max.z.max(point.z);
        }
        Aabb3::new(min, max)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraProjection {
    pub coordinates: [f64; 2],
    pub depth: f64,
}

/// Camera-side classification for a presentation-only linear handle.
///
/// Positive camera depth is the side nearest the viewer in the orthographic
/// viewport. Keeping this explicit prevents an end-on extrusion handle from
/// silently reversing its drag convention as the camera crosses the profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AxisCameraFacing {
    TowardCamera,
    AwayFromCamera,
    EdgeOn,
}

/// Deterministic screen-to-model projection for one signed linear drag.
///
/// `screen_axis_points_per_unit` is the exact projected displacement produced
/// by one positive model unit. When that axis is nearly end-on, vertical screen
/// motion is used with the full camera-space scale. This keeps the handle
/// draggable from front, rear, and edge-on views without publishing any UI
/// state into the geometry kernel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SignedDistanceDragProjection {
    screen_axis_points_per_unit: [f64; 2],
    fallback_points_per_unit: f64,
    facing: AxisCameraFacing,
    use_projected_axis: bool,
}

impl SignedDistanceDragProjection {
    pub fn new(
        screen_axis_points_per_unit: [f64; 2],
        camera_depth_per_unit: f64,
        fallback_points_per_unit: f64,
    ) -> Option<Self> {
        if screen_axis_points_per_unit
            .iter()
            .any(|component| !component.is_finite())
            || !camera_depth_per_unit.is_finite()
            || !fallback_points_per_unit.is_finite()
            || fallback_points_per_unit <= f64::EPSILON
        {
            return None;
        }
        let screen_length = screen_axis_points_per_unit[0].hypot(screen_axis_points_per_unit[1]);
        let facing = if camera_depth_per_unit > ORIENTATION_EPSILON {
            AxisCameraFacing::TowardCamera
        } else if camera_depth_per_unit < -ORIENTATION_EPSILON {
            AxisCameraFacing::AwayFromCamera
        } else {
            AxisCameraFacing::EdgeOn
        };
        Some(Self {
            screen_axis_points_per_unit,
            fallback_points_per_unit,
            facing,
            use_projected_axis: screen_length >= fallback_points_per_unit * END_ON_AXIS_RATIO,
        })
    }

    /// Converts one frame's pointer delta into a signed model-space distance.
    /// Positive values always follow the preview's authored direction.
    pub fn signed_distance_delta(self, screen_delta: [f64; 2]) -> f64 {
        if screen_delta.iter().any(|component| !component.is_finite()) {
            return 0.0;
        }
        if self.use_projected_axis {
            let squared_length = self.screen_axis_points_per_unit[0].mul_add(
                self.screen_axis_points_per_unit[0],
                self.screen_axis_points_per_unit[1] * self.screen_axis_points_per_unit[1],
            );
            if squared_length > f64::EPSILON {
                return screen_delta[0].mul_add(
                    self.screen_axis_points_per_unit[0],
                    screen_delta[1] * self.screen_axis_points_per_unit[1],
                ) / squared_length;
            }
        }
        // An orthographic end-on axis has no unique 2D projection. CAD
        // convention maps upward motion to increasing signed distance.
        -screen_delta[1] / self.fallback_points_per_unit
    }

    pub const fn facing(self) -> AxisCameraFacing {
        self.facing
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewState {
    pub yaw: f64,
    pub pitch: f64,
    pub roll: f64,
    pub zoom: f64,
    target: Point3,
    fit_radius: f64,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            yaw: -PI / 4.0,
            pitch: PI / 6.0,
            roll: 0.0,
            zoom: 1.0,
            target: Point3::default(),
            fit_radius: 1.0,
        }
    }
}

impl ViewState {
    pub fn reset_orientation(&mut self) {
        let target = self.target;
        let fit_radius = self.fit_radius;
        *self = Self {
            target,
            fit_radius,
            ..Self::default()
        };
    }

    pub fn frame(&mut self, bounds: Aabb3) {
        let radius = bounds_radius(bounds);
        if bounds.is_finite() && bounds.is_ordered() && radius.is_finite() && radius > f64::EPSILON
        {
            self.target = bounds_center(bounds);
            self.fit_radius = radius;
        }
    }

    pub const fn target(self) -> Point3 {
        self.target
    }

    /// Changes only the point the camera orbits and projects around. Keeping
    /// zoom and orientation intact lets a navigation gesture recover the
    /// document centre after a face-focused view without reframing the scene.
    /// Slides the camera target across the view plane, which is what every
    /// package's pan gesture does.
    pub fn pan_by(&mut self, horizontal: f64, vertical: f64) {
        let delta = self.world_delta_from_screen(horizontal, vertical);
        let target = self.target();
        self.set_target(Point3::new(
            target.x - delta[0],
            target.y - delta[1],
            target.z - delta[2],
        ));
    }

    pub fn set_target(&mut self, target: Point3) {
        if target.is_finite() {
            self.target = target;
        }
    }

    pub const fn fit_radius(self) -> f64 {
        self.fit_radius
    }

    pub fn orbit(&mut self, yaw_delta: f64, pitch_delta: f64) {
        let yaw_delta = finite_or_zero(yaw_delta);
        let pitch_delta = finite_or_zero(pitch_delta);
        if yaw_delta == 0.0 && pitch_delta == 0.0 {
            return;
        }
        // Compose around the camera's current screen-up and screen-right axes.
        // Adding world-Z yaw after a face view feels inverted whenever that
        // view carries roll; local composition keeps mouse directions stable.
        let orientation = view_orientation_quaternion(*self);
        let local_yaw = axis_angle_quaternion([0.0, 0.0, 1.0], yaw_delta);
        let local_pitch = axis_angle_quaternion([1.0, 0.0, 0.0], pitch_delta);
        let next = quaternion_multiply(orientation, quaternion_multiply(local_yaw, local_pitch));
        if let Some([yaw, pitch, roll]) = camera_euler_from_quaternion(next) {
            self.yaw = yaw;
            self.pitch = pitch;
            self.roll = roll;
        }
    }

    /// Snaps orientation to one view-cube face without changing framing.
    pub fn set_standard_view(&mut self, face: StandardView) {
        let depth = face.outward_normal();
        let up = face.preferred_up();
        let right = protocol_cross(depth, up);
        if let Some([yaw, pitch, roll]) = camera_euler_from_axes(right, depth, up) {
            self.yaw = yaw;
            self.pitch = pitch;
            self.roll = roll;
        }
    }

    /// Rolls the current face view in screen space. This is the operation used
    /// by the curved arrows around the view cube.
    pub fn rotate_in_plane_quarter_turn(&mut self, clockwise: bool) {
        let angle = if clockwise { -PI * 0.5 } else { PI * 0.5 };
        let next = quaternion_multiply(
            view_orientation_quaternion(*self),
            axis_angle_quaternion([0.0, 1.0, 0.0], angle),
        );
        if let Some([yaw, pitch, roll]) = camera_euler_from_quaternion(next) {
            self.yaw = yaw;
            self.pitch = pitch;
            self.roll = roll;
        }
    }

    pub fn nearest_standard_view(self) -> StandardView {
        StandardView::ALL
            .into_iter()
            .max_by(|left, right| {
                self.project_direction(left.outward_normal())
                    .depth
                    .total_cmp(&self.project_direction(right.outward_normal()).depth)
            })
            .unwrap_or(StandardView::Front)
    }

    pub fn set_zoom(&mut self, zoom: f64) {
        self.zoom = bounded_zoom(zoom);
    }

    pub fn zoom_by(&mut self, factor: f64) {
        if factor.is_finite() && factor > 0.0 {
            self.set_zoom(bounded_zoom(self.zoom) * factor);
        }
    }

    /// Converts an orthographic screen-plane delta into a world-space vector.
    /// Horizontal is positive screen-right and vertical is positive down.
    pub fn world_delta_from_screen(self, horizontal: f64, vertical: f64) -> [f64; 3] {
        let camera = [finite_or_zero(horizontal), 0.0, -finite_or_zero(vertical)];
        let rolled = rotate_y(camera, finite_or_zero(self.roll));
        let yawed = rotate_x(rolled, finite_or_zero(self.pitch));
        rotate_z(yawed, finite_or_zero(self.yaw))
    }

    /// The world-space direction pointing from the model toward the viewer.
    /// Under an orthographic projection this is the whole view direction, which
    /// is what a silhouette condition `n · v = 0` needs.
    #[must_use]
    pub fn view_direction(self) -> ProtocolVector3 {
        camera_world_axes(self).1
    }

    /// Projects a world-space direction through only the camera orientation.
    ///
    /// Unlike [`Self::project`], this intentionally excludes target and zoom.
    /// Screen-space orientation widgets can therefore use one common scale for
    /// every axis and retain true orthographic foreshortening.
    pub fn project_direction(self, direction: ProtocolVector3) -> CameraProjection {
        let relative = [direction.x, direction.y, direction.z];
        let yawed = rotate_z(relative, -finite_or_zero(self.yaw));
        let pitched = rotate_x(yawed, -finite_or_zero(self.pitch));
        let camera = rotate_y(pitched, -finite_or_zero(self.roll));

        CameraProjection {
            coordinates: [camera[0], -camera[2]],
            depth: camera[1],
        }
    }

    /// Orthographically projects a world-space point around `bounds`.
    ///
    /// Camera X maps to horizontal screen space, negative Z maps upward, and
    /// positive camera Y is retained as depth toward the viewer.
    pub fn project(self, point: Point3) -> CameraProjection {
        let relative = [
            point.x - self.target.x,
            point.y - self.target.y,
            point.z - self.target.z,
        ];
        let direction =
            self.project_direction(ProtocolVector3::new(relative[0], relative[1], relative[2]));
        let zoom = bounded_zoom(self.zoom);

        CameraProjection {
            coordinates: [
                direction.coordinates[0] * zoom,
                direction.coordinates[1] * zoom,
            ],
            depth: direction.depth,
        }
    }

    pub fn project_transformed(
        self,
        point: Point3,
        pivot: Point3,
        transform: DisplayTransform,
        animation_phase: f64,
    ) -> CameraProjection {
        self.project(transform.present_point(point, pivot, animation_phase))
    }

    /// Returns the canonical sketch-plane quarter turn that matches the
    /// nearest face-aligned camera roll. Zero keeps U right and V up; positive
    /// turns rotate that canonical canvas counter-clockwise on screen.
    pub fn face_sketch_quarter_turn(self, frame: PlanarFrame3) -> Option<u8> {
        let frame_u = normalized_protocol_vector(frame.u)?;
        let normal = normalized_protocol_vector(protocol_cross(frame_u, frame.v))?;
        let frame_v = normalized_protocol_vector(protocol_cross(normal, frame_u))?;
        let current_up = camera_world_axes(self).2;
        let u_score = protocol_dot(current_up, frame_u);
        let v_score = protocol_dot(current_up, frame_v);
        Some(if u_score.abs() > v_score.abs() {
            if u_score.is_sign_negative() { 3 } else { 1 }
        } else if v_score.is_sign_negative() {
            2
        } else {
            0
        })
    }

    pub fn face_aligned_target(
        self,
        frame: PlanarFrame3,
        focus: Point3,
        fit_radius: f64,
    ) -> Option<Self> {
        if !focus.is_finite() || !fit_radius.is_finite() || fit_radius <= f64::EPSILON {
            return None;
        }
        let frame_u = normalized_protocol_vector(frame.u)?;
        let normal = normalized_protocol_vector(protocol_cross(frame_u, frame.v))?;
        let frame_v = normalized_protocol_vector(protocol_cross(normal, frame_u))?;
        let current_up = camera_world_axes(self).2;
        let (mut up, score) =
            if protocol_dot(current_up, frame_u).abs() > protocol_dot(current_up, frame_v).abs() {
                (frame_u, protocol_dot(current_up, frame_u))
            } else {
                (frame_v, protocol_dot(current_up, frame_v))
            };
        if score.is_sign_negative() {
            up = protocol_scale(up, -1.0);
        }
        // Keep the selected outward normal facing the viewer, but choose the
        // quarter-turn whose in-plane up axis is nearest the current camera.
        // A mostly upright part therefore stays upright and a sideways part
        // stays sideways when entering its face sketch.
        let right = normalized_protocol_vector(protocol_cross(normal, up))?;
        let depth = normal;
        let [yaw, pitch, roll] = camera_euler_from_axes(right, depth, up)?;
        Some(Self {
            yaw,
            pitch,
            roll,
            zoom: 1.0,
            target: focus,
            fit_radius,
        })
    }
}

/// Deterministic, presentation-only move from the current model camera to a
/// selected face's exact local frame.
///
/// Orientation uses shortest-path quaternion interpolation while target,
/// framing radius, and logarithmic zoom use the same quintic ease. Sampling is
/// driven only by caller-provided deltas, so tests and 60 Hz rendering do not
/// depend on wall-clock scheduling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraTransition {
    source: ViewState,
    target: ViewState,
    source_orientation: [f64; 4],
    target_orientation: [f64; 4],
    elapsed_seconds: f64,
    duration_seconds: f64,
}

impl CameraTransition {
    pub fn face_aligned(
        source: ViewState,
        frame: PlanarFrame3,
        focus: Point3,
        fit_radius: f64,
    ) -> Option<Self> {
        let target = source.face_aligned_target(frame, focus, fit_radius)?;
        Some(Self::between(
            source,
            target,
            FACE_CAMERA_TRANSITION_SECONDS,
        ))
    }

    /// Flies back to a camera captured earlier, at the same pace as the
    /// outward flight.
    pub fn to_view(source: ViewState, target: ViewState) -> Option<Self> {
        (source != target).then(|| Self::between(source, target, FACE_CAMERA_TRANSITION_SECONDS))
    }

    fn between(source: ViewState, target: ViewState, duration_seconds: f64) -> Self {
        Self {
            source,
            target,
            source_orientation: view_orientation_quaternion(source),
            target_orientation: view_orientation_quaternion(target),
            elapsed_seconds: 0.0,
            duration_seconds: finite_positive_or(duration_seconds, 1.0),
        }
    }

    /// Advances the transition and returns the camera for this frame.
    pub fn advance(&mut self, delta_seconds: f64) -> ViewState {
        if delta_seconds.is_finite() && delta_seconds > 0.0 {
            self.elapsed_seconds =
                (self.elapsed_seconds + delta_seconds).min(self.duration_seconds);
        }
        self.current()
    }

    pub fn current(self) -> ViewState {
        let progress = (self.elapsed_seconds / self.duration_seconds).clamp(0.0, 1.0);
        if progress <= 0.0 {
            return self.source;
        }
        if progress >= 1.0 {
            return self.target;
        }
        let eased = smootherstep(progress);
        let orientation = quaternion_slerp(self.source_orientation, self.target_orientation, eased);
        let [yaw, pitch, roll] = camera_euler_from_quaternion(orientation)
            .expect("interpolation of valid unit camera orientations must remain valid");
        ViewState {
            yaw,
            pitch,
            roll,
            zoom: logarithmic_lerp(self.source.zoom, self.target.zoom, eased),
            target: lerp_point(self.source.target, self.target.target, eased),
            fit_radius: logarithmic_lerp(self.source.fit_radius, self.target.fit_radius, eased),
        }
    }

    pub fn is_complete(self) -> bool {
        self.elapsed_seconds >= self.duration_seconds
    }

    #[cfg(test)]
    fn target(self) -> ViewState {
        self.target
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionState {
    pub playing: bool,
    pub target_hz: u32,
    pub speed_rpm: f64,
    pub phase: f64,
    /// Measured repaint-start cadence. This is not GPU presentation telemetry.
    pub smoothed_fps: Option<f64>,
}

impl Default for MotionState {
    fn default() -> Self {
        Self {
            playing: false,
            target_hz: 60,
            speed_rpm: 6.0,
            phase: 0.0,
            smoothed_fps: None,
        }
    }
}

impl MotionState {
    pub fn play(&mut self) {
        if !self.playing {
            self.smoothed_fps = None;
        }
        self.playing = true;
    }

    pub fn pause(&mut self) {
        self.playing = false;
        // Motion is a transient inspection transform, never an authored pose.
        // Stopping therefore restores the exact committed model orientation.
        self.phase = 0.0;
        self.smoothed_fps = None;
    }

    pub fn toggle(&mut self) {
        if self.playing {
            self.pause();
        } else {
            self.play();
        }
    }

    #[cfg(test)]
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn set_speed_rpm(&mut self, speed_rpm: f64) {
        if speed_rpm.is_finite() {
            self.speed_rpm = speed_rpm.clamp(-MAX_ABS_SPEED_RPM, MAX_ABS_SPEED_RPM);
        }
    }

    #[cfg(test)]
    pub fn frame_interval(self) -> f64 {
        1.0 / f64::from(self.target_hz.max(1))
    }

    /// Advances animation by a bounded wall-clock delta.
    ///
    /// Non-finite and non-positive deltas are ignored. Long frames are capped
    /// so resuming a suspended window cannot cause a discontinuous jump.
    pub fn advance(&mut self, delta_seconds: f64) {
        if !delta_seconds.is_finite() || delta_seconds <= 0.0 {
            return;
        }

        let instantaneous_fps = 1.0 / delta_seconds;
        if instantaneous_fps.is_finite() {
            self.smoothed_fps = Some(self.smoothed_fps.map_or(instantaneous_fps, |previous_fps| {
                let previous_delta = 1.0 / previous_fps;
                let smoothed_delta =
                    previous_delta + (delta_seconds - previous_delta) * FPS_SMOOTHING_WEIGHT;
                1.0 / smoothed_delta
            }));
        }

        if self.playing {
            let delta_seconds = delta_seconds.min(MAX_FRAME_DELTA_SECONDS);
            let radians_per_second = self.speed_rpm * TAU / 60.0;
            self.phase = (self.phase + radians_per_second * delta_seconds).rem_euclid(TAU);
        }
    }
}

pub fn bounds_center(bounds: Aabb3) -> Point3 {
    if !bounds.is_finite() || !bounds.is_ordered() {
        return Point3::default();
    }
    Point3::new(
        bounds.min.x + (bounds.max.x - bounds.min.x) * 0.5,
        bounds.min.y + (bounds.max.y - bounds.min.y) * 0.5,
        bounds.min.z + (bounds.max.z - bounds.min.z) * 0.5,
    )
}

fn bounds_radius(bounds: Aabb3) -> f64 {
    let diagonal = [
        bounds.max.x - bounds.min.x,
        bounds.max.y - bounds.min.y,
        bounds.max.z - bounds.min.z,
    ];
    diagonal[0]
        .mul_add(
            diagonal[0],
            diagonal[1].mul_add(diagonal[1], diagonal[2] * diagonal[2]),
        )
        .sqrt()
        * 0.5
}

fn bounded_scale(scale: f64) -> f64 {
    if scale.is_finite() {
        scale.clamp(MIN_DISPLAY_SCALE, MAX_DISPLAY_SCALE)
    } else {
        1.0
    }
}

fn bounded_zoom(zoom: f64) -> f64 {
    if zoom.is_finite() {
        zoom.clamp(MIN_VIEW_ZOOM, MAX_VIEW_ZOOM)
    } else {
        1.0
    }
}

fn finite_positive_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value > f64::EPSILON {
        value
    } else {
        fallback
    }
}

fn normalized_protocol_vector(vector: ProtocolVector3) -> Option<ProtocolVector3> {
    let length_squared = vector
        .x
        .mul_add(vector.x, vector.y.mul_add(vector.y, vector.z * vector.z));
    if !length_squared.is_finite() || length_squared <= ORIENTATION_EPSILON.powi(2) {
        return None;
    }
    let inverse_length = length_squared.sqrt().recip();
    Some(protocol_scale(vector, inverse_length))
}

const fn protocol_cross(left: ProtocolVector3, right: ProtocolVector3) -> ProtocolVector3 {
    ProtocolVector3::new(
        left.y * right.z - left.z * right.y,
        left.z * right.x - left.x * right.z,
        left.x * right.y - left.y * right.x,
    )
}

const fn protocol_scale(vector: ProtocolVector3, scale: f64) -> ProtocolVector3 {
    ProtocolVector3::new(vector.x * scale, vector.y * scale, vector.z * scale)
}

const fn protocol_dot(left: ProtocolVector3, right: ProtocolVector3) -> f64 {
    left.x * right.x + left.y * right.y + left.z * right.z
}

fn camera_euler_from_axes(
    right: ProtocolVector3,
    depth: ProtocolVector3,
    up: ProtocolVector3,
) -> Option<[f64; 3]> {
    if !right.is_finite() || !depth.is_finite() || !up.is_finite() {
        return None;
    }
    let pitch = depth.z.clamp(-1.0, 1.0).asin();
    let (yaw, roll) = if pitch.cos().abs() > ORIENTATION_EPSILON {
        ((-depth.x).atan2(depth.y), (-right.z).atan2(up.z))
    } else {
        // At the X-rotation singularity only yaw + roll is observable. A
        // canonical zero roll preserves the exact camera basis and avoids
        // frame-to-frame Euler ambiguity at top/bottom views.
        (right.y.atan2(right.x), 0.0)
    };
    let values = [yaw, pitch, roll].map(normalize_angle);
    values
        .iter()
        .all(|value| value.is_finite())
        .then_some(values)
}

fn view_orientation_quaternion(view: ViewState) -> [f64; 4] {
    // Camera-to-world orientation is fixed-axis Y (roll), then X (pitch),
    // then Z (yaw): qz * qx * qy.
    let yaw = axis_angle_quaternion([0.0, 0.0, 1.0], view.yaw);
    let pitch = axis_angle_quaternion([1.0, 0.0, 0.0], view.pitch);
    let roll = axis_angle_quaternion([0.0, 1.0, 0.0], view.roll);
    normalized_quaternion(quaternion_multiply(quaternion_multiply(yaw, pitch), roll))
}

fn camera_world_axes(view: ViewState) -> (ProtocolVector3, ProtocolVector3, ProtocolVector3) {
    let orientation = view_orientation_quaternion(view);
    (
        quaternion_rotate(orientation, [1.0, 0.0, 0.0]),
        quaternion_rotate(orientation, [0.0, 1.0, 0.0]),
        quaternion_rotate(orientation, [0.0, 0.0, 1.0]),
    )
}

fn axis_angle_quaternion(axis: [f64; 3], angle: f64) -> [f64; 4] {
    let (sin, cos) = (finite_or_zero(angle) * 0.5).sin_cos();
    [cos, axis[0] * sin, axis[1] * sin, axis[2] * sin]
}

fn quaternion_multiply(left: [f64; 4], right: [f64; 4]) -> [f64; 4] {
    let [lw, lx, ly, lz] = left;
    let [rw, rx, ry, rz] = right;
    [
        lw * rw - lx * rx - ly * ry - lz * rz,
        lw * rx + lx * rw + ly * rz - lz * ry,
        lw * ry - lx * rz + ly * rw + lz * rx,
        lw * rz + lx * ry - ly * rx + lz * rw,
    ]
}

fn normalized_quaternion(quaternion: [f64; 4]) -> [f64; 4] {
    let length_squared = quaternion
        .iter()
        .fold(0.0, |sum, component| component.mul_add(*component, sum));
    if !length_squared.is_finite() || length_squared <= ORIENTATION_EPSILON.powi(2) {
        return [1.0, 0.0, 0.0, 0.0];
    }
    let inverse_length = length_squared.sqrt().recip();
    quaternion.map(|component| component * inverse_length)
}

fn quaternion_slerp(mut source: [f64; 4], target: [f64; 4], progress: f64) -> [f64; 4] {
    let mut dot = source
        .iter()
        .zip(target)
        .map(|(left, right)| *left * right)
        .sum::<f64>();
    if dot < 0.0 {
        source = source.map(|component| -component);
        dot = -dot;
    }
    let progress = progress.clamp(0.0, 1.0);
    if dot > 1.0 - ORIENTATION_EPSILON {
        return normalized_quaternion(std::array::from_fn(|index| {
            source[index] + (target[index] - source[index]) * progress
        }));
    }
    let angle = dot.clamp(-1.0, 1.0).acos();
    let sin_angle = angle.sin();
    if sin_angle.abs() <= ORIENTATION_EPSILON {
        return source;
    }
    let source_weight = ((1.0 - progress) * angle).sin() / sin_angle;
    let target_weight = (progress * angle).sin() / sin_angle;
    normalized_quaternion(std::array::from_fn(|index| {
        source[index] * source_weight + target[index] * target_weight
    }))
}

fn quaternion_rotate(quaternion: [f64; 4], vector: [f64; 3]) -> ProtocolVector3 {
    let [w, x, y, z] = normalized_quaternion(quaternion);
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
    ProtocolVector3::new(
        vector[0] + w * twice_cross[0] + second_cross[0],
        vector[1] + w * twice_cross[1] + second_cross[1],
        vector[2] + w * twice_cross[2] + second_cross[2],
    )
}

fn camera_euler_from_quaternion(quaternion: [f64; 4]) -> Option<[f64; 3]> {
    camera_euler_from_axes(
        quaternion_rotate(quaternion, [1.0, 0.0, 0.0]),
        quaternion_rotate(quaternion, [0.0, 1.0, 0.0]),
        quaternion_rotate(quaternion, [0.0, 0.0, 1.0]),
    )
}

fn smootherstep(progress: f64) -> f64 {
    let progress = progress.clamp(0.0, 1.0);
    progress * progress * progress * (progress * (progress * 6.0 - 15.0) + 10.0)
}

fn logarithmic_lerp(source: f64, target: f64, progress: f64) -> f64 {
    let source = finite_positive_or(source, 1.0);
    let target = finite_positive_or(target, source);
    (source.ln() + (target.ln() - source.ln()) * progress).exp()
}

fn lerp_point(source: Point3, target: Point3, progress: f64) -> Point3 {
    Point3::new(
        source.x + (target.x - source.x) * progress,
        source.y + (target.y - source.y) * progress,
        source.z + (target.z - source.z) * progress,
    )
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

fn normalize_angle(angle: f64) -> f64 {
    (angle + PI).rem_euclid(TAU) - PI
}

fn rotate_euler(mut point: [f64; 3], rotation: [f64; 3]) -> [f64; 3] {
    point = rotate_x(point, finite_or_zero(rotation[0]));
    point = rotate_y(point, finite_or_zero(rotation[1]));
    rotate_z(point, finite_or_zero(rotation[2]))
}

fn euler_quaternion(rotation: [f64; 3]) -> RotationQuaternion {
    let (sin_x, cos_x) = (finite_or_zero(rotation[0]) * 0.5).sin_cos();
    let (sin_y, cos_y) = (finite_or_zero(rotation[1]) * 0.5).sin_cos();
    let (sin_z, cos_z) = (finite_or_zero(rotation[2]) * 0.5).sin_cos();
    // Fixed-axis X then Y then Z is the Hamilton product qz * qy * qx.
    RotationQuaternion::new(
        cos_z * cos_y * cos_x + sin_z * sin_y * sin_x,
        cos_z * cos_y * sin_x - sin_z * sin_y * cos_x,
        cos_z * sin_y * cos_x + sin_z * cos_y * sin_x,
        sin_z * cos_y * cos_x - cos_z * sin_y * sin_x,
    )
}

fn rotate_x(point: [f64; 3], angle: f64) -> [f64; 3] {
    let (sin, cos) = angle.sin_cos();
    [
        point[0],
        cos * point[1] - sin * point[2],
        sin * point[1] + cos * point[2],
    ]
}

fn rotate_y(point: [f64; 3], angle: f64) -> [f64; 3] {
    let (sin, cos) = angle.sin_cos();
    [
        cos * point[0] + sin * point[2],
        point[1],
        -sin * point[0] + cos * point[2],
    ]
}

fn rotate_z(point: [f64; 3], angle: f64) -> [f64; 3] {
    let (sin, cos) = angle.sin_cos();
    [
        cos * point[0] - sin * point[1],
        sin * point[0] + cos * point[1],
        point[2],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::FRAC_PI_2;
    use std::time::{Duration, Instant};

    const EPSILON: f64 = 1.0e-12;

    fn bounds() -> Aabb3 {
        Aabb3::new(Point3::new(0.0, 0.0, 0.0), Point3::new(2.0, 2.0, 2.0))
    }

    fn assert_point_close(actual: Point3, expected: Point3) {
        assert!((actual.x - expected.x).abs() <= EPSILON, "x: {actual:?}");
        assert!((actual.y - expected.y).abs() <= EPSILON, "y: {actual:?}");
        assert!((actual.z - expected.z).abs() <= EPSILON, "z: {actual:?}");
    }

    #[test]
    fn tools_have_stable_labels_and_shortcuts() {
        let values = ActiveTool::ALL.map(|tool| (tool.label(), tool.shortcut()));
        assert_eq!(
            values,
            [
                ("Select", "V"),
                ("Measure", "I"),
                ("Orbit", "O"),
                ("Move", "M"),
                ("Rotate", "R"),
                ("Scale", "S"),
            ]
        );
    }

    #[test]
    fn identity_transform_preserves_point() {
        let point = Point3::new(1.75, 0.25, 1.5);
        assert_point_close(
            DisplayTransform::default().transform_point(point, bounds()),
            point,
        );
    }

    #[test]
    fn translation_moves_point_in_world_space() {
        let transform = DisplayTransform {
            translation: [3.0, -2.0, 0.5],
            ..DisplayTransform::default()
        };
        assert_point_close(
            transform.transform_point(Point3::new(2.0, 1.0, 0.0), bounds()),
            Point3::new(5.0, -1.0, 0.5),
        );
    }

    #[test]
    fn scale_is_bounded_and_uses_bounds_center() {
        let mut transform = DisplayTransform::default();
        transform.set_scale(2.0);
        assert_point_close(
            transform.transform_point(Point3::new(2.0, 1.0, 1.0), bounds()),
            Point3::new(3.0, 1.0, 1.0),
        );

        transform.set_scale(0.0);
        assert_eq!(transform.scale, DisplayTransform::MIN_SCALE);
        transform.set_scale(f64::INFINITY);
        assert_eq!(transform.scale, 1.0);
        transform.set_scale(f64::MAX);
        assert_eq!(transform.scale, DisplayTransform::MAX_SCALE);
    }

    #[test]
    fn euler_z_rotation_uses_bounds_center() {
        let transform = DisplayTransform {
            rotation: [0.0, 0.0, FRAC_PI_2],
            ..DisplayTransform::default()
        };
        assert_point_close(
            transform.transform_point(Point3::new(2.0, 1.0, 1.0), bounds()),
            Point3::new(1.0, 2.0, 1.0),
        );
    }

    #[test]
    fn camera_projection_reports_coordinates_and_depth() {
        let mut view = ViewState {
            yaw: 0.0,
            pitch: 0.0,
            zoom: 2.0,
            ..ViewState::default()
        };
        view.frame(bounds());
        let projection = view.project(Point3::new(2.0, 3.0, 4.0));
        assert_eq!(projection.coordinates, [2.0, -6.0]);
        assert_eq!(projection.depth, 2.0);
    }

    #[test]
    fn face_aligned_camera_maps_the_authoritative_frame_to_screen_axes() {
        let frames = [
            PlanarFrame3::new(
                Point3::new(3.0, -2.0, 5.0),
                ProtocolVector3::new(1.0, 0.0, 0.0),
                ProtocolVector3::new(0.0, 1.0, 0.0),
            ),
            PlanarFrame3::new(
                Point3::new(-1.0, 4.0, 2.0),
                ProtocolVector3::new(0.0, 1.0, 0.0),
                ProtocolVector3::new(0.0, 0.0, 1.0),
            ),
            PlanarFrame3::new(
                Point3::new(2.0, 3.0, -4.0),
                ProtocolVector3::new(1.0, 0.0, 0.0),
                ProtocolVector3::new(0.0, 0.0, 1.0),
            ),
            PlanarFrame3::new(
                Point3::new(7.0, 8.0, 9.0),
                ProtocolVector3::new(1.0, 1.0, 0.0),
                ProtocolVector3::new(-1.0, 1.0, 2.0),
            ),
        ];

        for frame in frames {
            let frame_right = normalized_protocol_vector(frame.u).unwrap();
            let normal = normalized_protocol_vector(protocol_cross(frame_right, frame.v)).unwrap();
            let up = normalized_protocol_vector(protocol_cross(normal, frame_right)).unwrap();
            let target =
                CameraTransition::face_aligned(ViewState::default(), frame, frame.origin, 3.0)
                    .expect("valid face frame")
                    .target();

            let projected_right = target.project_direction(frame_right);
            let projected_up = target.project_direction(up);
            let projected_normal = target.project_direction(normal);
            for projected in [projected_right, projected_up] {
                assert!(projected.depth.abs() <= 1.0e-9);
                assert!(
                    (projected.coordinates[0].hypot(projected.coordinates[1]) - 1.0).abs()
                        <= 1.0e-9
                );
            }
            let screen_dot = projected_right.coordinates[0] * projected_up.coordinates[0]
                + projected_right.coordinates[1] * projected_up.coordinates[1];
            assert!(screen_dot.abs() <= 1.0e-9);
            assert_projection_close(projected_normal, [0.0, 0.0], 1.0);
            assert_projection_close(target.project(frame.origin), [0.0, 0.0], 0.0);

            let source_up = camera_world_axes(ViewState::default()).2;
            let target_up = camera_world_axes(target).2;
            let expected_up =
                if protocol_dot(source_up, frame_right).abs() > protocol_dot(source_up, up).abs() {
                    frame_right
                } else {
                    up
                };
            assert!((protocol_dot(target_up, expected_up).abs() - 1.0).abs() <= 1.0e-9);
            assert!(protocol_dot(target_up, source_up) >= -1.0e-9);
        }
    }

    #[test]
    fn every_axis_face_camera_lands_on_the_selected_exterior_side() {
        let axes = [
            ProtocolVector3::new(1.0, 0.0, 0.0),
            ProtocolVector3::new(-1.0, 0.0, 0.0),
            ProtocolVector3::new(0.0, 1.0, 0.0),
            ProtocolVector3::new(0.0, -1.0, 0.0),
            ProtocolVector3::new(0.0, 0.0, 1.0),
            ProtocolVector3::new(0.0, 0.0, -1.0),
        ];
        for outward_normal in axes {
            let u = if outward_normal.z.abs() > 0.5 {
                ProtocolVector3::new(1.0, 0.0, 0.0)
            } else {
                ProtocolVector3::new(0.0, 0.0, 1.0)
            };
            let v = protocol_cross(outward_normal, u);
            let frame = PlanarFrame3::new(Point3::default(), u, v);
            let target =
                CameraTransition::face_aligned(ViewState::default(), frame, Point3::default(), 1.0)
                    .expect("axis face frame")
                    .target();
            let camera_side = target.project_direction(outward_normal);
            assert_projection_close(camera_side, [0.0, 0.0], 1.0);
        }
    }

    #[test]
    fn face_camera_preserves_the_nearest_upright_or_sideways_quarter_turn() {
        let frame = PlanarFrame3::new(
            Point3::default(),
            ProtocolVector3::new(1.0, 0.0, 0.0),
            ProtocolVector3::new(0.0, 0.0, 1.0),
        );
        let mut upright = ViewState::default();
        upright.set_standard_view(StandardView::Front);
        let upright_target = CameraTransition::face_aligned(upright, frame, Point3::default(), 1.0)
            .unwrap()
            .target();
        assert!(upright_target.project_direction(frame.v).coordinates[1].abs() > 1.0 - 1.0e-9);
        assert_eq!(upright.face_sketch_quarter_turn(frame), Some(0));

        let mut sideways = upright;
        sideways.rotate_in_plane_quarter_turn(true);
        let sideways_target =
            CameraTransition::face_aligned(sideways, frame, Point3::default(), 1.0)
                .unwrap()
                .target();
        assert!(sideways_target.project_direction(frame.u).coordinates[1].abs() > 1.0 - 1.0e-9);
        assert_eq!(sideways.face_sketch_quarter_turn(frame), Some(1));
    }

    #[test]
    fn orbit_uses_camera_local_axes_after_a_rolled_face_view() {
        let mut view = ViewState::default();
        view.set_standard_view(StandardView::Front);
        view.rotate_in_plane_quarter_turn(true);
        let up_before = camera_world_axes(view).2;

        view.orbit(0.37, 0.0);

        let up_after = camera_world_axes(view).2;
        assert!(protocol_dot(up_before, up_after) > 1.0 - 1.0e-9);
    }

    #[test]
    fn every_view_cube_face_snaps_to_its_named_outward_normal() {
        for face in StandardView::ALL {
            let mut view = ViewState::default();
            view.set_standard_view(face);
            assert_eq!(view.nearest_standard_view(), face);
            assert_projection_close(
                view.project_direction(face.outward_normal()),
                [0.0, 0.0],
                1.0,
            );
        }
    }

    #[test]
    fn signed_drag_projection_is_exact_in_screen_plane_and_end_on_views() {
        let diagonal =
            SignedDistanceDragProjection::new([6.0, -8.0], 0.0, 10.0).expect("screen-plane axis");
        assert_eq!(diagonal.facing(), AxisCameraFacing::EdgeOn);
        assert!((diagonal.signed_distance_delta([3.0, -4.0]) - 0.5).abs() <= 1.0e-12);
        assert!((diagonal.signed_distance_delta([-6.0, 8.0]) + 1.0).abs() <= 1.0e-12);

        let front =
            SignedDistanceDragProjection::new([0.0, 0.0], 1.0, 20.0).expect("front end-on axis");
        let rear =
            SignedDistanceDragProjection::new([0.0, 0.0], -1.0, 20.0).expect("rear end-on axis");
        assert_eq!(front.facing(), AxisCameraFacing::TowardCamera);
        assert_eq!(rear.facing(), AxisCameraFacing::AwayFromCamera);
        assert!((front.signed_distance_delta([7.0, -10.0]) - 0.5).abs() <= 1.0e-12);
        assert!((rear.signed_distance_delta([-9.0, 10.0]) + 0.5).abs() <= 1.0e-12);
    }

    #[test]
    fn invalid_drag_projection_fails_closed() {
        assert!(SignedDistanceDragProjection::new([f64::NAN, 0.0], 1.0, 10.0).is_none());
        assert!(SignedDistanceDragProjection::new([1.0, 0.0], 1.0, 0.0).is_none());
        let projection = SignedDistanceDragProjection::new([10.0, 0.0], 0.0, 10.0).unwrap();
        assert_eq!(projection.signed_distance_delta([f64::INFINITY, 0.0]), 0.0);
    }

    #[test]
    fn sixty_hertz_signed_drag_sampling_stays_inside_one_cpu_frame() {
        let projection = SignedDistanceDragProjection::new([7.5, -4.25], 0.4, 9.0).unwrap();
        let start = Instant::now();
        let mut checksum = 0.0;
        for sample in 0..10_000 {
            let value = f64::from(sample) * 1.0e-4;
            checksum += projection.signed_distance_delta([value.sin(), value.cos()]);
        }
        let elapsed = start.elapsed();
        assert!(checksum.is_finite());
        assert!(
            elapsed < Duration::from_micros(16_667),
            "10,000 drag projections took {elapsed:?}"
        );
    }

    #[test]
    fn face_camera_transition_is_smooth_deterministic_and_exact_at_completion() {
        let frame = PlanarFrame3::new(
            Point3::new(4.0, -3.0, 2.0),
            ProtocolVector3::new(1.0, 1.0, 0.5),
            ProtocolVector3::new(-0.25, 0.75, 1.0),
        );
        let focus = Point3::new(5.0, -1.0, 4.0);
        let mut first =
            CameraTransition::face_aligned(ViewState::default(), frame, focus, 2.5).unwrap();
        let mut second = first;
        assert_eq!(first.current(), ViewState::default());

        for delta in [1.0 / 120.0, 1.0 / 60.0, 0.04, 0.08, 0.2] {
            assert_eq!(first.advance(delta), second.advance(delta));
        }
        assert!(first.is_complete());
        assert_eq!(first.current(), first.target());
        assert_eq!(first.current().target(), focus);
        assert_eq!(first.current().fit_radius(), 2.5);
    }

    #[test]
    fn invalid_face_camera_targets_are_rejected_without_mutating_the_source() {
        let source = ViewState::default();
        let degenerate = PlanarFrame3::new(
            Point3::default(),
            ProtocolVector3::new(1.0, 0.0, 0.0),
            ProtocolVector3::new(2.0, 0.0, 0.0),
        );
        assert!(
            CameraTransition::face_aligned(source, degenerate, Point3::default(), 1.0).is_none()
        );
        assert!(
            CameraTransition::face_aligned(
                source,
                PlanarFrame3::new(
                    Point3::default(),
                    ProtocolVector3::new(1.0, 0.0, 0.0),
                    ProtocolVector3::new(0.0, 1.0, 0.0),
                ),
                Point3::new(f64::NAN, 0.0, 0.0),
                1.0,
            )
            .is_none()
        );
    }

    #[test]
    fn one_second_of_camera_transition_sampling_fits_one_60hz_cpu_frame() {
        let frame = PlanarFrame3::new(
            Point3::new(2.0, 3.0, 4.0),
            ProtocolVector3::new(1.0, 2.0, 0.5),
            ProtocolVector3::new(-0.5, 1.0, 2.0),
        );
        let transition = CameraTransition::face_aligned(
            ViewState::default(),
            frame,
            Point3::new(3.0, 2.0, 1.0),
            4.0,
        )
        .unwrap();
        let start = Instant::now();
        let mut checksum = 0.0;
        for frame_index in 0..360 {
            let mut sample = transition;
            sample.elapsed_seconds =
                FACE_CAMERA_TRANSITION_SECONDS * f64::from(frame_index) / 359.0;
            let view = sample.current();
            checksum += view.yaw + view.pitch + view.roll + view.zoom;
        }
        let elapsed = start.elapsed();
        assert!(checksum.is_finite());
        assert!(
            elapsed < Duration::from_micros(16_667),
            "360 deterministic camera samples took {elapsed:?}"
        );
    }

    #[test]
    fn screen_plane_delta_stays_visible_after_orbit() {
        let mut view = ViewState {
            yaw: 1.2,
            pitch: -0.7,
            zoom: 1.0,
            ..ViewState::default()
        };
        view.frame(bounds());
        let center = Point3::new(1.0, 1.0, 1.0);
        let delta = view.world_delta_from_screen(0.75, -0.4);
        let moved = Point3::new(
            center.x + delta[0],
            center.y + delta[1],
            center.z + delta[2],
        );
        let projected_center = view.project(center);
        let projected_moved = view.project(moved);

        assert!(
            (projected_moved.coordinates[0] - projected_center.coordinates[0] - 0.75).abs()
                <= EPSILON
        );
        assert!(
            (projected_moved.coordinates[1] - projected_center.coordinates[1] + 0.4).abs()
                <= EPSILON
        );
        assert!((projected_moved.depth - projected_center.depth).abs() <= EPSILON);
    }

    fn assert_projection_close(
        actual: CameraProjection,
        expected_coordinates: [f64; 2],
        expected_depth: f64,
    ) {
        assert!(
            (actual.coordinates[0] - expected_coordinates[0]).abs() <= 1.0e-9,
            "horizontal: {actual:?}"
        );
        assert!(
            (actual.coordinates[1] - expected_coordinates[1]).abs() <= 1.0e-9,
            "vertical: {actual:?}"
        );
        assert!(
            (actual.depth - expected_depth).abs() <= 1.0e-9,
            "depth: {actual:?}"
        );
    }

    #[test]
    fn pivoted_preview_converts_to_the_same_origin_based_similarity() {
        let transform = DisplayTransform {
            translation: [3.0, -2.0, 0.5],
            rotation: [0.3, -0.4, 0.7],
            scale: 1.6,
        };
        let pivot = bounds_center(bounds());
        let point = Point3::new(2.0, 0.25, 1.5);
        let expected = transform.transform_point_about(point, pivot);
        let similarity = transform.kernel_similarity(pivot);
        let rotation = similarity.rotation;
        let vector = [point.x, point.y, point.z];
        let imaginary = [rotation.x, rotation.y, rotation.z];
        let cross = |a: [f64; 3], b: [f64; 3]| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let twice_cross = cross(imaginary, vector).map(|value| value * 2.0);
        let second_cross = cross(imaginary, twice_cross);
        let rotated = [0, 1, 2]
            .map(|index| vector[index] + rotation.w * twice_cross[index] + second_cross[index]);
        let actual = Point3::new(
            rotated[0] * similarity.uniform_scale + similarity.translation.x,
            rotated[1] * similarity.uniform_scale + similarity.translation.y,
            rotated[2] * similarity.uniform_scale + similarity.translation.z,
        );
        assert_point_close(actual, expected);
    }

    #[test]
    fn animation_is_composed_after_preview_and_not_baked_into_kernel_transform() {
        let transform = DisplayTransform {
            translation: [2.0, 1.0, 0.0],
            rotation: [0.0, 0.0, FRAC_PI_2],
            scale: 1.5,
        };
        let pivot = bounds_center(bounds());
        let point = Point3::new(2.0, 1.0, 1.0);
        let previewed = transform.present_point(point, pivot, 0.0);
        let animated = transform.present_point(point, pivot, FRAC_PI_2);
        assert_ne!(previewed, animated);
    }

    #[test]
    fn motion_advance_is_deterministic_and_clamped() {
        let mut first = MotionState::default();
        let mut second = MotionState::default();
        first.play();
        second.play();

        for delta in [1.0 / 60.0, 1.0 / 30.0, 0.5, 1.0 / 120.0] {
            first.advance(delta);
            second.advance(delta);
        }
        assert_eq!(first, second);

        let expected_seconds = 1.0 / 60.0 + 1.0 / 30.0 + 0.25 + 1.0 / 120.0;
        let expected_phase = first.speed_rpm * TAU / 60.0 * expected_seconds;
        assert!((first.phase - expected_phase).abs() <= EPSILON);
    }

    #[test]
    fn stopping_motion_restores_the_authored_pose_and_stays_there() {
        let mut motion = MotionState {
            phase: 1.25,
            smoothed_fps: Some(60.0),
            ..MotionState::default()
        };
        motion.pause();
        assert_eq!(motion.phase, 0.0);
        assert_eq!(motion.smoothed_fps, None);
        motion.advance(1.0 / 30.0);
        assert_eq!(motion.phase, 0.0);

        motion.advance(f64::NAN);
        motion.advance(-1.0);
        assert_eq!(motion.phase, 0.0);
    }

    #[test]
    fn fps_uses_deterministic_exponential_smoothing() {
        let mut motion = MotionState {
            smoothed_fps: Some(60.0),
            ..MotionState::default()
        };
        motion.advance(1.0 / 30.0);
        let first_delta = 1.0 / 60.0 + (1.0 / 30.0 - 1.0 / 60.0) * FPS_SMOOTHING_WEIGHT;
        assert!((motion.smoothed_fps.unwrap() - 1.0 / first_delta).abs() <= EPSILON);
        motion.advance(1.0 / 30.0);
        let second_delta = first_delta + (1.0 / 30.0 - first_delta) * FPS_SMOOTHING_WEIGHT;
        assert!((motion.smoothed_fps.unwrap() - 1.0 / second_delta).abs() <= EPSILON);
    }

    #[test]
    fn jittery_cadence_smooths_frame_time_not_fps_arithmetic_mean() {
        let mut motion = MotionState::default();
        for _ in 0..100 {
            motion.advance(1.0 / 120.0);
            motion.advance(1.0 / 30.0);
        }
        let fps = motion.smoothed_fps.unwrap();
        assert!((45.0..=52.0).contains(&fps), "reported {fps} FPS");
    }

    #[test]
    fn fps_is_unknown_until_a_valid_frame_is_sampled() {
        let mut motion = MotionState::default();
        assert_eq!(motion.smoothed_fps, None);
        motion.advance(f64::NAN);
        motion.advance(f64::from_bits(1));
        assert_eq!(motion.smoothed_fps, None);
        motion.advance(1.0 / 120.0);
        assert_eq!(motion.smoothed_fps, Some(120.0));
    }

    #[test]
    fn long_stall_reports_raw_cadence_but_clamps_phase_jump() {
        let mut motion = MotionState::default();
        motion.play();
        motion.advance(1.0);

        assert_eq!(motion.smoothed_fps, Some(1.0));
        let expected_phase = motion.speed_rpm * TAU / 60.0 * MAX_FRAME_DELTA_SECONDS;
        assert!((motion.phase - expected_phase).abs() <= EPSILON);
    }

    #[test]
    fn reset_restores_motion_defaults() {
        let mut motion = MotionState::default();
        motion.play();
        motion.set_speed_rpm(500.0);
        motion.advance(0.1);
        motion.reset();
        assert_eq!(motion, MotionState::default());
        assert_eq!(motion.target_hz, 60);
        assert_eq!(motion.frame_interval(), 1.0 / 60.0);
    }
}
