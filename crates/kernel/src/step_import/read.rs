//! Reading STEP geometry entities into the kernel's carriers and curves.
//!
//! Everything here answers "what does entity `#n` describe", in millimetres
//! and radians, oriented the way the kernel's builders orient the same
//! carrier. Nothing here decides topology: that is `conform.rs`.

use artificer_step::{Entity, Graph, Value};

use crate::bspline::{SplineCurve3, SplineError, SplineSurface};
use crate::topology::{
    Cone, Curve3, Cylinder, ParameterRange, Plane, Point3, Sphere, Surface, Torus, Vector3,
};

/// Why a face, an edge, a shell or the whole file could not be read into the
/// kernel's vocabulary, named by code and by the entity it names.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Refusal {
    pub(crate) code: &'static str,
    pub(crate) entity: Option<u64>,
    pub(crate) message: String,
    /// A measured quantity and the bound it exceeded, in millimetres.
    pub(crate) measured: Option<(f64, f64)>,
}

impl Refusal {
    pub(crate) fn new(code: &'static str, entity: Option<u64>, message: impl Into<String>) -> Self {
        Self {
            code,
            entity,
            message: message.into(),
            measured: None,
        }
    }

    pub(crate) fn with_measure(mut self, measured: f64, allowed: f64) -> Self {
        self.measured = Some((measured, allowed));
        self
    }

    /// The message with the entity named in front of it, as reports print.
    pub(crate) fn text(&self) -> String {
        match self.entity {
            Some(id) => format!("#{id}: {}", self.message),
            None => self.message.clone(),
        }
    }
}

pub(crate) use crate::step_import::codes::{
    BSPLINE_DEGREE_UNSUPPORTED, BSPLINE_KNOTS_INVALID, BSPLINE_UNCLAMPED_UNSUPPORTED,
    ENTITY_UNSUPPORTED, FACE_UNSUPPORTED, GAP_EXCEEDS_TOLERANCE, RATIONAL_APPROXIMATED,
    RATIONAL_UNSUPPORTED, SHELL_OPEN,
};

/// A right-handed frame read from an `AXIS2_PLACEMENT_3D`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Frame3 {
    pub(crate) origin: Point3,
    pub(crate) x: Vector3,
    pub(crate) y: Vector3,
    pub(crate) z: Vector3,
}

/// A curve as the file defines it, before an edge bounds it.
#[derive(Clone, Debug)]
pub(crate) enum CurveGeometry {
    /// An unbounded line through `point` along the unit `direction`.
    Line { point: Point3, direction: Vector3 },
    /// `center + r(cos t·u + sin t·v)`, `u ⟂ v` unit.
    Circle {
        center: Point3,
        u: Vector3,
        v: Vector3,
        radius: f64,
    },
    /// `center + a cos t·u + b sin t·v`, `u ⟂ v` unit, `a ≥ b`.
    Ellipse {
        center: Point3,
        u: Vector3,
        v: Vector3,
        major: f64,
        minor: f64,
    },
    /// A non-rational clamped B-spline in the kernel's store.
    Bspline { curve: SplineCurve3 },
    /// A rational B-spline the kernel cannot hold: its data, kept so an edge
    /// that is really a circle or a line can still be recognised from
    /// samples of it. The control points are homogeneous, `[wx, wy, wz, w]`.
    Rational {
        degree: usize,
        knots: Vec<f64>,
        points: Vec<[f64; 4]>,
    },
}

impl CurveGeometry {
    /// The point of a rational B-spline at `t`, by de Boor's recursion on
    /// the homogeneous control points.
    pub(crate) fn rational_point(
        degree: usize,
        knots: &[f64],
        points: &[[f64; 4]],
        t: f64,
    ) -> Point3 {
        let last = points.len() - 1;
        let span = (degree..=last)
            .rev()
            .find(|&k| knots[k] <= t && knots[k] < knots[k + 1])
            .unwrap_or(degree);
        let mut local: Vec<[f64; 4]> = (0..=degree).map(|j| points[span - degree + j]).collect();
        for r in 1..=degree {
            for j in (r..=degree).rev() {
                let index = span - degree + j;
                let denominator = knots[index + degree + 1 - r] - knots[index];
                let alpha = if denominator == 0.0 {
                    0.0
                } else {
                    (t - knots[index]) / denominator
                };
                let mut blended = [0.0; 4];
                for (axis, value) in blended.iter_mut().enumerate() {
                    *value = (1.0 - alpha) * local[j - 1][axis] + alpha * local[j][axis];
                }
                local[j] = blended;
            }
        }
        let [x, y, z, w] = local[degree];
        if w.abs() > 0.0 {
            Point3::new(x / w, y / w, z / w)
        } else {
            Point3::new(x, y, z)
        }
    }

    /// The parameter interval a rational's knots define.
    pub(crate) fn rational_domain(degree: usize, knots: &[f64], count: usize) -> (f64, f64) {
        (knots[degree], knots[count])
    }
}

/// How a B-spline's weights were read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Weights {
    /// No weights, or weights all equal to the bit.
    Exact,
    /// Weights equal within the tolerance a non-rational reading is allowed
    /// at: the relative spread measured.
    Approximated(f64),
}

/// Reads entities of one file with its units applied.
pub(crate) struct Reader<'a> {
    pub(crate) graph: &'a Graph,
    /// Multiplies the file's lengths into millimetres.
    pub(crate) scale: f64,
    /// Multiplies the file's plane angles into radians.
    pub(crate) angle: f64,
    /// The distance within which two points are one, in millimetres: the
    /// file's declared accuracy.
    pub(crate) weld: f64,
    /// Rational B-spline weights within this relative spread of one another
    /// are read as non-rational.
    pub(crate) rational_tolerance: f64,
}

pub(crate) fn unit(vector: Vector3) -> Option<Vector3> {
    let length = vector.length();
    (length.is_finite() && length > 1.0e-300).then(|| vector / length)
}

/// A unit vector perpendicular to a unit `axis`, chosen deterministically.
pub(crate) fn any_perpendicular(axis: Vector3) -> Vector3 {
    let seed = if axis.x.abs() < 0.9 {
        Vector3::new(1.0, 0.0, 0.0)
    } else {
        Vector3::new(0.0, 1.0, 0.0)
    };
    unit(seed - axis * seed.dot(axis)).unwrap_or(Vector3::new(0.0, 0.0, 1.0))
}

impl Reader<'_> {
    pub(crate) fn entity(&self, id: u64) -> Result<&Entity, Refusal> {
        self.graph.get(id).ok_or_else(|| {
            Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(id),
                "the entity is referenced but never defined",
            )
        })
    }

    /// The entity, which must be, among other things, a `kind`.
    pub(crate) fn expect(&self, id: u64, kind: &str) -> Result<&Entity, Refusal> {
        let entity = self.entity(id)?;
        if entity.is(kind) {
            Ok(entity)
        } else {
            Err(Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(id),
                format!("a {} where a {kind} was expected", entity.kind()),
            ))
        }
    }

    pub(crate) fn reference(
        &self,
        entity: &Entity,
        index: usize,
        what: &str,
    ) -> Result<u64, Refusal> {
        entity.arg(index).as_ref().ok_or_else(|| {
            Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(entity.id),
                format!("{} has no {what}", entity.kind()),
            )
        })
    }

    pub(crate) fn number(&self, entity: &Entity, index: usize, what: &str) -> Result<f64, Refusal> {
        entity
            .arg(index)
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or_else(|| {
                Refusal::new(
                    ENTITY_UNSUPPORTED,
                    Some(entity.id),
                    format!("{} has no finite {what}", entity.kind()),
                )
            })
    }

    pub(crate) fn point(&self, id: u64) -> Result<Point3, Refusal> {
        let entity = self.expect(id, "CARTESIAN_POINT")?;
        let coordinates = entity.arg(1).as_f64s().ok_or_else(|| {
            Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(id),
                "a CARTESIAN_POINT without coordinates",
            )
        })?;
        let mut xyz = [0.0; 3];
        for (slot, value) in coordinates.iter().take(3).enumerate() {
            xyz[slot] = value * self.scale;
        }
        let point = Point3::new(xyz[0], xyz[1], xyz[2]);
        if !point.is_finite() {
            return Err(Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(id),
                "a CARTESIAN_POINT with a non-finite coordinate",
            ));
        }
        Ok(point)
    }

    pub(crate) fn direction(&self, id: u64) -> Result<Vector3, Refusal> {
        let entity = self.expect(id, "DIRECTION")?;
        let ratios = entity.arg(1).as_f64s().ok_or_else(|| {
            Refusal::new(ENTITY_UNSUPPORTED, Some(id), "a DIRECTION without ratios")
        })?;
        let mut xyz = [0.0; 3];
        for (slot, value) in ratios.iter().take(3).enumerate() {
            xyz[slot] = *value;
        }
        unit(Vector3::new(xyz[0], xyz[1], xyz[2]))
            .ok_or_else(|| Refusal::new(ENTITY_UNSUPPORTED, Some(id), "a DIRECTION of zero length"))
    }

    /// An `AXIS2_PLACEMENT_3D` as a right-handed frame: `z` is the axis,
    /// `x` the reference direction made perpendicular to it, `y = z × x`.
    pub(crate) fn placement(&self, id: u64) -> Result<Frame3, Refusal> {
        let entity = self.expect(id, "AXIS2_PLACEMENT_3D")?;
        let origin = self.point(self.reference(entity, 1, "location")?)?;
        let z = match entity.arg(2).as_ref() {
            Some(axis) => self.direction(axis)?,
            None => Vector3::new(0.0, 0.0, 1.0),
        };
        let x = match entity.arg(3).as_ref() {
            Some(reference) => {
                let hint = self.direction(reference)?;
                unit(hint - z * hint.dot(z)).unwrap_or_else(|| any_perpendicular(z))
            }
            None => any_perpendicular(z),
        };
        Ok(Frame3 {
            origin,
            x,
            y: z.cross(x),
            z,
        })
    }

    /// An `AXIS1_PLACEMENT`: a point and a unit direction.
    pub(crate) fn axis1(&self, id: u64) -> Result<(Point3, Vector3), Refusal> {
        let entity = self.expect(id, "AXIS1_PLACEMENT")?;
        let origin = self.point(self.reference(entity, 1, "location")?)?;
        let axis = match entity.arg(2).as_ref() {
            Some(axis) => self.direction(axis)?,
            None => Vector3::new(0.0, 0.0, 1.0),
        };
        Ok((origin, axis))
    }

    pub(crate) fn vertex_point(&self, id: u64) -> Result<Point3, Refusal> {
        let entity = self.expect(id, "VERTEX_POINT")?;
        self.point(self.reference(entity, 1, "point")?)
    }

    /// The direction and magnitude of a `VECTOR`, in millimetres.
    pub(crate) fn vector(&self, id: u64) -> Result<(Vector3, f64), Refusal> {
        let entity = self.expect(id, "VECTOR")?;
        let direction = self.direction(self.reference(entity, 1, "orientation")?)?;
        let magnitude = self.number(entity, 2, "magnitude")? * self.scale;
        Ok((direction, magnitude))
    }

    /// A full knot vector from STEP's distinct knots and multiplicities.
    fn knots(
        &self,
        entity: &Entity,
        multiplicities: &Value,
        values: &Value,
    ) -> Result<Vec<f64>, Refusal> {
        let multiplicities = multiplicities.as_list().ok_or_else(|| {
            Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(entity.id),
                "a B-spline without knot multiplicities",
            )
        })?;
        let values = values.as_f64s().ok_or_else(|| {
            Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(entity.id),
                "a B-spline without knots",
            )
        })?;
        if multiplicities.len() != values.len() {
            return Err(Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(entity.id),
                "a B-spline whose knots and multiplicities differ in count",
            ));
        }
        let mut knots = Vec::new();
        for (multiplicity, value) in multiplicities.iter().zip(values) {
            let count = multiplicity.as_usize().ok_or_else(|| {
                Refusal::new(
                    ENTITY_UNSUPPORTED,
                    Some(entity.id),
                    "a knot multiplicity that is not a count",
                )
            })?;
            knots.extend(std::iter::repeat_n(value, count));
        }
        Ok(knots)
    }

    /// Weights as a non-rational reading allows them: absent, or all equal
    /// within the tolerance. Otherwise the rational refusal.
    fn weights(&self, entity: &Entity, weights: &[f64]) -> Result<Weights, Refusal> {
        if weights.is_empty() {
            return Ok(Weights::Exact);
        }
        let (low, high) = weights
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), w| {
                (low.min(*w), high.max(*w))
            });
        if !(low.is_finite() && high.is_finite()) || low <= 0.0 {
            return Err(Refusal::new(
                RATIONAL_UNSUPPORTED,
                Some(entity.id),
                "a rational B-spline with a weight that is not a positive number",
            ));
        }
        let spread = (high - low) / high;
        if spread == 0.0 {
            Ok(Weights::Exact)
        } else if spread <= self.rational_tolerance {
            Ok(Weights::Approximated(spread))
        } else {
            Err(Refusal::new(
                RATIONAL_UNSUPPORTED,
                Some(entity.id),
                format!(
                    "a rational B-spline whose weights differ by {spread:.3e} relative; the kernel holds \
                     non-rational splines only (ADR 0050), and weights within {:.0e} of one another \
                     are read as one",
                    self.rational_tolerance
                ),
            )
            .with_measure(spread, self.rational_tolerance))
        }
    }

    /// The kernel's B-spline curve for a `B_SPLINE_CURVE_WITH_KNOTS`, simple
    /// or as the complex `(BOUNDED_CURVE()B_SPLINE_CURVE(...)
    /// B_SPLINE_CURVE_WITH_KNOTS(...)...RATIONAL_B_SPLINE_CURVE(...))` form.
    pub(crate) fn bspline_curve(
        &self,
        entity: &Entity,
    ) -> Result<(SplineCurve3, Weights), Refusal> {
        let (degree, points, multiplicities, knot_values, weights) = if entity.is_complex() {
            let base = entity.instance("B_SPLINE_CURVE").ok_or_else(|| {
                Refusal::new(
                    ENTITY_UNSUPPORTED,
                    Some(entity.id),
                    "a complex B-spline curve without its B_SPLINE_CURVE part",
                )
            })?;
            let with_knots = entity.instance("B_SPLINE_CURVE_WITH_KNOTS").ok_or_else(|| {
                Refusal::new(
                    FACE_UNSUPPORTED,
                    Some(entity.id),
                    format!(
                        "a B-spline curve of the {} kind; only B_SPLINE_CURVE_WITH_KNOTS is read",
                        entity.kinds().collect::<Vec<_>>().join("+")
                    ),
                )
            })?;
            let weights = entity
                .instance("RATIONAL_B_SPLINE_CURVE")
                .and_then(|rational| rational.arg(0).as_f64s())
                .unwrap_or_default();
            (
                base.arg(0).clone(),
                base.arg(1).clone(),
                with_knots.arg(0).clone(),
                with_knots.arg(1).clone(),
                weights,
            )
        } else {
            (
                entity.arg(1).clone(),
                entity.arg(2).clone(),
                entity.arg(6).clone(),
                entity.arg(7).clone(),
                Vec::new(),
            )
        };
        let degree = degree.as_usize().ok_or_else(|| {
            Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(entity.id),
                "a B-spline curve without a degree",
            )
        })?;
        let control: Vec<Point3> = points
            .as_refs()
            .ok_or_else(|| {
                Refusal::new(
                    ENTITY_UNSUPPORTED,
                    Some(entity.id),
                    "a B-spline curve without control points",
                )
            })?
            .into_iter()
            .map(|id| self.point(id))
            .collect::<Result<_, _>>()?;
        let knots = self.knots(entity, &multiplicities, &knot_values)?;
        let weights = self.weights(entity, &weights)?;
        let curve = SplineCurve3::new(
            degree,
            knots,
            control
                .iter()
                .map(|point| crate::bspline::array3(*point))
                .collect(),
        )
        .map_err(|error| spline_refusal(entity.id, error, "curve"))?;
        Ok((curve, weights))
    }

    /// A rational B-spline curve's homogeneous data, for sampling only.
    fn rational_curve(&self, entity: &Entity) -> Result<(CurveGeometry, Weights), Refusal> {
        let (degree, points, multiplicities, knot_values, weights) = if entity.is_complex() {
            let base = entity
                .instance("B_SPLINE_CURVE")
                .expect("checked by the caller");
            let with_knots = entity
                .instance("B_SPLINE_CURVE_WITH_KNOTS")
                .expect("checked by the caller");
            let weights = entity
                .instance("RATIONAL_B_SPLINE_CURVE")
                .and_then(|rational| rational.arg(0).as_f64s())
                .unwrap_or_default();
            (
                base.arg(0).clone(),
                base.arg(1).clone(),
                with_knots.arg(0).clone(),
                with_knots.arg(1).clone(),
                weights,
            )
        } else {
            (
                entity.arg(1).clone(),
                entity.arg(2).clone(),
                entity.arg(6).clone(),
                entity.arg(7).clone(),
                Vec::new(),
            )
        };
        let degree = degree.as_usize().unwrap_or(0);
        let control: Vec<Point3> = points
            .as_refs()
            .unwrap_or_default()
            .into_iter()
            .map(|id| self.point(id))
            .collect::<Result<_, _>>()?;
        let knots = self.knots(entity, &multiplicities, &knot_values)?;
        if degree == 0
            || control.len() != weights.len()
            || knots.len() != control.len() + degree + 1
        {
            return Err(Refusal::new(
                RATIONAL_UNSUPPORTED,
                Some(entity.id),
                "a rational B-spline curve whose weights, knots and control points do not fit together",
            ));
        }
        let points = control
            .iter()
            .zip(&weights)
            .map(|(point, weight)| {
                [
                    point.x * weight,
                    point.y * weight,
                    point.z * weight,
                    *weight,
                ]
            })
            .collect();
        Ok((
            CurveGeometry::Rational {
                degree,
                knots,
                points,
            },
            Weights::Exact,
        ))
    }

    /// The kernel's B-spline surface for a `B_SPLINE_SURFACE_WITH_KNOTS`,
    /// simple or in its complex form, with its natural orientation.
    pub(crate) fn bspline_surface(
        &self,
        entity: &Entity,
    ) -> Result<(SplineSurface, Weights), Refusal> {
        let (degree_u, degree_v, net, mult_u, mult_v, knots_u, knots_v, weights) = if entity
            .is_complex()
        {
            let base = entity.instance("B_SPLINE_SURFACE").ok_or_else(|| {
                Refusal::new(
                    ENTITY_UNSUPPORTED,
                    Some(entity.id),
                    "a complex B-spline surface without its B_SPLINE_SURFACE part",
                )
            })?;
            let with_knots = entity.instance("B_SPLINE_SURFACE_WITH_KNOTS").ok_or_else(|| {
                    Refusal::new(
                        FACE_UNSUPPORTED,
                        Some(entity.id),
                        format!(
                            "a B-spline surface of the {} kind; only B_SPLINE_SURFACE_WITH_KNOTS is read",
                            entity.kinds().collect::<Vec<_>>().join("+")
                        ),
                    )
                })?;
            let weights: Vec<f64> = entity
                .instance("RATIONAL_B_SPLINE_SURFACE")
                .and_then(|rational| rational.arg(0).as_list())
                .map(|rows| rows.iter().filter_map(Value::as_f64s).flatten().collect())
                .unwrap_or_default();
            (
                base.arg(0).clone(),
                base.arg(1).clone(),
                base.arg(2).clone(),
                with_knots.arg(0).clone(),
                with_knots.arg(1).clone(),
                with_knots.arg(2).clone(),
                with_knots.arg(3).clone(),
                weights,
            )
        } else {
            (
                entity.arg(1).clone(),
                entity.arg(2).clone(),
                entity.arg(3).clone(),
                entity.arg(8).clone(),
                entity.arg(9).clone(),
                entity.arg(10).clone(),
                entity.arg(11).clone(),
                Vec::new(),
            )
        };
        let degree = [
            degree_u.as_usize().ok_or_else(|| {
                Refusal::new(
                    ENTITY_UNSUPPORTED,
                    Some(entity.id),
                    "a B-spline surface without a u degree",
                )
            })?,
            degree_v.as_usize().ok_or_else(|| {
                Refusal::new(
                    ENTITY_UNSUPPORTED,
                    Some(entity.id),
                    "a B-spline surface without a v degree",
                )
            })?,
        ];
        let rows = net.as_list().ok_or_else(|| {
            Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(entity.id),
                "a B-spline surface without a control net",
            )
        })?;
        let mut points = Vec::new();
        let mut count_v = None;
        for row in rows {
            let ids = row.as_refs().ok_or_else(|| {
                Refusal::new(
                    ENTITY_UNSUPPORTED,
                    Some(entity.id),
                    "a control net row that is not a list of points",
                )
            })?;
            if *count_v.get_or_insert(ids.len()) != ids.len() {
                return Err(Refusal::new(
                    ENTITY_UNSUPPORTED,
                    Some(entity.id),
                    "a control net whose rows differ in length",
                ));
            }
            for id in ids {
                points.push(crate::bspline::array3(self.point(id)?));
            }
        }
        let counts = [rows.len(), count_v.unwrap_or(0)];
        let knots = [
            self.knots(entity, &mult_u, &knots_u)?,
            self.knots(entity, &mult_v, &knots_v)?,
        ];
        let weights = self.weights(entity, &weights)?;
        let surface = SplineSurface::new(degree, knots, counts, points)
            .map_err(|error| spline_refusal(entity.id, error, "surface"))?;
        Ok((surface, weights))
    }

    /// Reads a curve entity. `SURFACE_CURVE`, `SEAM_CURVE` and
    /// `INTERSECTION_CURVE` are read through their 3D curve.
    pub(crate) fn curve(&self, id: u64) -> Result<(CurveGeometry, Weights), Refusal> {
        let entity = self.entity(id)?;
        if entity.is("SURFACE_CURVE")
            || entity.is("SEAM_CURVE")
            || entity.is("INTERSECTION_CURVE")
            || entity.is("BOUNDED_SURFACE_CURVE")
        {
            let instance = entity
                .instance("SURFACE_CURVE")
                .or_else(|| entity.instance("SEAM_CURVE"))
                .or_else(|| entity.instance("INTERSECTION_CURVE"))
                .or_else(|| entity.instances.first())
                .expect("checked above");
            let basis = instance.arg(1).as_ref().ok_or_else(|| {
                Refusal::new(
                    ENTITY_UNSUPPORTED,
                    Some(id),
                    "a surface curve without its 3D curve",
                )
            })?;
            return self.curve(basis);
        }
        if entity.is("TRIMMED_CURVE") {
            let basis = self.reference(entity, 1, "basis curve")?;
            return self.curve(basis);
        }
        if entity.is("B_SPLINE_CURVE_WITH_KNOTS") || entity.is("B_SPLINE_CURVE") {
            return match self.bspline_curve(entity) {
                Ok((curve, weights)) => Ok((CurveGeometry::Bspline { curve }, weights)),
                // Kept as a rational so an edge on it can still be recognised
                // as a circle or a line from samples of it; an edge that is
                // neither is refused where it is bounded.
                Err(refusal) if refusal.code == RATIONAL_UNSUPPORTED => self.rational_curve(entity),
                Err(refusal) => Err(refusal),
            };
        }
        match entity.kind() {
            "LINE" => {
                let point = self.point(self.reference(entity, 1, "point")?)?;
                let (direction, _) = self.vector(self.reference(entity, 2, "direction")?)?;
                Ok((CurveGeometry::Line { point, direction }, Weights::Exact))
            }
            "CIRCLE" => {
                let frame = self.placement(self.reference(entity, 1, "placement")?)?;
                let radius = self.number(entity, 2, "radius")? * self.scale;
                if radius <= 0.0 {
                    return Err(Refusal::new(
                        ENTITY_UNSUPPORTED,
                        Some(id),
                        "a CIRCLE of zero radius",
                    ));
                }
                Ok((
                    CurveGeometry::Circle {
                        center: frame.origin,
                        u: frame.x,
                        v: frame.y,
                        radius,
                    },
                    Weights::Exact,
                ))
            }
            "ELLIPSE" => {
                let frame = self.placement(self.reference(entity, 1, "placement")?)?;
                let major = self.number(entity, 2, "semi-axis")? * self.scale;
                let minor = self.number(entity, 3, "semi-axis")? * self.scale;
                if major <= 0.0 || minor <= 0.0 {
                    return Err(Refusal::new(
                        ENTITY_UNSUPPORTED,
                        Some(id),
                        "an ELLIPSE with a zero semi-axis",
                    ));
                }
                // The kernel's ellipse keeps `a ≥ b` along `u`; a file that
                // lists the minor axis first is turned a quarter turn.
                let (u, v, major, minor) = if major >= minor {
                    (frame.x, frame.y, major, minor)
                } else {
                    (frame.y, frame.x * -1.0, minor, major)
                };
                Ok((
                    CurveGeometry::Ellipse {
                        center: frame.origin,
                        u,
                        v,
                        major,
                        minor,
                    },
                    Weights::Exact,
                ))
            }
            "POLYLINE" => {
                let points = entity.arg(1).as_refs().unwrap_or_default();
                if points.len() == 2 {
                    let start = self.point(points[0])?;
                    let end = self.point(points[1])?;
                    let direction = unit(end - start).ok_or_else(|| {
                        Refusal::new(
                            ENTITY_UNSUPPORTED,
                            Some(id),
                            "a POLYLINE of two coincident points",
                        )
                    })?;
                    return Ok((
                        CurveGeometry::Line {
                            point: start,
                            direction,
                        },
                        Weights::Exact,
                    ));
                }
                Err(Refusal::new(
                    FACE_UNSUPPORTED,
                    Some(id),
                    format!(
                        "a POLYLINE of {} points; only a two-point polyline is a curve the kernel carries",
                        points.len()
                    ),
                ))
            }
            other => Err(Refusal::new(
                FACE_UNSUPPORTED,
                Some(id),
                format!("a curve of kind {other}, which the kernel does not carry"),
            )),
        }
    }

    /// Reads a surface entity oriented so that its kernel normal is the face
    /// normal: `same_sense` false turns the carrier over. Returns the
    /// surface and whether a rational reading was approximated.
    pub(crate) fn surface(&self, id: u64, same_sense: bool) -> Result<(Surface, Weights), Refusal> {
        let entity = self.entity(id)?;
        let sign = if same_sense { 1.0 } else { -1.0 };
        if entity.is("B_SPLINE_SURFACE_WITH_KNOTS") || entity.is("B_SPLINE_SURFACE") {
            let (surface, weights) = self.bspline_surface(entity)?;
            let surface = if same_sense {
                surface
            } else {
                surface.reversed_u()
            };
            return Ok((Surface::Bspline(surface), weights));
        }
        match entity.kind() {
            "PLANE" => {
                let frame = self.placement(self.reference(entity, 1, "placement")?)?;
                let plane = if same_sense {
                    Plane::new(frame.origin, frame.x, frame.y)
                } else {
                    Plane::new(frame.origin, frame.y, frame.x)
                };
                Ok((Surface::Plane(plane), Weights::Exact))
            }
            "CYLINDRICAL_SURFACE" => {
                let frame = self.placement(self.reference(entity, 1, "placement")?)?;
                let radius = self.number(entity, 2, "radius")? * self.scale;
                if radius <= 0.0 {
                    return Err(Refusal::new(
                        FACE_UNSUPPORTED,
                        Some(id),
                        "a cylinder of zero radius",
                    ));
                }
                Ok((
                    Surface::Cylinder(Cylinder {
                        origin: frame.origin,
                        axis: frame.z,
                        radial_u: frame.x,
                        radial_v: frame.y,
                        radius,
                        angular_sign: sign,
                    }),
                    Weights::Exact,
                ))
            }
            "CONICAL_SURFACE" => {
                let frame = self.placement(self.reference(entity, 1, "placement")?)?;
                let radius = self.number(entity, 2, "radius")? * self.scale;
                let semi_angle = self.number(entity, 3, "semi-angle")? * self.angle;
                if radius < 0.0
                    || semi_angle.abs() <= 1.0e-12
                    || semi_angle.abs() >= std::f64::consts::FRAC_PI_2 - 1.0e-12
                {
                    return Err(Refusal::new(
                        FACE_UNSUPPORTED,
                        Some(id),
                        "a cone whose semi-angle is not strictly between zero and a right angle",
                    ));
                }
                Ok((
                    Surface::Cone(Cone {
                        origin: frame.origin,
                        axis: frame.z,
                        radial_u: frame.x,
                        radial_v: frame.y,
                        base_radius: radius,
                        slope: semi_angle.tan(),
                        angular_sign: sign,
                    }),
                    Weights::Exact,
                ))
            }
            "SPHERICAL_SURFACE" => {
                let frame = self.placement(self.reference(entity, 1, "placement")?)?;
                let radius = self.number(entity, 2, "radius")? * self.scale;
                if radius <= 0.0 {
                    return Err(Refusal::new(
                        FACE_UNSUPPORTED,
                        Some(id),
                        "a sphere of zero radius",
                    ));
                }
                Ok((
                    Surface::Sphere(Sphere {
                        origin: frame.origin,
                        axis: frame.z,
                        radial_u: frame.x,
                        radial_v: frame.y,
                        radius,
                        angular_sign: sign,
                    }),
                    Weights::Exact,
                ))
            }
            "TOROIDAL_SURFACE" => {
                let frame = self.placement(self.reference(entity, 1, "placement")?)?;
                let major = self.number(entity, 2, "major radius")? * self.scale;
                let minor = self.number(entity, 3, "minor radius")? * self.scale;
                if minor <= 0.0 || major <= minor {
                    return Err(Refusal::new(
                        FACE_UNSUPPORTED,
                        Some(id),
                        "a torus that is not a ring torus (the major radius must exceed the minor)",
                    ));
                }
                Ok((
                    Surface::Torus(Torus {
                        origin: frame.origin,
                        axis: frame.z,
                        radial_u: frame.x,
                        radial_v: frame.y,
                        major_radius: major,
                        minor_radius: minor,
                        angular_sign: sign,
                    }),
                    Weights::Exact,
                ))
            }
            "RECTANGULAR_TRIMMED_SURFACE" => {
                // The trimming is a parameter window the loops already
                // carry; the carrier is the basis surface, turned over when
                // exactly one of the two senses is reversed.
                let basis = self.reference(entity, 1, "basis surface")?;
                let u_sense = entity.arg(6).as_bool().unwrap_or(true);
                let v_sense = entity.arg(7).as_bool().unwrap_or(true);
                self.surface(basis, same_sense == (u_sense == v_sense))
            }
            "SURFACE_OF_LINEAR_EXTRUSION" => self.linear_extrusion(entity, sign),
            "SURFACE_OF_REVOLUTION" => self.revolution(entity, sign),
            other => Err(Refusal::new(
                FACE_UNSUPPORTED,
                Some(id),
                format!("a surface of kind {other}, which the kernel does not carry"),
            )),
        }
    }

    /// A `SURFACE_OF_LINEAR_EXTRUSION` whose generatrix is a line (a plane)
    /// or a circle about the extrusion direction (a cylinder).
    fn linear_extrusion(&self, entity: &Entity, sign: f64) -> Result<(Surface, Weights), Refusal> {
        let (curve, _) = self.curve(self.reference(entity, 1, "swept curve")?)?;
        let (direction, _) = self.vector(self.reference(entity, 2, "extrusion axis")?)?;
        match curve {
            CurveGeometry::Line {
                point,
                direction: along,
            } => {
                let normal = along.cross(direction);
                if unit(normal).is_none() {
                    return Err(Refusal::new(
                        FACE_UNSUPPORTED,
                        Some(entity.id),
                        "a line extruded along itself",
                    ));
                }
                let plane = if sign > 0.0 {
                    Plane::new(point, along, direction)
                } else {
                    Plane::new(point, direction, along)
                };
                Ok((Surface::Plane(plane), Weights::Exact))
            }
            CurveGeometry::Circle {
                center,
                u,
                v,
                radius,
            } => {
                let axis = u.cross(v);
                let along = axis.dot(direction);
                if along.abs() < 1.0 - 1.0e-9 {
                    return Err(Refusal::new(
                        FACE_UNSUPPORTED,
                        Some(entity.id),
                        "a circle extruded obliquely to its own axis, which is no elementary surface",
                    ));
                }
                Ok((
                    Surface::Cylinder(Cylinder {
                        origin: center,
                        axis,
                        radial_u: u,
                        radial_v: v,
                        radius,
                        angular_sign: sign * along.signum(),
                    }),
                    Weights::Exact,
                ))
            }
            _ => Err(Refusal::new(
                FACE_UNSUPPORTED,
                Some(entity.id),
                "a surface of linear extrusion whose generatrix is neither a line nor a circle",
            )),
        }
    }

    /// A `SURFACE_OF_REVOLUTION` whose generatrix is a line (a cylinder, a
    /// cone or a plane) or a circle in a plane through the axis (a sphere or
    /// a ring torus). The surface's azimuth zero is the generatrix itself.
    fn revolution(&self, entity: &Entity, sign: f64) -> Result<(Surface, Weights), Refusal> {
        let (curve, _) = self.curve(self.reference(entity, 1, "swept curve")?)?;
        let (origin, axis) = self.axis1(self.reference(entity, 2, "axis")?)?;
        let radial_of = |point: Point3| -> (f64, Option<Vector3>) {
            let relative = point - origin;
            let radial = relative - axis * relative.dot(axis);
            (radial.length(), unit(radial))
        };
        match curve {
            CurveGeometry::Line { point, direction } => {
                let axial = direction.dot(axis);
                // The generatrix's own radial direction is the surface's
                // reference direction; a line through the axis takes it from
                // a point further along.
                let (mut distance, mut radial) = radial_of(point);
                if radial.is_none() {
                    let far = point + direction * 1.0;
                    let (_, other) = radial_of(far);
                    radial = other;
                    distance = 0.0;
                }
                let Some(radial_u) = radial else {
                    return Err(Refusal::new(
                        FACE_UNSUPPORTED,
                        Some(entity.id),
                        "a line revolved about itself",
                    ));
                };
                let radial_v = axis.cross(radial_u);
                let radial_rate = direction.dot(radial_u);
                if axial.abs() < 1.0e-12 {
                    // A radial line sweeps a plane through the point.
                    let (u, v) = if sign * radial_rate.signum() > 0.0 {
                        (radial_u, radial_v)
                    } else {
                        (radial_v, radial_u)
                    };
                    let plane_origin = origin + axis * (point - origin).dot(axis);
                    return Ok((
                        Surface::Plane(Plane::new(plane_origin, u, v)),
                        Weights::Exact,
                    ));
                }
                let slope = radial_rate / axial;
                let height = (point - origin).dot(axis);
                let angular_sign = sign * axial.signum();
                if slope.abs() <= 1.0e-12 {
                    if distance <= 0.0 {
                        return Err(Refusal::new(
                            FACE_UNSUPPORTED,
                            Some(entity.id),
                            "a line revolved about itself",
                        ));
                    }
                    return Ok((
                        Surface::Cylinder(Cylinder {
                            origin,
                            axis,
                            radial_u,
                            radial_v,
                            radius: distance,
                            angular_sign,
                        }),
                        Weights::Exact,
                    ));
                }
                Ok((
                    Surface::Cone(Cone {
                        origin,
                        axis,
                        radial_u,
                        radial_v,
                        base_radius: distance - slope * height,
                        slope,
                        angular_sign,
                    }),
                    Weights::Exact,
                ))
            }
            CurveGeometry::Circle {
                center,
                u,
                v,
                radius,
            } => {
                let normal = u.cross(v);
                if normal.dot(axis).abs() > 1.0e-9 {
                    return Err(Refusal::new(
                        FACE_UNSUPPORTED,
                        Some(entity.id),
                        "a circle revolved out of a plane through the axis, which is no elementary surface",
                    ));
                }
                let (distance, radial) = radial_of(center);
                let height = (center - origin).dot(axis);
                let ring_origin = origin + axis * height;
                if distance <= self.weld {
                    // A circle centred on the axis sweeps a sphere.
                    let radial_u = unit(u - axis * u.dot(axis))
                        .or_else(|| unit(v - axis * v.dot(axis)))
                        .unwrap_or_else(|| any_perpendicular(axis));
                    let radial_v = axis.cross(radial_u);
                    let outward = normal.dot(radial_u.cross(axis)).signum();
                    return Ok((
                        Surface::Sphere(Sphere {
                            origin: ring_origin,
                            axis,
                            radial_u,
                            radial_v,
                            radius,
                            angular_sign: sign * outward,
                        }),
                        Weights::Exact,
                    ));
                }
                let radial_u = radial.expect("a positive distance has a direction");
                if distance <= radius {
                    return Err(Refusal::new(
                        FACE_UNSUPPORTED,
                        Some(entity.id),
                        "a circle revolved about an axis it reaches, which is no ring torus",
                    ));
                }
                let radial_v = axis.cross(radial_u);
                let outward = normal.dot(radial_u.cross(axis)).signum();
                Ok((
                    Surface::Torus(Torus {
                        origin: ring_origin,
                        axis,
                        radial_u,
                        radial_v,
                        major_radius: distance,
                        minor_radius: radius,
                        angular_sign: sign * outward,
                    }),
                    Weights::Exact,
                ))
            }
            _ => Err(Refusal::new(
                FACE_UNSUPPORTED,
                Some(entity.id),
                "a surface of revolution whose generatrix is neither a line nor a circle",
            )),
        }
    }
}

fn spline_refusal(id: u64, error: SplineError, what: &str) -> Refusal {
    let (code, message) = match error {
        SplineError::Degree => (
            BSPLINE_DEGREE_UNSUPPORTED,
            format!("a B-spline {what} of a degree the kernel does not carry (one to five)"),
        ),
        SplineError::Rational => (RATIONAL_UNSUPPORTED, format!("a rational B-spline {what}")),
        SplineError::Unclamped => (
            BSPLINE_UNCLAMPED_UNSUPPORTED,
            format!(
                "a B-spline {what} whose knot vector is not clamped; closed and periodic splines are not carried"
            ),
        ),
        SplineError::Knots => (
            BSPLINE_KNOTS_INVALID,
            format!(
                "a B-spline {what} whose knot vector does not fit its degree and control points"
            ),
        ),
        SplineError::NonFinite => (
            BSPLINE_KNOTS_INVALID,
            format!("a B-spline {what} with a non-finite knot or coordinate"),
        ),
    };
    Refusal::new(code, Some(id), message)
}

/// The kernel `Curve3` an edge from `start` to `end` runs along, and its
/// parameter range from the start to the end: decreasing when the file's
/// `same_sense` says the curve runs the other way.
pub(crate) fn bounded_curve(
    geometry: CurveGeometry,
    start: Point3,
    end: Point3,
    same_sense: bool,
    closed: bool,
    weld: f64,
    entity: u64,
) -> Result<(Curve3, ParameterRange), Refusal> {
    match geometry {
        CurveGeometry::Line { direction, .. } => {
            let chord = end - start;
            let length = chord.length();
            if length <= weld {
                return Err(Refusal::new(
                    ENTITY_UNSUPPORTED,
                    Some(entity),
                    "a line edge whose two vertices coincide",
                ));
            }
            let off = chord.cross(direction).length() / length;
            if off > 1.0e-6 {
                return Err(Refusal::new(
                    GAP_EXCEEDS_TOLERANCE,
                    Some(entity),
                    "a line edge whose vertices do not lie along its line",
                )
                .with_measure(off * length, weld));
            }
            Ok(Curve3::line_segment([start, end]))
        }
        CurveGeometry::Circle {
            center,
            u,
            v,
            radius,
        } => {
            let angle_of = |point: Point3| {
                let arm = point - center;
                arm.dot(v).atan2(arm.dot(u))
            };
            let radial_error = [start, end]
                .into_iter()
                .map(|point| ((point - center).length() - radius).abs())
                .fold(0.0_f64, f64::max);
            let plane_error = [start, end]
                .into_iter()
                .map(|point| (point - center).dot(u.cross(v)).abs())
                .fold(0.0_f64, f64::max);
            let error = radial_error.max(plane_error);
            if error > weld.max(1.0e-9) * 4.0 {
                return Err(Refusal::new(
                    GAP_EXCEEDS_TOLERANCE,
                    Some(entity),
                    "a circular edge whose vertices do not lie on its circle within the file's accuracy",
                )
                .with_measure(error, weld));
            }
            let theta_start = canonical_angle(angle_of(start));
            let sweep = if closed {
                std::f64::consts::TAU
            } else {
                let raw = (angle_of(end) - theta_start).rem_euclid(std::f64::consts::TAU);
                if raw <= 1.0e-12 {
                    std::f64::consts::TAU
                } else {
                    raw
                }
            };
            let sweep = if same_sense {
                sweep
            } else {
                sweep - std::f64::consts::TAU
            };
            Ok((
                Curve3::Circle {
                    center,
                    u,
                    v,
                    radius,
                },
                ParameterRange::new(theta_start, theta_start + sweep),
            ))
        }
        CurveGeometry::Ellipse {
            center,
            u,
            v,
            major,
            minor,
        } => {
            let angle_of = |point: Point3| {
                let arm = point - center;
                (arm.dot(v) / minor).atan2(arm.dot(u) / major)
            };
            let on_ellipse = |point: Point3| {
                let arm = point - center;
                let x = arm.dot(u) / major;
                let y = arm.dot(v) / minor;
                ((x * x + y * y).sqrt() - 1.0).abs() * major
            };
            let error = on_ellipse(start).max(on_ellipse(end));
            if error > weld.max(1.0e-9) * 4.0 {
                return Err(Refusal::new(
                    GAP_EXCEEDS_TOLERANCE,
                    Some(entity),
                    "an elliptical edge whose vertices do not lie on its ellipse within the file's accuracy",
                )
                .with_measure(error, weld));
            }
            let theta_start = canonical_angle(angle_of(start));
            let sweep = if closed {
                std::f64::consts::TAU
            } else {
                let raw = (angle_of(end) - theta_start).rem_euclid(std::f64::consts::TAU);
                if raw <= 1.0e-12 {
                    std::f64::consts::TAU
                } else {
                    raw
                }
            };
            let sweep = if same_sense {
                sweep
            } else {
                sweep - std::f64::consts::TAU
            };
            Ok((
                Curve3::Ellipse {
                    center,
                    u,
                    v,
                    major_radius: major,
                    minor_radius: minor,
                },
                ParameterRange::new(theta_start, theta_start + sweep),
            ))
        }
        CurveGeometry::Bspline { curve } => {
            let (from, to) = curve.domain();
            let (first, last) = (curve.point(from), curve.point(to));
            let (first, last) = if same_sense {
                (first, last)
            } else {
                (last, first)
            };
            let gap = first.distance(start).max(last.distance(end));
            if gap > weld.max(1.0e-9) * 4.0 {
                return Err(Refusal::new(
                    GAP_EXCEEDS_TOLERANCE,
                    Some(entity),
                    "a B-spline edge whose vertices do not lie at its ends within the file's accuracy",
                )
                .with_measure(gap, weld));
            }
            Ok((
                Curve3::Bspline { curve },
                if same_sense {
                    ParameterRange::new(from, to)
                } else {
                    ParameterRange::new(to, from)
                },
            ))
        }
        CurveGeometry::Rational { .. } => Err(Refusal::new(
            RATIONAL_UNSUPPORTED,
            Some(entity),
            "a rational B-spline edge that is neither a line nor a circle within the file's \
             accuracy; the kernel holds non-rational splines only (ADR 0050)",
        )),
    }
}

/// Points along a curve at `count + 1` parameters from one end to the
/// other, for recognising what a B-spline edge really is.
pub(crate) fn sample_curve(geometry: &CurveGeometry, count: usize) -> Option<Vec<Point3>> {
    let (from, to, at): (f64, f64, Box<dyn Fn(f64) -> Point3>) = match geometry {
        CurveGeometry::Bspline { curve } => {
            let (from, to) = curve.domain();
            let curve = *curve;
            (from, to, Box::new(move |t| curve.point(t)))
        }
        CurveGeometry::Rational {
            degree,
            knots,
            points,
        } => {
            let (from, to) = CurveGeometry::rational_domain(*degree, knots, points.len());
            let (degree, knots, points) = (*degree, knots.clone(), points.clone());
            (
                from,
                to,
                Box::new(move |t| CurveGeometry::rational_point(degree, &knots, &points, t)),
            )
        }
        _ => return None,
    };
    Some(
        (0..=count)
            .map(|step| at((to - from).mul_add(step as f64 / count as f64, from)))
            .collect(),
    )
}

/// The line or circle a sampled curve lies on within `tolerance`, if it
/// lies on one: the exact curve a B-spline spelling of a conic is snapped
/// to. A closed sampling is fitted through three of its points.
pub(crate) fn recognise_conic(samples: &[Point3], tolerance: f64) -> Option<CurveGeometry> {
    if samples.len() < 3 {
        return None;
    }
    let first = samples[0];
    let last = samples[samples.len() - 1];
    let closed = first.distance(last) <= tolerance;
    // A line: every sample within tolerance of the chord.
    if !closed && let Some(direction) = unit(last - first) {
        let off = samples
            .iter()
            .map(|point| (*point - first).cross(direction).length())
            .fold(0.0_f64, f64::max);
        if off <= tolerance {
            return Some(CurveGeometry::Line {
                point: first,
                direction,
            });
        }
    }
    // A circle through three well-spread samples.
    let third = samples.len() / 3;
    let (a, b, c) = if closed {
        (samples[0], samples[third], samples[2 * third])
    } else {
        (first, samples[samples.len() / 2], last)
    };
    let ab = b - a;
    let ac = c - a;
    let normal = unit(ab.cross(ac))?;
    // The circumcentre in the plane of the three points.
    let ab_length = ab.dot(ab);
    let ac_length = ac.dot(ac);
    let cross = ab.cross(ac);
    let denominator = 2.0 * cross.dot(cross);
    if denominator <= 1.0e-300 {
        return None;
    }
    let to_center = (ac * ab_length - ab * ac_length).cross(cross) / denominator;
    let center = a + to_center;
    let radius = to_center.length();
    if !radius.is_finite() || radius <= tolerance {
        return None;
    }
    let error = samples
        .iter()
        .map(|point| {
            let arm = *point - center;
            ((arm.length() - radius).abs()).max(arm.dot(normal).abs())
        })
        .fold(0.0_f64, f64::max);
    if error > tolerance {
        return None;
    }
    let u = unit(a - center)?;
    let v = normal.cross(u);
    Some(CurveGeometry::Circle {
        center,
        u,
        v,
        radius,
    })
}

/// An angle in `[0, 2π)`, with a value within a nanoradian of a whole turn
/// read as zero, so the kernel's canonical seam azimuths come out exact.
pub(crate) fn canonical_angle(angle: f64) -> f64 {
    let wrapped = angle.rem_euclid(std::f64::consts::TAU);
    if wrapped >= std::f64::consts::TAU - 1.0e-9 || wrapped <= 1.0e-9 {
        0.0
    } else if (wrapped - std::f64::consts::PI).abs() <= 1.0e-9 {
        std::f64::consts::PI
    } else {
        wrapped
    }
}

/// Where a file keeps its solids: the shapes of the first product that has
/// any, or, without product structure, every solid and shell model in the
/// file.
#[derive(Clone, Debug)]
pub(crate) struct ShapeSource {
    pub(crate) kind: ShapeKind,
    /// The closed shells that bound material: one for a solid, every shell
    /// for a shell-based model.
    pub(crate) outer: Vec<u64>,
    /// A `BREP_WITH_VOIDS`'s cavity shells, each with whether its
    /// orientation is as written.
    pub(crate) voids: Vec<(u64, bool)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShapeKind {
    Solid,
    ShellModel,
}

const SOLID_KINDS: [&str; 4] = [
    "MANIFOLD_SOLID_BREP",
    "BREP_WITH_VOIDS",
    "FACETED_BREP",
    "SHELL_BASED_SURFACE_MODEL",
];

fn shape_source(reader: &Reader<'_>, entity: &Entity) -> Result<Option<ShapeSource>, Refusal> {
    if entity.is("BREP_WITH_VOIDS") {
        let instance = entity.instance("BREP_WITH_VOIDS").expect("checked");
        let outer = instance.arg(1).as_ref().ok_or_else(|| {
            Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(entity.id),
                "a BREP_WITH_VOIDS without an outer shell",
            )
        })?;
        let mut voids = Vec::new();
        for id in instance.arg(2).as_refs().unwrap_or_default() {
            let oriented = reader.entity(id)?;
            if oriented.is("ORIENTED_CLOSED_SHELL") {
                let inner = oriented.instance("ORIENTED_CLOSED_SHELL").expect("checked");
                let shell = inner.arg(2).as_ref().ok_or_else(|| {
                    Refusal::new(
                        ENTITY_UNSUPPORTED,
                        Some(id),
                        "an oriented shell without its shell",
                    )
                })?;
                voids.push((shell, inner.arg(3).as_bool().unwrap_or(true)));
            } else {
                voids.push((id, true));
            }
        }
        return Ok(Some(ShapeSource {
            kind: ShapeKind::Solid,
            outer: vec![outer],
            voids,
        }));
    }
    if entity.is("MANIFOLD_SOLID_BREP") || entity.is("FACETED_BREP") {
        let instance = entity
            .instance("MANIFOLD_SOLID_BREP")
            .or_else(|| entity.instance("FACETED_BREP"))
            .expect("checked");
        let outer = instance.arg(1).as_ref().ok_or_else(|| {
            Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(entity.id),
                "a solid without an outer shell",
            )
        })?;
        return Ok(Some(ShapeSource {
            kind: ShapeKind::Solid,
            outer: vec![outer],
            voids: Vec::new(),
        }));
    }
    if entity.is("SHELL_BASED_SURFACE_MODEL") {
        let instance = entity
            .instance("SHELL_BASED_SURFACE_MODEL")
            .expect("checked");
        let shells = instance.arg(1).as_refs().unwrap_or_default();
        return Ok(Some(ShapeSource {
            kind: ShapeKind::ShellModel,
            outer: shells,
            voids: Vec::new(),
        }));
    }
    Ok(None)
}

/// The shapes to import. With product structure, the first product
/// definition whose shape representation (directly or through a
/// `SHAPE_REPRESENTATION_RELATIONSHIP`) holds solids; otherwise every solid
/// in the file. An assembly's other occurrences are noted, not placed.
pub(crate) fn find_shapes(reader: &Reader<'_>) -> Result<(Vec<ShapeSource>, usize), Refusal> {
    let graph = reader.graph;
    let occurrences = graph.count("NEXT_ASSEMBLY_USAGE_OCCURRENCE");
    let mut chosen: Option<Vec<u64>> = None;
    for definition in graph.of_kind("SHAPE_DEFINITION_REPRESENTATION") {
        let Some(representation) = definition.arg(1).as_ref() else {
            continue;
        };
        let mut representations = vec![representation];
        for relationship in graph.of_kind("SHAPE_REPRESENTATION_RELATIONSHIP") {
            let instance = relationship
                .instance("SHAPE_REPRESENTATION_RELATIONSHIP")
                .or_else(|| relationship.instance("REPRESENTATION_RELATIONSHIP"))
                .expect("checked");
            let pair = [instance.arg(2).as_ref(), instance.arg(3).as_ref()];
            if pair.contains(&Some(representation)) {
                representations.extend(
                    pair.into_iter()
                        .flatten()
                        .filter(|id| *id != representation),
                );
            }
        }
        let mut solids = Vec::new();
        for id in representations {
            let Some(entity) = graph.get(id) else {
                continue;
            };
            let items = entity
                .instances
                .iter()
                .find(|instance| instance.kind.ends_with("REPRESENTATION"))
                .map(|instance| instance.arg(1).as_refs().unwrap_or_default())
                .unwrap_or_default();
            for item in items {
                if graph
                    .get(item)
                    .is_some_and(|entity| SOLID_KINDS.iter().any(|kind| entity.is(kind)))
                {
                    solids.push(item);
                }
            }
        }
        if !solids.is_empty() {
            chosen = Some(solids);
            break;
        }
    }
    let ids: Vec<u64> = match chosen {
        Some(ids) => ids,
        None => graph
            .entities()
            .filter(|entity| SOLID_KINDS.iter().any(|kind| entity.is(kind)))
            .map(|entity| entity.id)
            .collect(),
    };
    let mut shapes = Vec::new();
    for id in ids {
        if let Some(shape) = shape_source(reader, reader.entity(id)?)? {
            shapes.push(shape);
        }
    }
    Ok((shapes, occurrences))
}
