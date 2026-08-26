use std::collections::{BTreeMap, BTreeSet};

use artificer_protocol::{ArcDirection, PlanarCurve2, PrecisionPolicy};

use crate::{
    Angle, CircularPatternDistribution, CurveDirection, CurveIntersections, CurveOutputRole,
    CurveProvenance, EvaluatedCurve2, FilletBranchHints, Integer, Length, MAX_ACTIVE_SKETCH_CURVES,
    MAX_ACTIVE_SKETCH_POINTS, MAX_CURVE_EDITS_PER_TRANSACTION, MAX_PATTERN_INSTANCES,
    MAX_POLYGON_SIDES, MIN_POLYGON_SIDES, OutputRole, PointInput, PointOutputRole, SignedLength,
    SketchCurve2, SketchDefinition, SketchEntityId, SketchEntityRecord, SketchEntityRole,
    SketchInputValues, SketchOperationId, SketchOperationRecord, SketchOutputOwner,
    SketchOutputRef, SketchPoint2, SketchPointId, SketchPointRecord, SketchRecipe,
    SketchValidationError, SketchValue, SketchVector2, TrimCurve, intersect_curves,
    select_trim_span,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointBindingDraft {
    Existing(SketchPointId),
    Output(PointOutputRole),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointOutputDraft {
    pub role: PointOutputRole,
    pub position: SketchPoint2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurveDraft2 {
    Line {
        start: PointBindingDraft,
        end: PointBindingDraft,
    },
    CircularArc {
        center: PointBindingDraft,
        start: PointBindingDraft,
        end: PointBindingDraft,
        direction: CurveDirection,
    },
    Circle {
        center: PointBindingDraft,
        radius: f64,
        direction: CurveDirection,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurveOutputDraft {
    pub role: CurveOutputRole,
    pub entity_role: SketchEntityRole,
    pub geometry: CurveDraft2,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrimitiveEvaluation {
    pub points: Vec<PointOutputDraft>,
    pub curves: Vec<CurveOutputDraft>,
}

impl PrimitiveEvaluation {
    #[must_use]
    pub fn curve_count(&self) -> usize {
        self.curves.len()
    }
}

/// Deterministically evaluates a persisted primitive recipe without allocating
/// permanent IDs or mutating the sketch definition.
pub fn evaluate_recipe(
    definition: &SketchDefinition,
    recipe: &SketchRecipe,
    inputs: &SketchInputValues,
    precision: PrecisionPolicy,
) -> Result<PrimitiveEvaluation, SketchValidationError> {
    let mut builder = EvaluationBuilder::new(definition, precision);
    let entity_role = recipe.default_curve_role();

    match recipe {
        SketchRecipe::LegacyImportedProfile { profile } => {
            let mut imported_points = BTreeMap::new();
            let mut point_index = 0_usize;
            let mut curve_index = 0_usize;
            for region in &profile.regions {
                for profile_loop in std::iter::once(&region.outer).chain(region.holes.iter()) {
                    for curve in &profile_loop.curves {
                        let role = CurveOutputRole::ImportedCurve(index_u16(curve_index)?);
                        curve_index = curve_index
                            .checked_add(1)
                            .ok_or(SketchValidationError::ArithmeticOverflow)?;
                        match *curve {
                            PlanarCurve2::Line { start, end } => {
                                let start = import_point(
                                    &mut builder,
                                    &mut imported_points,
                                    &mut point_index,
                                    start.into(),
                                )?;
                                let end = import_point(
                                    &mut builder,
                                    &mut imported_points,
                                    &mut point_index,
                                    end.into(),
                                )?;
                                builder.add_line(role, SketchEntityRole::Profile, start, end)?;
                            }
                            PlanarCurve2::CircularArc {
                                center,
                                start,
                                end,
                                direction,
                            } => {
                                let center = import_point(
                                    &mut builder,
                                    &mut imported_points,
                                    &mut point_index,
                                    center.into(),
                                )?;
                                let start = import_point(
                                    &mut builder,
                                    &mut imported_points,
                                    &mut point_index,
                                    start.into(),
                                )?;
                                let end = import_point(
                                    &mut builder,
                                    &mut imported_points,
                                    &mut point_index,
                                    end.into(),
                                )?;
                                builder.add_arc(
                                    role,
                                    SketchEntityRole::Profile,
                                    center,
                                    start,
                                    end,
                                    protocol_direction(direction),
                                )?;
                            }
                            PlanarCurve2::Circle {
                                center,
                                radius,
                                direction,
                            } => {
                                let center = import_point(
                                    &mut builder,
                                    &mut imported_points,
                                    &mut point_index,
                                    center.into(),
                                )?;
                                builder.add_circle(
                                    role,
                                    SketchEntityRole::Profile,
                                    center,
                                    radius,
                                    protocol_direction(direction),
                                )?;
                            }
                        }
                    }
                }
            }
        }
        SketchRecipe::Point { position } => {
            builder.add_derived_point(PointOutputRole::Point, *position)?;
        }
        SketchRecipe::Line { start, end } | SketchRecipe::CentreLine { start, end } => {
            let start = builder.bind_input(*start, PointOutputRole::Start)?;
            let end = builder.bind_input(*end, PointOutputRole::End)?;
            builder.add_line(CurveOutputRole::Curve, entity_role, start, end)?;
        }
        SketchRecipe::Polyline {
            vertices,
            closed,
            construction,
        } => {
            let minimum = if *closed { 3 } else { 2 };
            if vertices.len() < minimum {
                return Err(SketchValidationError::ResourceLimit {
                    resource: "polyline_vertices",
                    requested: vertices.len(),
                    limit: minimum,
                });
            }
            let segment_count = vertices.len() - usize::from(!*closed);
            if segment_count > MAX_CURVE_EDITS_PER_TRANSACTION {
                return Err(SketchValidationError::ResourceLimit {
                    resource: "curve_edits",
                    requested: segment_count,
                    limit: MAX_CURVE_EDITS_PER_TRANSACTION,
                });
            }
            let mut bindings = Vec::with_capacity(vertices.len());
            for (index, vertex) in vertices.iter().copied().enumerate() {
                let role = PointOutputRole::Vertex(index_u16(index)?);
                bindings.push(builder.bind_input(vertex, role)?);
            }
            let mut exact_positions = BTreeSet::new();
            for binding in &bindings {
                let position = builder.position(*binding)?;
                let bits = (position.u.to_bits(), position.v.to_bits());
                if !exact_positions.insert(bits) {
                    return Err(SketchValidationError::FeatureTooSmall {
                        operation: placeholder_operation(),
                    });
                }
            }
            let role = if *construction {
                SketchEntityRole::Construction
            } else {
                SketchEntityRole::Profile
            };
            for index in 0..segment_count {
                let end_index = (index + 1) % bindings.len();
                builder.add_line(
                    CurveOutputRole::Segment(index_u16(index)?),
                    role,
                    bindings[index],
                    bindings[end_index],
                )?;
            }
        }
        SketchRecipe::TwoPointRectangle {
            first_corner,
            width,
            height,
        } => {
            let anchor = builder.bind_input(*first_corner, PointOutputRole::Corner(0))?;
            let anchor_position = builder.position(anchor)?;
            let width = resolve_signed(*width, inputs)?;
            let height = resolve_signed(*height, inputs)?;
            require_component_size(width, precision)?;
            require_component_size(height, precision)?;
            let horizontal = SketchPoint2::new(anchor_position.u + width, anchor_position.v);
            let opposite = SketchPoint2::new(anchor_position.u + width, anchor_position.v + height);
            let vertical = SketchPoint2::new(anchor_position.u, anchor_position.v + height);
            let center = SketchPoint2::new(
                anchor_position.u + width * 0.5,
                anchor_position.v + height * 0.5,
            );
            builder.add_derived_point(PointOutputRole::Center, center)?;
            let h = builder.add_derived_point(PointOutputRole::Corner(1), horizontal)?;
            let o = builder.add_derived_point(PointOutputRole::Corner(2), opposite)?;
            let v = builder.add_derived_point(PointOutputRole::Corner(3), vertical)?;
            let corners = if width * height > 0.0 {
                [anchor, h, o, v]
            } else {
                [anchor, v, o, h]
            };
            builder.add_closed_lines(&corners, entity_role, CurveOutputRole::Side)?;
        }
        SketchRecipe::CentrePointRectangle {
            center,
            width,
            height,
        } => {
            let center = builder.bind_input(*center, PointOutputRole::Center)?;
            let center_position = builder.position(center)?;
            let width = resolve_length(*width, inputs)?;
            let height = resolve_length(*height, inputs)?;
            require_component_size(width, precision)?;
            require_component_size(height, precision)?;
            let half_width = width * 0.5;
            let half_height = height * 0.5;
            let positions = [
                SketchPoint2::new(
                    center_position.u - half_width,
                    center_position.v - half_height,
                ),
                SketchPoint2::new(
                    center_position.u + half_width,
                    center_position.v - half_height,
                ),
                SketchPoint2::new(
                    center_position.u + half_width,
                    center_position.v + half_height,
                ),
                SketchPoint2::new(
                    center_position.u - half_width,
                    center_position.v + half_height,
                ),
            ];
            let mut corners = Vec::with_capacity(4);
            for (index, position) in positions.into_iter().enumerate() {
                corners.push(
                    builder
                        .add_derived_point(PointOutputRole::Corner(index_u16(index)?), position)?,
                );
            }
            builder.add_closed_lines(&corners, entity_role, CurveOutputRole::Side)?;
        }
        SketchRecipe::CentrePointCircle {
            center,
            radius,
            radial_angle,
        } => {
            let center = builder.bind_input(*center, PointOutputRole::Center)?;
            let center_position = builder.position(center)?;
            let radius = resolve_length(*radius, inputs)?;
            require_component_size(radius, precision)?;
            let angle = resolve_angle(*radial_angle, inputs)?;
            let radial = SketchPoint2::new(
                center_position.u + radius * angle.cos(),
                center_position.v + radius * angle.sin(),
            );
            builder.add_derived_point(PointOutputRole::RadialPoint, radial)?;
            builder.add_circle(
                CurveOutputRole::Curve,
                entity_role,
                center,
                radius,
                CurveDirection::CounterClockwise,
            )?;
        }
        SketchRecipe::TwoPointCircle {
            first_diameter_point,
            second_diameter_point,
            direction,
        } => {
            let first =
                builder.bind_input(*first_diameter_point, PointOutputRole::DiameterPoint(0))?;
            let second =
                builder.bind_input(*second_diameter_point, PointOutputRole::DiameterPoint(1))?;
            let first_position = builder.position(first)?;
            let second_position = builder.position(second)?;
            let diameter = distance(first_position, second_position);
            require_component_size(diameter, precision)?;
            let center_position = midpoint(first_position, second_position);
            let center = builder.add_derived_point(PointOutputRole::Center, center_position)?;
            builder.add_circle(
                CurveOutputRole::Curve,
                entity_role,
                center,
                diameter * 0.5,
                *direction,
            )?;
        }
        SketchRecipe::CentreStartEndArc {
            center,
            start,
            end,
            direction,
        } => {
            let center = builder.bind_input(*center, PointOutputRole::Center)?;
            let start = builder.bind_input(*start, PointOutputRole::ArcStart)?;
            let end = builder.bind_input(*end, PointOutputRole::ArcEnd)?;
            let center_position = builder.position(center)?;
            let start_position = builder.position(start)?;
            let end_position = builder.position(end)?;
            let start_radius = distance(center_position, start_position);
            let end_radius = distance(center_position, end_position);
            require_component_size(start_radius, precision)?;
            if (start_radius - end_radius).abs() > precision.linear_agreement
                || distance(start_position, end_position) < precision.min_feature_size
            {
                return Err(SketchValidationError::ArcRadiusMismatch {
                    operation: placeholder_operation(),
                });
            }
            builder.add_arc(
                CurveOutputRole::Curve,
                entity_role,
                center,
                start,
                end,
                *direction,
            )?;
        }
        SketchRecipe::InnerDiameterPolygon {
            center,
            inner_diameter,
            sides,
            rotation,
        } => {
            let apothem = resolve_length(*inner_diameter, inputs)? * 0.5;
            let side_count = resolve_sides(*sides, inputs)?;
            let circumradius = apothem / (std::f64::consts::PI / f64::from(side_count)).cos();
            let rotation = resolve_angle(*rotation, inputs)?;
            builder.add_polygon(*center, circumradius, side_count, rotation, entity_role)?;
        }
        SketchRecipe::OuterDiameterPolygon {
            center,
            outer_diameter,
            sides,
            rotation,
        } => {
            let circumradius = resolve_length(*outer_diameter, inputs)? * 0.5;
            let side_count = resolve_sides(*sides, inputs)?;
            let rotation = resolve_angle(*rotation, inputs)?;
            builder.add_polygon(*center, circumradius, side_count, rotation, entity_role)?;
        }
        SketchRecipe::TwoPointSlot {
            first_cap_center,
            second_cap_center,
            width,
        } => {
            let first = builder.bind_input(*first_cap_center, PointOutputRole::CapCenter(0))?;
            let second = builder.bind_input(*second_cap_center, PointOutputRole::CapCenter(1))?;
            let first_pos = builder.position(first)?;
            let second_pos = builder.position(second)?;
            let center = SketchPoint2::new(
                (first_pos.u + second_pos.u) * 0.5,
                (first_pos.v + second_pos.v) * 0.5,
            );
            builder.add_derived_point(PointOutputRole::Center, center)?;
            let width = resolve_length(*width, inputs)?;
            builder.add_slot(first, second, width, entity_role)?;
        }
        SketchRecipe::CentreOuterPointSlot {
            center,
            overall_length,
            width,
            angle,
        } => {
            let center = builder.bind_input(*center, PointOutputRole::Center)?;
            let center_position = builder.position(center)?;
            let overall_length = resolve_length(*overall_length, inputs)?;
            let width = resolve_length(*width, inputs)?;
            let angle = resolve_angle(*angle, inputs)?;
            if overall_length <= width || width < precision.min_feature_size {
                return Err(SketchValidationError::InvalidSlotDimensions);
            }
            let half_separation = (overall_length - width) * 0.5;
            let delta_u = angle.cos() * half_separation;
            let delta_v = angle.sin() * half_separation;
            let first = builder.add_derived_point(
                PointOutputRole::CapCenter(0),
                SketchPoint2::new(center_position.u - delta_u, center_position.v - delta_v),
            )?;
            let second = builder.add_derived_point(
                PointOutputRole::CapCenter(1),
                SketchPoint2::new(center_position.u + delta_u, center_position.v + delta_v),
            )?;
            builder.add_slot(first, second, width, entity_role)?;
        }
        SketchRecipe::RectangularPattern {
            sources,
            columns,
            rows,
            column_spacing,
            row_spacing,
            direction,
        } => {
            let sources = pattern_sources(definition, sources)?;
            let columns = resolve_pattern_count(*columns, inputs, 1)?;
            let rows = resolve_pattern_count(*rows, inputs, 1)?;
            let instances = usize::from(columns) * usize::from(rows);
            if !(2..=usize::from(MAX_PATTERN_INSTANCES)).contains(&instances) {
                return Err(SketchValidationError::ResourceLimit {
                    resource: "pattern_instances",
                    requested: instances,
                    limit: usize::from(MAX_PATTERN_INSTANCES),
                });
            }
            let column_spacing = resolve_signed(*column_spacing, inputs)?;
            let row_spacing = resolve_signed(*row_spacing, inputs)?;
            if columns > 1 {
                require_component_size(column_spacing, precision)?;
            }
            if rows > 1 {
                require_component_size(row_spacing, precision)?;
            }
            let direction = resolve_angle(*direction, inputs)?;
            let column_axis = (direction.cos(), direction.sin());
            let row_axis = (-column_axis.1, column_axis.0);
            for row in 0..rows {
                for column in 0..columns {
                    let instance = row * columns + column;
                    if instance == 0 {
                        continue;
                    }
                    let translation = (
                        f64::from(column) * column_spacing * column_axis.0
                            + f64::from(row) * row_spacing * row_axis.0,
                        f64::from(column) * column_spacing * column_axis.1
                            + f64::from(row) * row_spacing * row_axis.1,
                    );
                    for (source_index, (_, role, curve)) in sources.iter().enumerate() {
                        builder.add_pattern_curve(
                            instance,
                            index_u16(source_index)?,
                            *role,
                            *curve,
                            |point| {
                                SketchPoint2::new(point.u + translation.0, point.v + translation.1)
                            },
                        )?;
                    }
                }
            }
        }
        SketchRecipe::CircularPattern {
            sources,
            center,
            count,
            total_angle,
            distribution,
            rotate_instances,
        } => {
            let sources = pattern_sources(definition, sources)?;
            let count = resolve_pattern_count(*count, inputs, 2)?;
            let total_angle = resolve_angle(*total_angle, inputs)?;
            if total_angle.abs() < precision.angular_agreement_radians {
                return Err(SketchValidationError::FeatureTooSmall {
                    operation: placeholder_operation(),
                });
            }
            let center = builder.bind_input(*center, PointOutputRole::Center)?;
            let center_position = builder.position(center)?;
            let selection_anchor = pattern_anchor(&sources);
            let divisor = match distribution {
                CircularPatternDistribution::Complete => f64::from(count),
                CircularPatternDistribution::Extent => f64::from(count - 1),
            };
            for instance in 1..count {
                let angle = total_angle * f64::from(instance) / divisor;
                let rotated_anchor = rotate_about(selection_anchor, center_position, angle);
                let translation = rotated_anchor - selection_anchor;
                for (source_index, (_, role, curve)) in sources.iter().enumerate() {
                    builder.add_pattern_curve(
                        instance,
                        index_u16(source_index)?,
                        *role,
                        *curve,
                        |point| {
                            if *rotate_instances {
                                rotate_about(point, center_position, angle)
                            } else {
                                point + translation
                            }
                        },
                    )?;
                }
            }
        }
        SketchRecipe::Fillet {
            first,
            second,
            radius,
        } => {
            builder.retire_curve_budget(2);
            let radius = resolve_length(*radius, inputs)?;
            require_component_size(radius, precision)?;
            add_fillet(&mut builder, definition, *first, *second, radius, precision)?;
        }
        SketchRecipe::FilletWithHints {
            first,
            second,
            radius,
            hints,
        } => {
            builder.retire_curve_budget(2);
            let radius = resolve_length(*radius, inputs)?;
            require_component_size(radius, precision)?;
            add_general_fillet(
                &mut builder,
                definition,
                *first,
                *second,
                radius,
                *hints,
                precision,
            )?;
        }
        SketchRecipe::Chamfer {
            first,
            second,
            first_distance,
            second_distance,
        } => {
            builder.retire_curve_budget(2);
            let first_distance = resolve_length(*first_distance, inputs)?;
            let second_distance = resolve_length(*second_distance, inputs)?;
            require_component_size(first_distance, precision)?;
            require_component_size(second_distance, precision)?;
            add_chamfer(
                &mut builder,
                definition,
                *first,
                *second,
                first_distance,
                second_distance,
                precision,
            )?;
        }
        SketchRecipe::Trim {
            target,
            limits,
            pick,
        } => {
            let target_record = definition
                .entity(*target)
                .filter(|record| record.active && record.visible)
                .ok_or(SketchValidationError::MissingEntity { entity: *target })?;
            let target_curve = definition
                .evaluated_curve(*target)
                .map_err(|_| SketchValidationError::MissingEntity { entity: *target })?;
            let mut canonical_limits = limits.clone();
            canonical_limits.sort_unstable();
            canonical_limits.dedup();
            if canonical_limits.is_empty() {
                return Err(SketchValidationError::EmptyEntitySelection);
            }
            if canonical_limits.contains(target) {
                return Err(SketchValidationError::DuplicateEntitySelection { entity: *target });
            }
            let mut trim_limits = Vec::with_capacity(canonical_limits.len());
            for limit in canonical_limits {
                let record = definition
                    .entity(limit)
                    .filter(|record| record.active && record.visible)
                    .ok_or(SketchValidationError::MissingEntity { entity: limit })?;
                if record.role != target_record.role {
                    return Err(SketchValidationError::TrimRoleMismatch {
                        target: *target,
                        limit,
                    });
                }
                let curve = definition
                    .evaluated_curve(limit)
                    .map_err(|_| SketchValidationError::MissingEntity { entity: limit })?;
                trim_limits.push(TrimCurve {
                    entity: limit,
                    curve,
                });
            }
            let selection = select_trim_span(
                TrimCurve {
                    entity: *target,
                    curve: target_curve,
                },
                &trim_limits,
                *pick,
                &precision,
                MAX_CURVE_EDITS_PER_TRANSACTION,
            )
            .map_err(|_| SketchValidationError::InvalidTrimSelection)?;
            builder.retire_curve_budget(1);
            for (index, fragment) in selection.retained.into_iter().enumerate() {
                builder.add_trim_fragment(index_u16(index)?, target_record.role, fragment.curve)?;
            }
        }
    }

    builder.finish()
}

struct EvaluationBuilder<'a> {
    definition: &'a SketchDefinition,
    precision: PrecisionPolicy,
    points: Vec<PointOutputDraft>,
    point_positions: BTreeMap<PointOutputRole, SketchPoint2>,
    curves: Vec<CurveOutputDraft>,
    curve_roles: BTreeSet<CurveOutputRole>,
    retired_curve_budget: usize,
}

impl<'a> EvaluationBuilder<'a> {
    fn new(definition: &'a SketchDefinition, precision: PrecisionPolicy) -> Self {
        Self {
            definition,
            precision,
            points: Vec::new(),
            point_positions: BTreeMap::new(),
            curves: Vec::new(),
            curve_roles: BTreeSet::new(),
            retired_curve_budget: 0,
        }
    }

    fn bind_input(
        &mut self,
        input: PointInput,
        role: PointOutputRole,
    ) -> Result<PointBindingDraft, SketchValidationError> {
        match input {
            PointInput::Existing(id) => {
                let point = self
                    .definition
                    .point(id)
                    .ok_or(SketchValidationError::MissingPoint { point: id })?;
                if !point.active {
                    return Err(SketchValidationError::InactivePointReference { point: id });
                }
                Ok(PointBindingDraft::Existing(id))
            }
            PointInput::Position(position) => self.add_derived_point(role, position),
        }
    }

    fn add_derived_point(
        &mut self,
        role: PointOutputRole,
        position: SketchPoint2,
    ) -> Result<PointBindingDraft, SketchValidationError> {
        validate_position(position, self.precision)?;
        if self.point_positions.insert(role, position).is_some() {
            return Err(SketchValidationError::DuplicateOutputRole);
        }
        self.points.push(PointOutputDraft { role, position });
        Ok(PointBindingDraft::Output(role))
    }

    fn position(&self, binding: PointBindingDraft) -> Result<SketchPoint2, SketchValidationError> {
        match binding {
            PointBindingDraft::Existing(id) => self
                .definition
                .point(id)
                .filter(|point| point.active)
                .map(|point| point.evaluated_position)
                .ok_or(SketchValidationError::MissingPoint { point: id }),
            PointBindingDraft::Output(role) => self
                .point_positions
                .get(&role)
                .copied()
                .ok_or(SketchValidationError::DuplicateOutputRole),
        }
    }

    fn add_line(
        &mut self,
        role: CurveOutputRole,
        entity_role: SketchEntityRole,
        start: PointBindingDraft,
        end: PointBindingDraft,
    ) -> Result<(), SketchValidationError> {
        if distance(self.position(start)?, self.position(end)?) < self.precision.min_feature_size {
            return Err(SketchValidationError::FeatureTooSmall {
                operation: placeholder_operation(),
            });
        }
        self.add_curve(role, entity_role, CurveDraft2::Line { start, end })
    }

    fn add_arc(
        &mut self,
        role: CurveOutputRole,
        entity_role: SketchEntityRole,
        center: PointBindingDraft,
        start: PointBindingDraft,
        end: PointBindingDraft,
        direction: CurveDirection,
    ) -> Result<(), SketchValidationError> {
        self.add_curve(
            role,
            entity_role,
            CurveDraft2::CircularArc {
                center,
                start,
                end,
                direction,
            },
        )
    }

    fn add_circle(
        &mut self,
        role: CurveOutputRole,
        entity_role: SketchEntityRole,
        center: PointBindingDraft,
        radius: f64,
        direction: CurveDirection,
    ) -> Result<(), SketchValidationError> {
        if !radius.is_finite() || radius < self.precision.min_feature_size {
            return Err(SketchValidationError::FeatureTooSmall {
                operation: placeholder_operation(),
            });
        }
        self.add_curve(
            role,
            entity_role,
            CurveDraft2::Circle {
                center,
                radius,
                direction,
            },
        )
    }

    fn add_curve(
        &mut self,
        role: CurveOutputRole,
        entity_role: SketchEntityRole,
        geometry: CurveDraft2,
    ) -> Result<(), SketchValidationError> {
        if !self.curve_roles.insert(role) {
            return Err(SketchValidationError::DuplicateOutputRole);
        }
        self.curves.push(CurveOutputDraft {
            role,
            entity_role,
            geometry,
        });
        Ok(())
    }

    fn add_closed_lines(
        &mut self,
        points: &[PointBindingDraft],
        entity_role: SketchEntityRole,
        role: fn(u16) -> CurveOutputRole,
    ) -> Result<(), SketchValidationError> {
        for index in 0..points.len() {
            self.add_line(
                role(index_u16(index)?),
                entity_role,
                points[index],
                points[(index + 1) % points.len()],
            )?;
        }
        Ok(())
    }

    fn add_polygon(
        &mut self,
        center_input: PointInput,
        circumradius: f64,
        side_count: u16,
        rotation: f64,
        entity_role: SketchEntityRole,
    ) -> Result<(), SketchValidationError> {
        require_component_size(circumradius, self.precision)?;
        let center = self.bind_input(center_input, PointOutputRole::Center)?;
        let center_position = self.position(center)?;
        let mut vertices = Vec::with_capacity(usize::from(side_count));
        let step = std::f64::consts::TAU / f64::from(side_count);
        for index in 0..side_count {
            let angle = rotation + f64::from(index) * step;
            let point = SketchPoint2::new(
                center_position.u + circumradius * angle.cos(),
                center_position.v + circumradius * angle.sin(),
            );
            vertices.push(self.add_derived_point(PointOutputRole::Vertex(index), point)?);
        }
        self.add_closed_lines(&vertices, entity_role, CurveOutputRole::Side)
    }

    fn add_slot(
        &mut self,
        first_center: PointBindingDraft,
        second_center: PointBindingDraft,
        width: f64,
        entity_role: SketchEntityRole,
    ) -> Result<(), SketchValidationError> {
        require_component_size(width, self.precision)?;
        let first = self.position(first_center)?;
        let second = self.position(second_center)?;
        let separation = distance(first, second);
        require_component_size(separation, self.precision)?;
        let radius = width * 0.5;
        let direction_u = (second.u - first.u) / separation;
        let direction_v = (second.v - first.v) / separation;
        let normal_u = -direction_v * radius;
        let normal_v = direction_u * radius;
        let positions = [
            SketchPoint2::new(first.u + normal_u, first.v + normal_v),
            SketchPoint2::new(first.u - normal_u, first.v - normal_v),
            SketchPoint2::new(second.u - normal_u, second.v - normal_v),
            SketchPoint2::new(second.u + normal_u, second.v + normal_v),
        ];
        let mut endpoints = Vec::with_capacity(4);
        for (index, position) in positions.into_iter().enumerate() {
            let (rail, endpoint) = match index {
                0 => (1, 1),
                1 => (0, 0),
                2 => (0, 1),
                _ => (1, 0),
            };
            endpoints.push(
                self.add_derived_point(PointOutputRole::RailEndpoint { rail, endpoint }, position)?,
            );
        }
        self.add_arc(
            CurveOutputRole::Cap(0),
            entity_role,
            first_center,
            endpoints[0],
            endpoints[1],
            CurveDirection::CounterClockwise,
        )?;
        self.add_line(
            CurveOutputRole::Rail(0),
            entity_role,
            endpoints[1],
            endpoints[2],
        )?;
        self.add_arc(
            CurveOutputRole::Cap(1),
            entity_role,
            second_center,
            endpoints[2],
            endpoints[3],
            CurveDirection::CounterClockwise,
        )?;
        self.add_line(
            CurveOutputRole::Rail(1),
            entity_role,
            endpoints[3],
            endpoints[0],
        )?;
        Ok(())
    }

    fn add_pattern_curve(
        &mut self,
        instance: u16,
        source: u16,
        entity_role: SketchEntityRole,
        curve: EvaluatedCurve2,
        transform: impl Fn(SketchPoint2) -> SketchPoint2,
    ) -> Result<(), SketchValidationError> {
        let point = |slot| PointOutputRole::PatternPoint {
            instance,
            source,
            point: slot,
        };
        let role = CurveOutputRole::PatternCurve { instance, source };
        match curve {
            EvaluatedCurve2::Line { start, end } => {
                let start = self.add_derived_point(point(0), transform(start))?;
                let end = self.add_derived_point(point(1), transform(end))?;
                self.add_line(role, entity_role, start, end)
            }
            EvaluatedCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            } => {
                let center = self.add_derived_point(point(0), transform(center))?;
                let start = self.add_derived_point(point(1), transform(start))?;
                let end = self.add_derived_point(point(2), transform(end))?;
                self.add_arc(role, entity_role, center, start, end, direction)
            }
            EvaluatedCurve2::Circle {
                center,
                radius,
                direction,
            } => {
                let center = self.add_derived_point(point(0), transform(center))?;
                self.add_circle(role, entity_role, center, radius, direction)
            }
        }
    }

    fn add_trim_fragment(
        &mut self,
        fragment: u16,
        entity_role: SketchEntityRole,
        curve: EvaluatedCurve2,
    ) -> Result<(), SketchValidationError> {
        let point = |slot| PointOutputRole::TrimPoint {
            fragment,
            point: slot,
        };
        let role = CurveOutputRole::TrimFragment(fragment);
        match curve {
            EvaluatedCurve2::Line { start, end } => {
                let start = self.add_derived_point(point(0), start)?;
                let end = self.add_derived_point(point(1), end)?;
                self.add_line(role, entity_role, start, end)
            }
            EvaluatedCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            } => {
                let center = self.add_derived_point(point(0), center)?;
                let start = self.add_derived_point(point(1), start)?;
                let end = self.add_derived_point(point(2), end)?;
                self.add_arc(role, entity_role, center, start, end, direction)
            }
            EvaluatedCurve2::Circle { .. } => {
                // A valid periodic Trim always has at least two limits and its
                // retained spans are proper circular arcs.
                Err(SketchValidationError::InvalidTrimSelection)
            }
        }
    }

    fn retire_curve_budget(&mut self, count: usize) {
        self.retired_curve_budget = count;
    }

    fn finish(self) -> Result<PrimitiveEvaluation, SketchValidationError> {
        if self.curves.len() > MAX_CURVE_EDITS_PER_TRANSACTION {
            return Err(SketchValidationError::ResourceLimit {
                resource: "curve_edits",
                requested: self.curves.len(),
                limit: MAX_CURVE_EDITS_PER_TRANSACTION,
            });
        }
        let requested_curves = self
            .definition
            .active_entities()
            .count()
            .saturating_sub(self.retired_curve_budget)
            .checked_add(self.curves.len())
            .ok_or(SketchValidationError::ArithmeticOverflow)?;
        if requested_curves > MAX_ACTIVE_SKETCH_CURVES {
            return Err(SketchValidationError::ResourceLimit {
                resource: "active_curves",
                requested: requested_curves,
                limit: MAX_ACTIVE_SKETCH_CURVES,
            });
        }
        let requested_points = self
            .definition
            .active_points()
            .count()
            .checked_add(self.points.len())
            .ok_or(SketchValidationError::ArithmeticOverflow)?;
        if requested_points > MAX_ACTIVE_SKETCH_POINTS {
            return Err(SketchValidationError::ResourceLimit {
                resource: "active_points",
                requested: requested_points,
                limit: MAX_ACTIVE_SKETCH_POINTS,
            });
        }
        Ok(PrimitiveEvaluation {
            points: self.points,
            curves: self.curves,
        })
    }
}

fn pattern_sources(
    definition: &SketchDefinition,
    sources: &[SketchEntityId],
) -> Result<Vec<(SketchEntityId, SketchEntityRole, EvaluatedCurve2)>, SketchValidationError> {
    if sources.is_empty() {
        return Err(SketchValidationError::EmptyEntitySelection);
    }
    let mut canonical = sources.to_vec();
    canonical.sort_unstable();
    for pair in canonical.windows(2) {
        if pair[0] == pair[1] {
            return Err(SketchValidationError::DuplicateEntitySelection { entity: pair[0] });
        }
    }
    canonical
        .into_iter()
        .map(|entity| {
            let record = definition
                .entity(entity)
                .filter(|record| record.active)
                .ok_or(SketchValidationError::MissingEntity { entity })?;
            if record.role == SketchEntityRole::Reference {
                return Err(SketchValidationError::UnsupportedPatternSource { entity });
            }
            Ok((entity, record.role, definition.evaluated_curve(entity)?))
        })
        .collect()
}

fn pattern_anchor(sources: &[(SketchEntityId, SketchEntityRole, EvaluatedCurve2)]) -> SketchPoint2 {
    let mut sum_u = 0.0;
    let mut sum_v = 0.0;
    let mut count = 0_u32;
    let mut include = |point: SketchPoint2| {
        sum_u += point.u;
        sum_v += point.v;
        count += 1;
    };
    for (_, _, curve) in sources {
        match *curve {
            EvaluatedCurve2::Line { start, end } => {
                include(start);
                include(end);
            }
            EvaluatedCurve2::CircularArc {
                center, start, end, ..
            } => {
                include(center);
                include(start);
                include(end);
            }
            EvaluatedCurve2::Circle { center, .. } => include(center),
        }
    }
    SketchPoint2::new(sum_u / f64::from(count), sum_v / f64::from(count))
}

fn rotate_about(point: SketchPoint2, center: SketchPoint2, angle: f64) -> SketchPoint2 {
    let delta = point - center;
    let cosine = angle.cos();
    let sine = angle.sin();
    SketchPoint2::new(
        center.u + cosine.mul_add(delta.u, -(sine * delta.v)),
        center.v + sine.mul_add(delta.u, cosine * delta.v),
    )
}

#[derive(Clone, Copy)]
struct CornerLine {
    role: SketchEntityRole,
    start_id: SketchPointId,
    end_id: SketchPointId,
    start: SketchPoint2,
    end: SketchPoint2,
}

#[derive(Clone, Copy)]
struct CornerSelection {
    first: CornerLine,
    second: CornerLine,
    first_common_is_start: bool,
    second_common_is_start: bool,
    corner: SketchPoint2,
    first_other: SketchPoint2,
    second_other: SketchPoint2,
}

fn corner_selection(
    definition: &SketchDefinition,
    first: SketchEntityId,
    second: SketchEntityId,
    precision: PrecisionPolicy,
) -> Result<CornerSelection, SketchValidationError> {
    if first == second {
        return Err(SketchValidationError::InvalidCornerSelection);
    }
    let read_line = |entity| {
        let record = definition
            .entity(entity)
            .filter(|record| record.active)
            .ok_or(SketchValidationError::MissingEntity { entity })?;
        let SketchCurve2::Line { start, end } = record.geometry else {
            return Err(SketchValidationError::InvalidCornerSelection);
        };
        let start_point = definition
            .point(start)
            .filter(|point| point.active)
            .ok_or(SketchValidationError::MissingPoint { point: start })?;
        let end_point = definition
            .point(end)
            .filter(|point| point.active)
            .ok_or(SketchValidationError::MissingPoint { point: end })?;
        Ok(CornerLine {
            role: record.role,
            start_id: start,
            end_id: end,
            start: start_point.evaluated_position,
            end: end_point.evaluated_position,
        })
    };
    let first_line = read_line(first)?;
    let second_line = read_line(second)?;
    if first_line.role != second_line.role || first_line.role == SketchEntityRole::Reference {
        return Err(SketchValidationError::InvalidCornerSelection);
    }
    let candidates = [
        (true, true, first_line.start, second_line.start),
        (true, false, first_line.start, second_line.end),
        (false, true, first_line.end, second_line.start),
        (false, false, first_line.end, second_line.end),
    ];
    let connection_tolerance = precision
        .linear_agreement
        .max(precision.modeling_resolution);
    let mut connected = candidates
        .into_iter()
        .filter(|(_, _, first, second)| distance(*first, *second) <= connection_tolerance);
    let Some((first_common_is_start, second_common_is_start, first_corner, second_corner)) =
        connected.next()
    else {
        return Err(SketchValidationError::InvalidCornerSelection);
    };
    if connected.next().is_some() {
        return Err(SketchValidationError::InvalidCornerSelection);
    }
    let first_other = if first_common_is_start {
        first_line.end
    } else {
        first_line.start
    };
    let second_other = if second_common_is_start {
        second_line.end
    } else {
        second_line.start
    };
    Ok(CornerSelection {
        first: first_line,
        second: second_line,
        first_common_is_start,
        second_common_is_start,
        corner: midpoint(first_corner, second_corner),
        first_other,
        second_other,
    })
}

fn normalized_from(
    origin: SketchPoint2,
    target: SketchPoint2,
) -> Result<(f64, f64, f64), SketchValidationError> {
    let delta_u = target.u - origin.u;
    let delta_v = target.v - origin.v;
    let length = delta_u.hypot(delta_v);
    if !length.is_finite() || length == 0.0 {
        return Err(SketchValidationError::InvalidCornerSelection);
    }
    Ok((delta_u / length, delta_v / length, length))
}

fn add_trimmed_source(
    builder: &mut EvaluationBuilder<'_>,
    selection: CornerSelection,
    source_index: u8,
    tangent: PointBindingDraft,
) -> Result<(), SketchValidationError> {
    let (line, common_is_start) = if source_index == 0 {
        (selection.first, selection.first_common_is_start)
    } else {
        (selection.second, selection.second_common_is_start)
    };
    let other = PointBindingDraft::Existing(if common_is_start {
        line.end_id
    } else {
        line.start_id
    });
    let (start, end) = if common_is_start {
        (tangent, other)
    } else {
        (other, tangent)
    };
    builder.add_line(
        CurveOutputRole::TrimmedSource(source_index),
        line.role,
        start,
        end,
    )
}

fn add_fillet(
    builder: &mut EvaluationBuilder<'_>,
    definition: &SketchDefinition,
    first: SketchEntityId,
    second: SketchEntityId,
    radius: f64,
    precision: PrecisionPolicy,
) -> Result<(), SketchValidationError> {
    let selection = corner_selection(definition, first, second, precision)?;
    let (first_u, first_v, first_length) =
        normalized_from(selection.corner, selection.first_other)?;
    let (second_u, second_v, second_length) =
        normalized_from(selection.corner, selection.second_other)?;
    let cosine = (first_u * second_u + first_v * second_v).clamp(-1.0, 1.0);
    let angle = cosine.acos();
    if angle <= precision.angular_agreement_radians
        || std::f64::consts::PI - angle <= precision.angular_agreement_radians
    {
        return Err(SketchValidationError::InvalidCornerSelection);
    }
    let trim_distance = radius / (angle * 0.5).tan();
    require_component_size(trim_distance, precision)?;
    if trim_distance >= first_length - precision.min_feature_size
        || trim_distance >= second_length - precision.min_feature_size
    {
        return Err(SketchValidationError::CornerDistanceTooLarge);
    }
    let first_tangent_position = SketchPoint2::new(
        selection.corner.u + first_u * trim_distance,
        selection.corner.v + first_v * trim_distance,
    );
    let second_tangent_position = SketchPoint2::new(
        selection.corner.u + second_u * trim_distance,
        selection.corner.v + second_v * trim_distance,
    );
    let bisector_u = first_u + second_u;
    let bisector_v = first_v + second_v;
    let bisector_length = bisector_u.hypot(bisector_v);
    if bisector_length <= precision.angular_agreement_radians {
        return Err(SketchValidationError::InvalidCornerSelection);
    }
    let center_distance = radius / (angle * 0.5).sin();
    let center_position = SketchPoint2::new(
        selection.corner.u + bisector_u / bisector_length * center_distance,
        selection.corner.v + bisector_v / bisector_length * center_distance,
    );
    let first_tangent =
        builder.add_derived_point(PointOutputRole::Tangency(0), first_tangent_position)?;
    let second_tangent =
        builder.add_derived_point(PointOutputRole::Tangency(1), second_tangent_position)?;
    let center = builder.add_derived_point(PointOutputRole::FilletCenter, center_position)?;
    add_trimmed_source(builder, selection, 0, first_tangent)?;
    add_trimmed_source(builder, selection, 1, second_tangent)?;
    let first_radius = first_tangent_position - center_position;
    let second_radius = second_tangent_position - center_position;
    let direction = if first_radius.cross(second_radius) > 0.0 {
        CurveDirection::CounterClockwise
    } else {
        CurveDirection::Clockwise
    };
    builder.add_arc(
        CurveOutputRole::CornerConnector,
        selection.first.role,
        center,
        first_tangent,
        second_tangent,
        direction,
    )
}

#[derive(Clone, Copy)]
enum GeneralSourceBinding {
    Line {
        start: SketchPointId,
        end: SketchPointId,
    },
    Arc {
        center: SketchPointId,
        start: SketchPointId,
        end: SketchPointId,
        direction: CurveDirection,
    },
    Circle {
        center: SketchPointId,
        direction: CurveDirection,
    },
}

#[derive(Clone, Copy)]
struct GeneralFilletSource {
    entity: SketchEntityId,
    role: SketchEntityRole,
    curve: EvaluatedCurve2,
    binding: GeneralSourceBinding,
}

#[derive(Clone, Copy)]
enum OffsetLocus {
    Line {
        origin: SketchPoint2,
        direction: SketchVector2,
    },
    /// `signed_radius` retains which side of the source circle generated the
    /// positive locus radius. It is required to recover the correct tangent
    /// point when the requested fillet is larger than the source radius.
    Circle {
        center: SketchPoint2,
        signed_radius: f64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetainedBranch {
    OpenStart,
    OpenEnd,
    CircleCornerToTangent,
    CircleTangentToCorner,
}

#[derive(Clone, Copy)]
struct GeneralFilletCandidate {
    center: SketchPoint2,
    tangencies: [SketchPoint2; 2],
    retained: [RetainedBranch; 2],
    connector_direction: CurveDirection,
    score: f64,
}

fn add_general_fillet(
    builder: &mut EvaluationBuilder<'_>,
    definition: &SketchDefinition,
    first_entity: SketchEntityId,
    second_entity: SketchEntityId,
    radius: f64,
    hints: FilletBranchHints,
    precision: PrecisionPolicy,
) -> Result<(), SketchValidationError> {
    if first_entity == second_entity {
        return Err(SketchValidationError::InvalidCornerSelection);
    }
    if !hints.first_pick.is_finite()
        || !hints.second_pick.is_finite()
        || !hints.corner_hint.is_finite()
    {
        return Err(SketchValidationError::NonFiniteValue);
    }

    let first = read_general_fillet_source(definition, first_entity)?;
    let second = read_general_fillet_source(definition, second_entity)?;
    if first.role != second.role || first.role == SketchEntityRole::Reference {
        return Err(SketchValidationError::InvalidCornerSelection);
    }
    let sources = [first, second];
    let picks = [hints.first_pick, hints.second_pick];
    let pick_parameters = [
        fillet_hint_parameter(first, hints.first_pick, precision)?,
        fillet_hint_parameter(second, hints.second_pick, precision)?,
    ];
    let (corner, corner_parameters) =
        select_general_fillet_corner(first, second, hints.corner_hint, precision)?;

    let first_loci = offset_loci(first.curve, radius, precision);
    let second_loci = offset_loci(second.curve, radius, precision);
    let mut candidates = Vec::new();
    for first_locus in first_loci {
        for second_locus in second_loci.iter().copied() {
            for center in offset_locus_intersections(first_locus, second_locus, precision) {
                if let Some(candidate) = build_general_fillet_candidate(
                    sources,
                    [first_locus, second_locus],
                    picks,
                    pick_parameters,
                    corner_parameters,
                    corner,
                    center,
                    radius,
                    precision,
                )? {
                    candidates.push(candidate);
                }
            }
        }
    }
    canonicalize_fillet_candidates(&mut candidates, precision);
    if candidates.is_empty() {
        return Err(SketchValidationError::FilletNoBoundedSolution);
    }
    candidates.sort_by(|first, second| {
        first
            .score
            .total_cmp(&second.score)
            .then_with(|| first.center.total_cmp(&second.center))
            .then_with(|| first.tangencies[0].total_cmp(&second.tangencies[0]))
            .then_with(|| first.tangencies[1].total_cmp(&second.tangencies[1]))
    });
    if let Some(second_best) = candidates.get(1) {
        let scale = candidates[0]
            .score
            .sqrt()
            .max(second_best.score.sqrt())
            .max(radius)
            .max(1.0);
        let ambiguity = fillet_linear_tolerance(precision, scale) * 8.0;
        if (candidates[0].score.sqrt() - second_best.score.sqrt()).abs() <= ambiguity {
            return Err(SketchValidationError::FilletAmbiguousSolution);
        }
    }
    let selected = candidates[0];

    let first_tangent =
        builder.add_derived_point(PointOutputRole::Tangency(0), selected.tangencies[0])?;
    let second_tangent =
        builder.add_derived_point(PointOutputRole::Tangency(1), selected.tangencies[1])?;
    let center = builder.add_derived_point(PointOutputRole::FilletCenter, selected.center)?;
    let corner_binding = sources
        .iter()
        .any(|source| matches!(source.binding, GeneralSourceBinding::Circle { .. }))
        .then(|| builder.add_derived_point(PointOutputRole::FilletCorner, corner))
        .transpose()?;
    add_general_trimmed_source(
        builder,
        first,
        0,
        selected.retained[0],
        first_tangent,
        corner_binding,
    )?;
    add_general_trimmed_source(
        builder,
        second,
        1,
        selected.retained[1],
        second_tangent,
        corner_binding,
    )?;
    builder.add_arc(
        CurveOutputRole::CornerConnector,
        first.role,
        center,
        first_tangent,
        second_tangent,
        selected.connector_direction,
    )
}

fn read_general_fillet_source(
    definition: &SketchDefinition,
    entity: SketchEntityId,
) -> Result<GeneralFilletSource, SketchValidationError> {
    let record = definition
        .entity(entity)
        .filter(|record| record.active)
        .ok_or(SketchValidationError::MissingEntity { entity })?;
    let binding = match record.geometry {
        SketchCurve2::Line { start, end } => GeneralSourceBinding::Line { start, end },
        SketchCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } => GeneralSourceBinding::Arc {
            center,
            start,
            end,
            direction,
        },
        SketchCurve2::Circle {
            center, direction, ..
        } => GeneralSourceBinding::Circle { center, direction },
    };
    Ok(GeneralFilletSource {
        entity,
        role: record.role,
        curve: definition.evaluated_curve(entity)?,
        binding,
    })
}

fn select_general_fillet_corner(
    first: GeneralFilletSource,
    second: GeneralFilletSource,
    hint: SketchPoint2,
    precision: PrecisionPolicy,
) -> Result<(SketchPoint2, [f64; 2]), SketchValidationError> {
    let CurveIntersections::Points { mut intersections } =
        intersect_curves(first.curve, second.curve, &precision)
    else {
        return Err(SketchValidationError::FilletNoBoundedSolution);
    };
    intersections.retain(|intersection| !intersection.is_tangent);
    intersections.sort_by(|first, second| {
        first
            .point
            .distance_squared(hint)
            .total_cmp(&second.point.distance_squared(hint))
            .then_with(|| first.first_parameter.total_cmp(&second.first_parameter))
            .then_with(|| first.second_parameter.total_cmp(&second.second_parameter))
            .then_with(|| first.point.total_cmp(&second.point))
    });
    let Some(selected) = intersections.first().copied() else {
        return Err(SketchValidationError::FilletNoBoundedSolution);
    };
    if let Some(other) = intersections.get(1) {
        let scale = selected
            .point
            .distance(hint)
            .max(other.point.distance(hint))
            .max(first.curve.arc_length())
            .max(second.curve.arc_length())
            .max(1.0);
        if (selected.point.distance(hint) - other.point.distance(hint)).abs()
            <= fillet_linear_tolerance(precision, scale) * 8.0
        {
            return Err(SketchValidationError::FilletAmbiguousSolution);
        }
    }
    Ok((
        selected.point,
        [selected.first_parameter, selected.second_parameter],
    ))
}

fn fillet_hint_parameter(
    source: GeneralFilletSource,
    pick: SketchPoint2,
    precision: PrecisionPolicy,
) -> Result<f64, SketchValidationError> {
    let parameter = source.curve.closest_parameter(pick);
    let evaluated = source
        .curve
        .evaluate(if source.curve.is_periodic() && parameter == 1.0 {
            0.0
        } else {
            parameter
        })
        .map_err(|_| SketchValidationError::FilletHintOffSource {
            entity: source.entity,
        })?;
    let scale = source
        .curve
        .arc_length()
        .max(pick.u.abs())
        .max(pick.v.abs());
    if evaluated.distance(pick) > fillet_linear_tolerance(precision, scale) * 8.0 {
        return Err(SketchValidationError::FilletHintOffSource {
            entity: source.entity,
        });
    }
    Ok(parameter)
}

fn offset_loci(
    curve: EvaluatedCurve2,
    radius: f64,
    precision: PrecisionPolicy,
) -> Vec<OffsetLocus> {
    match curve {
        EvaluatedCurve2::Line { start, end } => {
            let Some(direction) = (end - start).normalized() else {
                return Vec::new();
            };
            let normal = direction.left_normal() * radius;
            vec![
                OffsetLocus::Line {
                    origin: start + normal,
                    direction,
                },
                OffsetLocus::Line {
                    origin: start + -normal,
                    direction,
                },
            ]
        }
        EvaluatedCurve2::CircularArc { center, start, .. } => {
            circular_offset_loci(center, center.distance(start), radius, precision)
        }
        EvaluatedCurve2::Circle {
            center,
            radius: source_radius,
            ..
        } => circular_offset_loci(center, source_radius, radius, precision),
    }
}

fn circular_offset_loci(
    center: SketchPoint2,
    source_radius: f64,
    fillet_radius: f64,
    precision: PrecisionPolicy,
) -> Vec<OffsetLocus> {
    [source_radius + fillet_radius, source_radius - fillet_radius]
        .into_iter()
        .filter(|signed_radius| {
            signed_radius.is_finite() && signed_radius.abs() >= precision.min_feature_size
        })
        .map(|signed_radius| OffsetLocus::Circle {
            center,
            signed_radius,
        })
        .collect()
}

fn offset_locus_intersections(
    first: OffsetLocus,
    second: OffsetLocus,
    precision: PrecisionPolicy,
) -> Vec<SketchPoint2> {
    match (first, second) {
        (
            OffsetLocus::Line {
                origin: first_origin,
                direction: first_direction,
            },
            OffsetLocus::Line {
                origin: second_origin,
                direction: second_direction,
            },
        ) => {
            let denominator = first_direction.cross(second_direction);
            if denominator.abs() <= precision.angular_agreement_radians.max(f64::EPSILON * 64.0) {
                Vec::new()
            } else {
                let parameter =
                    (second_origin - first_origin).cross(second_direction) / denominator;
                vec![first_origin + first_direction * parameter]
            }
        }
        (
            OffsetLocus::Line { origin, direction },
            OffsetLocus::Circle {
                center,
                signed_radius,
            },
        )
        | (
            OffsetLocus::Circle {
                center,
                signed_radius,
            },
            OffsetLocus::Line { origin, direction },
        ) => infinite_line_circle_intersections(
            origin,
            direction,
            center,
            signed_radius.abs(),
            precision,
        ),
        (
            OffsetLocus::Circle {
                center: first_center,
                signed_radius: first_radius,
            },
            OffsetLocus::Circle {
                center: second_center,
                signed_radius: second_radius,
            },
        ) => circle_circle_locus_intersections(
            first_center,
            first_radius.abs(),
            second_center,
            second_radius.abs(),
            precision,
        ),
    }
}

fn infinite_line_circle_intersections(
    origin: SketchPoint2,
    direction: SketchVector2,
    center: SketchPoint2,
    radius: f64,
    precision: PrecisionPolicy,
) -> Vec<SketchPoint2> {
    let offset = origin - center;
    let along = offset.dot(direction);
    let discriminant = radius.mul_add(radius, -(offset.length_squared() - along * along));
    let scale = radius
        .max(offset.length())
        .max(origin.u.abs())
        .max(origin.v.abs());
    let tolerance = fillet_linear_tolerance(precision, scale) * radius.max(1.0) * 8.0;
    if discriminant < -tolerance {
        return Vec::new();
    }
    let root = discriminant.max(0.0).sqrt();
    let base_parameter = -along;
    let mut points = vec![origin + direction * (base_parameter - root)];
    if root > fillet_linear_tolerance(precision, scale) {
        points.push(origin + direction * (base_parameter + root));
    }
    points.sort_by(SketchPoint2::total_cmp);
    points
}

fn circle_circle_locus_intersections(
    first_center: SketchPoint2,
    first_radius: f64,
    second_center: SketchPoint2,
    second_radius: f64,
    precision: PrecisionPolicy,
) -> Vec<SketchPoint2> {
    let delta = second_center - first_center;
    let separation = delta.length();
    let scale = first_radius
        .max(second_radius)
        .max(separation)
        .max(first_center.u.abs())
        .max(first_center.v.abs())
        .max(second_center.u.abs())
        .max(second_center.v.abs());
    let tolerance = fillet_linear_tolerance(precision, scale) * 8.0;
    if separation <= tolerance
        || separation > first_radius + second_radius + tolerance
        || separation < (first_radius - second_radius).abs() - tolerance
    {
        return Vec::new();
    }
    let along = (first_radius * first_radius - second_radius * second_radius
        + separation * separation)
        / (2.0 * separation);
    let height_squared = first_radius.mul_add(first_radius, -(along * along));
    let squared_tolerance = tolerance * first_radius.max(1.0) * 8.0;
    if height_squared < -squared_tolerance {
        return Vec::new();
    }
    let unit = delta / separation;
    let base = first_center + unit * along;
    let height = height_squared.max(0.0).sqrt();
    let mut points = vec![base];
    if height > tolerance {
        let perpendicular = unit.left_normal() * height;
        points[0] = base + -perpendicular;
        points.push(base + perpendicular);
    }
    points.sort_by(SketchPoint2::total_cmp);
    points
}

#[allow(clippy::too_many_arguments)]
fn build_general_fillet_candidate(
    sources: [GeneralFilletSource; 2],
    loci: [OffsetLocus; 2],
    picks: [SketchPoint2; 2],
    pick_parameters: [f64; 2],
    corner_parameters: [f64; 2],
    corner: SketchPoint2,
    center: SketchPoint2,
    radius: f64,
    precision: PrecisionPolicy,
) -> Result<Option<GeneralFilletCandidate>, SketchValidationError> {
    let Some(first_tangent) = tangent_on_source(sources[0], loci[0], center, precision) else {
        return Ok(None);
    };
    let Some(second_tangent) = tangent_on_source(sources[1], loci[1], center, precision) else {
        return Ok(None);
    };
    let Some(first_retained) = retained_branch(
        sources[0].curve,
        corner_parameters[0],
        first_tangent.1,
        pick_parameters[0],
        precision,
    ) else {
        return Ok(None);
    };
    let Some(second_retained) = retained_branch(
        sources[1].curve,
        corner_parameters[1],
        second_tangent.1,
        pick_parameters[1],
        precision,
    ) else {
        return Ok(None);
    };
    let connector_direction =
        connector_direction(center, first_tangent.0, second_tangent.0, radius, precision)?;
    if !prove_fillet_tangency(
        sources,
        [first_tangent, second_tangent],
        center,
        radius,
        connector_direction,
        precision,
    ) {
        return Err(SketchValidationError::FilletTangencyFailure);
    }
    let score = first_tangent.0.distance_squared(picks[0])
        + second_tangent.0.distance_squared(picks[1])
        + center.distance_squared(corner);
    Ok(Some(GeneralFilletCandidate {
        center,
        tangencies: [first_tangent.0, second_tangent.0],
        retained: [first_retained, second_retained],
        connector_direction,
        score,
    }))
}

fn tangent_on_source(
    source: GeneralFilletSource,
    locus: OffsetLocus,
    fillet_center: SketchPoint2,
    precision: PrecisionPolicy,
) -> Option<(SketchPoint2, f64)> {
    let scale = source
        .curve
        .arc_length()
        .max(fillet_center.u.abs())
        .max(fillet_center.v.abs());
    let tolerance = fillet_linear_tolerance(precision, scale) * 8.0;
    match (source.curve, locus) {
        (EvaluatedCurve2::Line { start, end }, OffsetLocus::Line { direction, .. }) => {
            let length = start.distance(end);
            let parameter = (fillet_center - start).dot(direction) / length;
            let parameter_tolerance = tolerance / length.max(precision.min_feature_size);
            if parameter < -parameter_tolerance || parameter > 1.0 + parameter_tolerance {
                return None;
            }
            let parameter = parameter.clamp(0.0, 1.0);
            let tangent = start + (end - start) * parameter;
            Some((tangent, parameter))
        }
        (
            EvaluatedCurve2::CircularArc { center, .. }
            | EvaluatedCurve2::Circle {
                center, radius: _, ..
            },
            OffsetLocus::Circle { signed_radius, .. },
        ) => {
            let source_radius = source.curve.radius()?;
            let delta = fillet_center - center;
            let separation = delta.length();
            if separation <= tolerance || (separation - signed_radius.abs()).abs() > tolerance {
                return None;
            }
            let tangent = center + delta / separation * (source_radius * signed_radius.signum());
            let parameter = source.curve.closest_parameter(tangent);
            let evaluated = source.curve.evaluate(parameter).ok()?;
            if evaluated.distance(tangent) > tolerance {
                return None;
            }
            Some((tangent, parameter))
        }
        _ => None,
    }
}

fn retained_branch(
    curve: EvaluatedCurve2,
    corner_parameter: f64,
    tangent_parameter: f64,
    pick_parameter: f64,
    precision: PrecisionPolicy,
) -> Option<RetainedBranch> {
    let total_length = curve.arc_length();
    let parameter_tolerance = precision
        .parameter_resolution
        .max(precision.min_feature_size / total_length.max(precision.min_feature_size));
    if curve.is_periodic() {
        let forward_span = (tangent_parameter - corner_parameter).rem_euclid(1.0);
        let pick_from_corner = (pick_parameter - corner_parameter).rem_euclid(1.0);
        if forward_span <= parameter_tolerance
            || 1.0 - forward_span <= parameter_tolerance
            || pick_from_corner <= parameter_tolerance
            || 1.0 - pick_from_corner <= parameter_tolerance
            || (pick_from_corner - forward_span).abs() <= parameter_tolerance
        {
            return None;
        }
        if pick_from_corner < forward_span {
            Some(RetainedBranch::CircleCornerToTangent)
        } else {
            Some(RetainedBranch::CircleTangentToCorner)
        }
    } else if pick_parameter < corner_parameter - parameter_tolerance {
        (tangent_parameter < corner_parameter - parameter_tolerance
            && pick_parameter <= tangent_parameter + parameter_tolerance
            && tangent_parameter * total_length >= precision.min_feature_size
            && (corner_parameter - tangent_parameter) * total_length >= precision.min_feature_size)
            .then_some(RetainedBranch::OpenStart)
    } else if pick_parameter > corner_parameter + parameter_tolerance {
        (tangent_parameter > corner_parameter + parameter_tolerance
            && pick_parameter + parameter_tolerance >= tangent_parameter
            && (1.0 - tangent_parameter) * total_length >= precision.min_feature_size
            && (tangent_parameter - corner_parameter) * total_length >= precision.min_feature_size)
            .then_some(RetainedBranch::OpenEnd)
    } else {
        None
    }
}

fn connector_direction(
    center: SketchPoint2,
    first: SketchPoint2,
    second: SketchPoint2,
    radius: f64,
    precision: PrecisionPolicy,
) -> Result<CurveDirection, SketchValidationError> {
    let first_radius = first - center;
    let second_radius = second - center;
    let cross = first_radius.cross(second_radius);
    let normalized_cross = cross / radius.powi(2);
    let angular_tolerance = precision
        .angular_agreement_radians
        .max(precision.modeling_resolution / radius.max(precision.min_feature_size));
    if normalized_cross.abs() <= angular_tolerance {
        if first_radius.dot(second_radius) < 0.0 {
            return Err(SketchValidationError::FilletAmbiguousSolution);
        }
        return Err(SketchValidationError::FilletNoBoundedSolution);
    }
    Ok(if cross > 0.0 {
        CurveDirection::CounterClockwise
    } else {
        CurveDirection::Clockwise
    })
}

fn prove_fillet_tangency(
    sources: [GeneralFilletSource; 2],
    tangencies: [(SketchPoint2, f64); 2],
    center: SketchPoint2,
    radius: f64,
    connector_direction: CurveDirection,
    precision: PrecisionPolicy,
) -> bool {
    let direction_sign = match connector_direction {
        CurveDirection::CounterClockwise => 1.0,
        CurveDirection::Clockwise => -1.0,
    };
    let linear_tolerance = fillet_linear_tolerance(precision, radius.max(1.0)) * 16.0;
    let angular_tolerance = precision
        .angular_agreement_radians
        .max(precision.modeling_resolution / radius.max(precision.min_feature_size))
        * 16.0;
    sources
        .into_iter()
        .zip(tangencies)
        .all(|(source, (point, parameter))| {
            if (center.distance(point) - radius).abs() > linear_tolerance {
                return false;
            }
            let Ok(source_tangent) = source.curve.tangent(parameter) else {
                return false;
            };
            let connector_tangent = (point - center).left_normal() * direction_sign;
            let denominator = source_tangent.length() * connector_tangent.length();
            denominator > 0.0
                && source_tangent.cross(connector_tangent).abs() / denominator <= angular_tolerance
        })
}

fn canonicalize_fillet_candidates(
    candidates: &mut Vec<GeneralFilletCandidate>,
    precision: PrecisionPolicy,
) {
    candidates.sort_by(|first, second| {
        first
            .center
            .total_cmp(&second.center)
            .then_with(|| first.tangencies[0].total_cmp(&second.tangencies[0]))
            .then_with(|| first.tangencies[1].total_cmp(&second.tangencies[1]))
    });
    let scale = candidates
        .iter()
        .map(|candidate| {
            candidate
                .center
                .u
                .abs()
                .max(candidate.center.v.abs())
                .max(1.0)
        })
        .fold(1.0_f64, f64::max);
    let tolerance = fillet_linear_tolerance(precision, scale) * 8.0;
    candidates.dedup_by(|first, second| {
        first.center.distance(second.center) <= tolerance
            && first.tangencies[0].distance(second.tangencies[0]) <= tolerance
            && first.tangencies[1].distance(second.tangencies[1]) <= tolerance
            && first.retained == second.retained
    });
}

fn add_general_trimmed_source(
    builder: &mut EvaluationBuilder<'_>,
    source: GeneralFilletSource,
    source_index: u8,
    retained: RetainedBranch,
    tangent: PointBindingDraft,
    corner: Option<PointBindingDraft>,
) -> Result<(), SketchValidationError> {
    let role = CurveOutputRole::TrimmedSource(source_index);
    match (source.binding, retained) {
        (GeneralSourceBinding::Line { start, .. }, RetainedBranch::OpenStart) => builder.add_line(
            role,
            source.role,
            PointBindingDraft::Existing(start),
            tangent,
        ),
        (GeneralSourceBinding::Line { end, .. }, RetainedBranch::OpenEnd) => {
            builder.add_line(role, source.role, tangent, PointBindingDraft::Existing(end))
        }
        (
            GeneralSourceBinding::Arc {
                center,
                start,
                direction,
                ..
            },
            RetainedBranch::OpenStart,
        ) => builder.add_arc(
            role,
            source.role,
            PointBindingDraft::Existing(center),
            PointBindingDraft::Existing(start),
            tangent,
            direction,
        ),
        (
            GeneralSourceBinding::Arc {
                center,
                end,
                direction,
                ..
            },
            RetainedBranch::OpenEnd,
        ) => builder.add_arc(
            role,
            source.role,
            PointBindingDraft::Existing(center),
            tangent,
            PointBindingDraft::Existing(end),
            direction,
        ),
        (
            GeneralSourceBinding::Circle { center, direction },
            RetainedBranch::CircleCornerToTangent,
        ) => builder.add_arc(
            role,
            source.role,
            PointBindingDraft::Existing(center),
            corner.ok_or(SketchValidationError::FilletTangencyFailure)?,
            tangent,
            direction,
        ),
        (
            GeneralSourceBinding::Circle { center, direction },
            RetainedBranch::CircleTangentToCorner,
        ) => builder.add_arc(
            role,
            source.role,
            PointBindingDraft::Existing(center),
            tangent,
            corner.ok_or(SketchValidationError::FilletTangencyFailure)?,
            direction,
        ),
        _ => Err(SketchValidationError::FilletTangencyFailure),
    }
}

fn fillet_linear_tolerance(precision: PrecisionPolicy, scale: f64) -> f64 {
    precision
        .modeling_resolution
        .max(precision.linear_agreement)
        .max(f64::EPSILON * scale.max(1.0) * 128.0)
}

fn add_chamfer(
    builder: &mut EvaluationBuilder<'_>,
    definition: &SketchDefinition,
    first: SketchEntityId,
    second: SketchEntityId,
    first_distance: f64,
    second_distance: f64,
    precision: PrecisionPolicy,
) -> Result<(), SketchValidationError> {
    let selection = corner_selection(definition, first, second, precision)?;
    let (first_u, first_v, first_length) =
        normalized_from(selection.corner, selection.first_other)?;
    let (second_u, second_v, second_length) =
        normalized_from(selection.corner, selection.second_other)?;
    let cosine = (first_u * second_u + first_v * second_v).clamp(-1.0, 1.0);
    if 1.0 - cosine.abs() <= precision.angular_agreement_radians {
        return Err(SketchValidationError::InvalidCornerSelection);
    }
    if first_distance >= first_length - precision.min_feature_size
        || second_distance >= second_length - precision.min_feature_size
    {
        return Err(SketchValidationError::CornerDistanceTooLarge);
    }
    let first_trim = builder.add_derived_point(
        PointOutputRole::Tangency(0),
        SketchPoint2::new(
            selection.corner.u + first_u * first_distance,
            selection.corner.v + first_v * first_distance,
        ),
    )?;
    let second_trim = builder.add_derived_point(
        PointOutputRole::Tangency(1),
        SketchPoint2::new(
            selection.corner.u + second_u * second_distance,
            selection.corner.v + second_v * second_distance,
        ),
    )?;
    add_trimmed_source(builder, selection, 0, first_trim)?;
    add_trimmed_source(builder, selection, 1, second_trim)?;
    builder.add_line(
        CurveOutputRole::CornerConnector,
        selection.first.role,
        first_trim,
        second_trim,
    )
}

pub(crate) fn instantiate_evaluation(
    definition: &mut SketchDefinition,
    operation_id: SketchOperationId,
    recipe: SketchRecipe,
    points: &[PointOutputDraft],
    curves: &[CurveOutputDraft],
) -> Result<SketchOperationRecord, SketchValidationError> {
    let mut point_ids = BTreeMap::new();
    let mut outputs = BTreeMap::new();
    for point in points {
        let id = definition.allocate_point()?;
        if point_ids.insert(point.role, id).is_some() {
            return Err(SketchValidationError::DuplicateOutputRole);
        }
        if outputs
            .insert(OutputRole::Point(point.role), SketchOutputRef::Point(id))
            .is_some()
        {
            return Err(SketchValidationError::DuplicateOutputRole);
        }
        definition.insert_point(SketchPointRecord {
            id,
            owner: SketchOutputOwner {
                operation: operation_id,
                role: point.role,
            },
            evaluated_position: point.position,
            active: true,
        });
    }

    for curve in curves {
        let id = definition.allocate_entity()?;
        let geometry = instantiate_curve(curve.geometry, &point_ids)?;
        if outputs
            .insert(OutputRole::Curve(curve.role), SketchOutputRef::Curve(id))
            .is_some()
        {
            return Err(SketchValidationError::DuplicateOutputRole);
        }
        definition.insert_entity(SketchEntityRecord {
            id,
            role: curve.entity_role,
            geometry,
            provenance: CurveProvenance {
                operation: operation_id,
                role: curve.role,
            },
            visible: true,
            active: true,
            superseded_by: None,
        });
    }

    Ok(SketchOperationRecord {
        id: operation_id,
        recipe,
        outputs,
        active: true,
    })
}

pub(crate) fn instantiate_curve(
    curve: CurveDraft2,
    points: &BTreeMap<PointOutputRole, SketchPointId>,
) -> Result<SketchCurve2, SketchValidationError> {
    let resolve = |binding| match binding {
        PointBindingDraft::Existing(id) => Ok(id),
        PointBindingDraft::Output(role) => points
            .get(&role)
            .copied()
            .ok_or(SketchValidationError::DuplicateOutputRole),
    };
    Ok(match curve {
        CurveDraft2::Line { start, end } => SketchCurve2::Line {
            start: resolve(start)?,
            end: resolve(end)?,
        },
        CurveDraft2::CircularArc {
            center,
            start,
            end,
            direction,
        } => SketchCurve2::CircularArc {
            center: resolve(center)?,
            start: resolve(start)?,
            end: resolve(end)?,
            direction,
        },
        CurveDraft2::Circle {
            center,
            radius,
            direction,
        } => SketchCurve2::Circle {
            center: resolve(center)?,
            radius,
            direction,
        },
    })
}

fn resolve_length(
    value: SketchValue<Length>,
    inputs: &SketchInputValues,
) -> Result<f64, SketchValidationError> {
    let value = value.resolve(inputs)?.get();
    if !value.is_finite() {
        return Err(SketchValidationError::NonFiniteValue);
    }
    if value <= 0.0 {
        return Err(SketchValidationError::FeatureTooSmall {
            operation: placeholder_operation(),
        });
    }
    Ok(value)
}

fn resolve_signed(
    value: SketchValue<SignedLength>,
    inputs: &SketchInputValues,
) -> Result<f64, SketchValidationError> {
    let value = value.resolve(inputs)?.get();
    if !value.is_finite() {
        return Err(SketchValidationError::NonFiniteValue);
    }
    Ok(value)
}

fn resolve_angle(
    value: SketchValue<Angle>,
    inputs: &SketchInputValues,
) -> Result<f64, SketchValidationError> {
    let value = value.resolve(inputs)?.get();
    if !value.is_finite() {
        return Err(SketchValidationError::NonFiniteValue);
    }
    Ok(value)
}

fn resolve_sides(
    value: SketchValue<Integer>,
    inputs: &SketchInputValues,
) -> Result<u16, SketchValidationError> {
    let count = value.resolve(inputs)?.get();
    if !(MIN_POLYGON_SIDES..=MAX_POLYGON_SIDES).contains(&count) {
        return Err(SketchValidationError::PolygonSideCount { count });
    }
    Ok(count)
}

fn resolve_pattern_count(
    value: SketchValue<Integer>,
    inputs: &SketchInputValues,
    minimum: u16,
) -> Result<u16, SketchValidationError> {
    let count = value.resolve(inputs)?.get();
    if !(minimum..=MAX_PATTERN_INSTANCES).contains(&count) {
        return Err(SketchValidationError::PatternCount { count, minimum });
    }
    Ok(count)
}

fn require_component_size(
    value: f64,
    precision: PrecisionPolicy,
) -> Result<(), SketchValidationError> {
    if !value.is_finite() {
        return Err(SketchValidationError::NonFiniteValue);
    }
    if value.abs() < precision.min_feature_size {
        return Err(SketchValidationError::FeatureTooSmall {
            operation: placeholder_operation(),
        });
    }
    Ok(())
}

fn validate_position(
    point: SketchPoint2,
    precision: PrecisionPolicy,
) -> Result<(), SketchValidationError> {
    if !point.is_finite() {
        return Err(SketchValidationError::NonFiniteValue);
    }
    if point.u.abs() > precision.max_abs_coordinate || point.v.abs() > precision.max_abs_coordinate
    {
        return Err(SketchValidationError::CoordinateOutOfBounds {
            max_abs_coordinate: precision.max_abs_coordinate,
        });
    }
    Ok(())
}

fn distance(first: SketchPoint2, second: SketchPoint2) -> f64 {
    (second.u - first.u).hypot(second.v - first.v)
}

fn midpoint(first: SketchPoint2, second: SketchPoint2) -> SketchPoint2 {
    SketchPoint2::new((first.u + second.u) * 0.5, (first.v + second.v) * 0.5)
}

fn index_u16(index: usize) -> Result<u16, SketchValidationError> {
    u16::try_from(index).map_err(|_| SketchValidationError::ArithmeticOverflow)
}

fn import_point(
    builder: &mut EvaluationBuilder<'_>,
    imported: &mut BTreeMap<(u64, u64), PointBindingDraft>,
    next_index: &mut usize,
    point: SketchPoint2,
) -> Result<PointBindingDraft, SketchValidationError> {
    let canonical_bits = |value: f64| if value == 0.0 { 0 } else { value.to_bits() };
    let key = (canonical_bits(point.u), canonical_bits(point.v));
    if let Some(binding) = imported.get(&key) {
        return Ok(*binding);
    }
    let role = PointOutputRole::ImportedPoint(index_u16(*next_index)?);
    *next_index = next_index
        .checked_add(1)
        .ok_or(SketchValidationError::ArithmeticOverflow)?;
    let binding = builder.add_derived_point(role, point)?;
    imported.insert(key, binding);
    Ok(binding)
}

const fn protocol_direction(direction: ArcDirection) -> CurveDirection {
    match direction {
        ArcDirection::CounterClockwise => CurveDirection::CounterClockwise,
        ArcDirection::Clockwise => CurveDirection::Clockwise,
    }
}

fn placeholder_operation() -> SketchOperationId {
    SketchOperationId::new(1).expect("one is a valid non-zero operation ID")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(u: f64, v: f64) -> PointInput {
        PointInput::Position(SketchPoint2::new(u, v))
    }

    fn length(value: f64) -> SketchValue<Length> {
        SketchValue::Literal(Length::new(value).expect("valid length"))
    }

    fn signed(value: f64) -> SketchValue<SignedLength> {
        SketchValue::Literal(SignedLength::new(value).expect("valid length"))
    }

    fn angle(value: f64) -> SketchValue<Angle> {
        SketchValue::Literal(Angle::radians(value).expect("valid angle"))
    }

    #[test]
    fn rectangle_outputs_four_connected_counter_clockwise_sides() {
        let recipe = SketchRecipe::TwoPointRectangle {
            first_corner: point(5.0, 4.0),
            width: signed(-3.0),
            height: signed(2.0),
        };
        let evaluation = evaluate_recipe(
            &SketchDefinition::new(),
            &recipe,
            &SketchInputValues::default(),
            PrecisionPolicy::default(),
        )
        .expect("rectangle");
        assert_eq!(evaluation.points.len(), 5);
        assert!(evaluation.points.iter().any(|p| p.role == PointOutputRole::Center));
        assert_eq!(evaluation.curves.len(), 4);
        let positions: BTreeMap<_, _> = evaluation
            .points
            .iter()
            .map(|point| (point.role, point.position))
            .collect();
        let mut twice_area = 0.0;
        for curve in &evaluation.curves {
            let CurveDraft2::Line { start, end } = curve.geometry else {
                panic!("rectangle edge must be a line")
            };
            let get = |binding| match binding {
                PointBindingDraft::Output(role) => positions[&role],
                PointBindingDraft::Existing(_) => panic!("all corners are new"),
            };
            let a = get(start);
            let b = get(end);
            twice_area += a.u * b.v - b.u * a.v;
        }
        assert!(twice_area > 0.0);
    }

    #[test]
    fn inner_polygon_uses_apothem_and_shared_vertices() {
        let recipe = SketchRecipe::InnerDiameterPolygon {
            center: point(0.0, 0.0),
            inner_diameter: length(10.0),
            sides: SketchValue::Literal(Integer::new(6)),
            rotation: angle(0.0),
        };
        let evaluation = evaluate_recipe(
            &SketchDefinition::new(),
            &recipe,
            &SketchInputValues::default(),
            PrecisionPolicy::default(),
        )
        .expect("hexagon");
        assert_eq!(evaluation.curves.len(), 6);
        assert_eq!(evaluation.points.len(), 7);
        let expected_radius = 5.0 / (std::f64::consts::PI / 6.0).cos();
        let first = evaluation
            .points
            .iter()
            .find(|point| point.role == PointOutputRole::Vertex(0))
            .expect("first vertex");
        assert!((first.position.u - expected_radius).abs() < 1.0e-12);
    }

    #[test]
    fn slot_is_two_lines_and_two_exact_semicircular_arcs() {
        let recipe = SketchRecipe::TwoPointSlot {
            first_cap_center: point(-4.0, 0.0),
            second_cap_center: point(4.0, 0.0),
            width: length(2.0),
        };
        let evaluation = evaluate_recipe(
            &SketchDefinition::new(),
            &recipe,
            &SketchInputValues::default(),
            PrecisionPolicy::default(),
        )
        .expect("slot");
        assert_eq!(evaluation.points.len(), 7);
        assert!(evaluation.points.iter().any(|p| p.role == PointOutputRole::Center));
        assert_eq!(evaluation.curves.len(), 4);
        assert_eq!(
            evaluation
                .curves
                .iter()
                .filter(|curve| matches!(curve.geometry, CurveDraft2::CircularArc { .. }))
                .count(),
            2
        );
    }
}
