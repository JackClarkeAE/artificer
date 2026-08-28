//! Validated parametric curves and surfaces.
//!
//! Algorithms operate on owned data and reject malformed parameterizations at
//! construction. Evaluation uses de Casteljau/de Boor rather than power-basis
//! expansions, keeping the implementation stable over the declared domain.

use crate::{Bounds3, Direction3, Point2, Point3, Vector2, Vector3};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeometryError {
    NonFinite,
    EmptyControlNet,
    InvalidDegree,
    InvalidKnotVector,
    InvalidWeight,
    ParameterOutsideDomain,
    SingularEvaluation,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParameterDomain {
    pub start: f64,
    pub end: f64,
    pub periodic: bool,
}

impl ParameterDomain {
    pub fn new(start: f64, end: f64, periodic: bool) -> Result<Self, GeometryError> {
        if !start.is_finite() || !end.is_finite() {
            return Err(GeometryError::NonFinite);
        }
        if start >= end {
            return Err(GeometryError::InvalidKnotVector);
        }
        Ok(Self {
            start,
            end,
            periodic,
        })
    }

    pub fn canonicalize(self, parameter: f64) -> Result<f64, GeometryError> {
        if !parameter.is_finite() {
            return Err(GeometryError::NonFinite);
        }
        if self.periodic {
            let width = self.end - self.start;
            let wrapped = self.start + (parameter - self.start).rem_euclid(width);
            Ok(if parameter == self.end {
                self.end
            } else {
                wrapped
            })
        } else if (self.start..=self.end).contains(&parameter) {
            Ok(parameter)
        } else {
            Err(GeometryError::ParameterOutsideDomain)
        }
    }
}

pub trait ParametricCurve3 {
    fn domain(&self) -> ParameterDomain;
    fn evaluate(&self, parameter: f64) -> Result<Point3, GeometryError>;
    fn derivative(&self, parameter: f64) -> Result<Vector3, GeometryError>;

    fn project(&self, point: Point3) -> Result<(f64, f64), GeometryError> {
        project_curve(self, point)
    }

    fn extrema_parameters(&self, axis: usize) -> Result<Vec<f64>, GeometryError> {
        curve_extrema(self, axis)
    }
}

pub trait ParametricCurve2 {
    fn domain(&self) -> ParameterDomain;
    fn evaluate(&self, parameter: f64) -> Result<Point2, GeometryError>;
    fn derivative(&self, parameter: f64) -> Result<Vector2, GeometryError>;
    fn second_derivative(&self, parameter: f64) -> Result<Vector2, GeometryError>;

    fn curvature(&self, parameter: f64) -> Result<f64, GeometryError> {
        let d = self.derivative(parameter)?;
        let dd = self.second_derivative(parameter)?;
        let speed_sq = d.length_squared();
        let speed_cubed = speed_sq * speed_sq.sqrt();
        if speed_cubed <= 1.0e-14 {
            return Err(GeometryError::SingularEvaluation);
        }
        Ok(d.cross(dd) / speed_cubed)
    }

    fn project(&self, point: Point2) -> Result<(f64, f64), GeometryError> {
        project_curve_2d(self, point)
    }
}

pub trait ParametricSurface3 {
    fn u_domain(&self) -> ParameterDomain;
    fn v_domain(&self) -> ParameterDomain;
    fn evaluate(&self, u: f64, v: f64) -> Result<Point3, GeometryError>;
    fn derivatives(&self, u: f64, v: f64) -> Result<SurfaceDerivatives, GeometryError>;

    fn project(&self, point: Point3) -> Result<([f64; 2], f64), GeometryError> {
        project_surface(self, point)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BezierCurve3 {
    control_points: Vec<Point3>,
}

impl BezierCurve3 {
    pub fn new(control_points: Vec<Point3>) -> Result<Self, GeometryError> {
        validate_points(&control_points)?;
        Ok(Self { control_points })
    }

    pub fn degree(&self) -> usize {
        self.control_points.len() - 1
    }
    pub fn control_points(&self) -> &[Point3] {
        &self.control_points
    }

    pub fn evaluate(&self, parameter: f64) -> Result<Point3, GeometryError> {
        if !parameter.is_finite() {
            return Err(GeometryError::NonFinite);
        }
        if !(0.0..=1.0).contains(&parameter) {
            return Err(GeometryError::ParameterOutsideDomain);
        }
        Ok(de_casteljau(&self.control_points, parameter))
    }

    pub fn derivative(&self, parameter: f64) -> Result<Vector3, GeometryError> {
        if self.degree() == 0 {
            return Ok(Vector3::default());
        }
        let degree = self.degree() as f64;
        let controls = self
            .control_points
            .windows(2)
            .map(|pair| (pair[1] - pair[0]) * degree)
            .collect::<Vec<_>>();
        evaluate_vectors(&controls, parameter)
    }

    pub fn split(&self, parameter: f64) -> Result<(Self, Self), GeometryError> {
        if !parameter.is_finite() {
            return Err(GeometryError::NonFinite);
        }
        if !(0.0..=1.0).contains(&parameter) {
            return Err(GeometryError::ParameterOutsideDomain);
        }
        let mut layer = self.control_points.clone();
        let mut left = vec![layer[0]];
        let mut right = vec![*layer.last().expect("validated nonempty")];
        while layer.len() > 1 {
            layer = layer
                .windows(2)
                .map(|pair| lerp(pair[0], pair[1], parameter))
                .collect();
            left.push(layer[0]);
            right.push(*layer.last().expect("nonempty de Casteljau layer"));
        }
        right.reverse();
        Ok((Self::new(left)?, Self::new(right)?))
    }

    pub fn elevate_degree(&self) -> Result<Self, GeometryError> {
        let degree = self.degree();
        if degree == 0 {
            return Self::new(vec![self.control_points[0], self.control_points[0]]);
        }
        let mut elevated = Vec::with_capacity(self.control_points.len() + 1);
        elevated.push(self.control_points[0]);
        for index in 1..=degree {
            let alpha = index as f64 / (degree + 1) as f64;
            elevated.push(lerp(
                self.control_points[index],
                self.control_points[index - 1],
                alpha,
            ));
        }
        elevated.push(self.control_points[degree]);
        Self::new(elevated)
    }

    pub fn control_bounds(&self) -> Bounds3 {
        bounds(&self.control_points)
    }

    /// Returns the closest sampled/Newton-refined parameter and squared distance.
    pub fn project(&self, point: Point3) -> Result<(f64, f64), GeometryError> {
        if !point.is_finite() {
            return Err(GeometryError::NonFinite);
        }
        let mut best = (0.0, f64::INFINITY);
        for index in 0..=64 {
            let parameter = index as f64 / 64.0;
            let delta = self.evaluate(parameter)? - point;
            let distance = delta.dot(delta);
            if distance < best.1 {
                best = (parameter, distance);
            }
        }
        let mut parameter = best.0;
        for _ in 0..8 {
            let curve = self.evaluate(parameter)?;
            let tangent = self.derivative(parameter)?;
            let denominator = tangent.dot(tangent);
            if denominator <= f64::EPSILON {
                break;
            }
            let next = (parameter - (curve - point).dot(tangent) / denominator).clamp(0.0, 1.0);
            if (next - parameter).abs() <= 1.0e-14 {
                break;
            }
            parameter = next;
        }
        let delta = self.evaluate(parameter)? - point;
        Ok((parameter, delta.dot(delta)))
    }
}

impl ParametricCurve3 for BezierCurve3 {
    fn domain(&self) -> ParameterDomain {
        ParameterDomain {
            start: 0.0,
            end: 1.0,
            periodic: false,
        }
    }
    fn evaluate(&self, parameter: f64) -> Result<Point3, GeometryError> {
        Self::evaluate(self, parameter)
    }
    fn derivative(&self, parameter: f64) -> Result<Vector3, GeometryError> {
        Self::derivative(self, parameter)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BSplineCurve3 {
    degree: usize,
    control_points: Vec<Point3>,
    knots: Vec<f64>,
    periodic: bool,
}

impl BSplineCurve3 {
    pub fn new(
        degree: usize,
        control_points: Vec<Point3>,
        knots: Vec<f64>,
        periodic: bool,
    ) -> Result<Self, GeometryError> {
        validate_spline(degree, &control_points, &knots)?;
        Ok(Self {
            degree,
            control_points,
            knots,
            periodic,
        })
    }
    pub fn degree(&self) -> usize {
        self.degree
    }
    pub fn control_points(&self) -> &[Point3] {
        &self.control_points
    }
    pub fn knots(&self) -> &[f64] {
        &self.knots
    }
    pub fn domain(&self) -> ParameterDomain {
        ParameterDomain {
            start: self.knots[self.degree],
            end: self.knots[self.control_points.len()],
            periodic: self.periodic,
        }
    }
    pub fn evaluate(&self, parameter: f64) -> Result<Point3, GeometryError> {
        let parameter = self.domain().canonicalize(parameter)?;
        Ok(de_boor(
            self.degree,
            &self.control_points,
            &self.knots,
            parameter,
        ))
    }
    pub fn derivative(&self, parameter: f64) -> Result<Vector3, GeometryError> {
        if self.degree == 0 {
            return Ok(Vector3::default());
        }
        let controls = (0..self.control_points.len() - 1)
            .map(|index| {
                let denominator = self.knots[index + self.degree + 1] - self.knots[index + 1];
                if denominator == 0.0 {
                    Vector3::default()
                } else {
                    (self.control_points[index + 1] - self.control_points[index])
                        * (self.degree as f64 / denominator)
                }
            })
            .collect::<Vec<_>>();
        let knots = self.knots[1..self.knots.len() - 1].to_vec();
        evaluate_vector_spline(self.degree - 1, &controls, &knots, parameter, self.periodic)
    }
    pub fn insert_knot(&self, knot: f64) -> Result<Self, GeometryError> {
        let knot = self.domain().canonicalize(knot)?;
        let span = find_span(self.degree, self.control_points.len(), &self.knots, knot);
        let multiplicity = self.knots.iter().filter(|value| **value == knot).count();
        if multiplicity > self.degree {
            return Err(GeometryError::InvalidKnotVector);
        }
        let mut points = vec![Point3::default(); self.control_points.len() + 1];
        points[..=span - self.degree].copy_from_slice(&self.control_points[..=span - self.degree]);
        points[span - multiplicity + 1..]
            .copy_from_slice(&self.control_points[span - multiplicity..]);
        for (index, point) in points
            .iter_mut()
            .enumerate()
            .take(span - multiplicity + 1)
            .skip(span - self.degree + 1)
        {
            let alpha =
                (knot - self.knots[index]) / (self.knots[index + self.degree] - self.knots[index]);
            *point = lerp(
                self.control_points[index - 1],
                self.control_points[index],
                alpha,
            );
        }
        let mut knots = self.knots.clone();
        knots.insert(span + 1, knot);
        Self::new(self.degree, points, knots, self.periodic)
    }
    pub fn control_bounds(&self) -> Bounds3 {
        bounds(&self.control_points)
    }
}

impl ParametricCurve3 for BSplineCurve3 {
    fn domain(&self) -> ParameterDomain {
        Self::domain(self)
    }
    fn evaluate(&self, parameter: f64) -> Result<Point3, GeometryError> {
        Self::evaluate(self, parameter)
    }
    fn derivative(&self, parameter: f64) -> Result<Vector3, GeometryError> {
        Self::derivative(self, parameter)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NurbsCurve3 {
    spline: BSplineCurve3,
    weights: Vec<f64>,
}

impl NurbsCurve3 {
    pub fn new(
        degree: usize,
        control_points: Vec<Point3>,
        weights: Vec<f64>,
        knots: Vec<f64>,
        periodic: bool,
    ) -> Result<Self, GeometryError> {
        if weights.len() != control_points.len()
            || weights
                .iter()
                .any(|weight| !weight.is_finite() || *weight <= 0.0)
        {
            return Err(GeometryError::InvalidWeight);
        }
        Ok(Self {
            spline: BSplineCurve3::new(degree, control_points, knots, periodic)?,
            weights,
        })
    }
    pub fn domain(&self) -> ParameterDomain {
        self.spline.domain()
    }
    pub fn evaluate(&self, parameter: f64) -> Result<Point3, GeometryError> {
        let parameter = self.domain().canonicalize(parameter)?;
        let basis = basis_values(
            self.spline.degree,
            &self.spline.knots,
            self.spline.control_points.len(),
            parameter,
        );
        let mut numerator = Vector3::default();
        let mut denominator = 0.0;
        for ((point, weight), basis) in self
            .spline
            .control_points
            .iter()
            .zip(&self.weights)
            .zip(basis)
        {
            let factor = weight * basis;
            numerator = numerator + point_vector(*point) * factor;
            denominator += factor;
        }
        if !denominator.is_finite() || denominator <= f64::MIN_POSITIVE {
            return Err(GeometryError::SingularEvaluation);
        }
        Ok(vector_point(numerator / denominator))
    }
    pub fn derivative(&self, parameter: f64) -> Result<Vector3, GeometryError> {
        let domain = self.domain();
        let width = domain.end - domain.start;
        let h = (width * 1.0e-6).max(f64::EPSILON.sqrt());
        let lo = (parameter - h).max(domain.start);
        let hi = (parameter + h).min(domain.end);
        Ok((self.evaluate(hi)? - self.evaluate(lo)?) / (hi - lo))
    }
}

impl ParametricCurve3 for NurbsCurve3 {
    fn domain(&self) -> ParameterDomain {
        Self::domain(self)
    }
    fn evaluate(&self, parameter: f64) -> Result<Point3, GeometryError> {
        Self::evaluate(self, parameter)
    }
    fn derivative(&self, parameter: f64) -> Result<Vector3, GeometryError> {
        Self::derivative(self, parameter)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceCurvature {
    pub gaussian: f64,
    pub mean: f64,
    pub principal_min: f64,
    pub principal_max: f64,
    pub normal: Vector3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceDerivatives {
    pub point: Point3,
    pub du: Vector3,
    pub dv: Vector3,
    pub duu: Vector3,
    pub dvv: Vector3,
    pub duv: Vector3,
}

impl SurfaceDerivatives {
    #[must_use]
    pub fn normal(&self) -> Option<Vector3> {
        let n = self.du.cross(self.dv);
        let len = n.length();
        if len.is_finite() && len > 1.0e-12 {
            Some(n / len)
        } else {
            None
        }
    }

    #[must_use]
    pub fn first_fundamental_form(&self) -> (f64, f64, f64) {
        (
            self.du.dot(self.du),
            self.du.dot(self.dv),
            self.dv.dot(self.dv),
        )
    }

    #[must_use]
    pub fn second_fundamental_form(&self) -> Option<(f64, f64, f64)> {
        let n = self.normal()?;
        Some((self.duu.dot(n), self.duv.dot(n), self.dvv.dot(n)))
    }

    #[must_use]
    pub fn curvature(&self) -> Option<SurfaceCurvature> {
        let normal = self.normal()?;
        let (e, f, g) = self.first_fundamental_form();
        let (l, m, n) = self.second_fundamental_form()?;
        let eg_minus_f2 = e * g - f * f;
        if eg_minus_f2.abs() <= 1.0e-14 {
            return None;
        }
        let gaussian = (l * n - m * m) / eg_minus_f2;
        let mean = (e * n - 2.0 * f * m + g * l) / (2.0 * eg_minus_f2);
        let discriminant = (mean * mean - gaussian).max(0.0);
        let root = discriminant.sqrt();
        let k1 = mean - root;
        let k2 = mean + root;
        Some(SurfaceCurvature {
            gaussian,
            mean,
            principal_min: k1.min(k2),
            principal_max: k1.max(k2),
            normal,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BezierSurface3 {
    rows: usize,
    columns: usize,
    control_points: Vec<Point3>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BSplineSurface3 {
    u_degree: usize,
    v_degree: usize,
    rows: usize,
    columns: usize,
    control_points: Vec<Point3>,
    u_knots: Vec<f64>,
    v_knots: Vec<f64>,
}

impl BSplineSurface3 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        u_degree: usize,
        v_degree: usize,
        rows: usize,
        columns: usize,
        control_points: Vec<Point3>,
        u_knots: Vec<f64>,
        v_knots: Vec<f64>,
    ) -> Result<Self, GeometryError> {
        if rows == 0 || columns == 0 || rows.checked_mul(columns) != Some(control_points.len()) {
            return Err(GeometryError::EmptyControlNet);
        }
        validate_spline(u_degree, &control_points[..columns], &u_knots)?;
        let column = (0..rows)
            .map(|row| control_points[row * columns])
            .collect::<Vec<_>>();
        validate_spline(v_degree, &column, &v_knots)?;
        Ok(Self {
            u_degree,
            v_degree,
            rows,
            columns,
            control_points,
            u_knots,
            v_knots,
        })
    }

    pub fn u_domain(&self) -> ParameterDomain {
        ParameterDomain {
            start: self.u_knots[self.u_degree],
            end: self.u_knots[self.columns],
            periodic: false,
        }
    }

    pub fn v_domain(&self) -> ParameterDomain {
        ParameterDomain {
            start: self.v_knots[self.v_degree],
            end: self.v_knots[self.rows],
            periodic: false,
        }
    }

    pub fn evaluate(&self, u: f64, v: f64) -> Result<Point3, GeometryError> {
        let u = self.u_domain().canonicalize(u)?;
        let v = self.v_domain().canonicalize(v)?;
        let across_rows = self
            .control_points
            .chunks(self.columns)
            .map(|row| de_boor(self.u_degree, row, &self.u_knots, u))
            .collect::<Vec<_>>();
        Ok(de_boor(self.v_degree, &across_rows, &self.v_knots, v))
    }

    pub fn derivatives(&self, u: f64, v: f64) -> Result<SurfaceDerivatives, GeometryError> {
        let u = self.u_domain().canonicalize(u)?;
        let v = self.v_domain().canonicalize(v)?;
        let (u_b, u_db, u_ddb) = basis_derivatives(self.u_degree, &self.u_knots, self.columns, u);
        let (v_b, v_db, v_ddb) = basis_derivatives(self.v_degree, &self.v_knots, self.rows, v);

        let mut pt = Vector3::default();
        let mut du = Vector3::default();
        let mut dv = Vector3::default();
        let mut duu = Vector3::default();
        let mut dvv = Vector3::default();
        let mut duv = Vector3::default();

        for row in 0..self.rows {
            let vb = v_b[row];
            let vdb = v_db[row];
            let vddb = v_ddb[row];
            for col in 0..self.columns {
                let ub = u_b[col];
                let udb = u_db[col];
                let uddb = u_ddb[col];
                let p = point_vector(self.control_points[row * self.columns + col]);

                pt = pt + p * (ub * vb);
                du = du + p * (udb * vb);
                dv = dv + p * (ub * vdb);
                duu = duu + p * (uddb * vb);
                dvv = dvv + p * (ub * vddb);
                duv = duv + p * (udb * vdb);
            }
        }

        Ok(SurfaceDerivatives {
            point: vector_point(pt),
            du,
            dv,
            duu,
            dvv,
            duv,
        })
    }

    pub fn insert_knot_u(&self, knot: f64) -> Result<Self, GeometryError> {
        let knot = self.u_domain().canonicalize(knot)?;
        let mut new_points = Vec::with_capacity(self.rows * (self.columns + 1));
        let mut new_u_knots = Vec::new();
        for row in 0..self.rows {
            let row_pts =
                self.control_points[row * self.columns..(row + 1) * self.columns].to_vec();
            let curve = BSplineCurve3::new(self.u_degree, row_pts, self.u_knots.clone(), false)?;
            let inserted = curve.insert_knot(knot)?;
            new_points.extend_from_slice(inserted.control_points());
            if row == 0 {
                new_u_knots = inserted.knots().to_vec();
            }
        }
        Self::new(
            self.u_degree,
            self.v_degree,
            self.rows,
            self.columns + 1,
            new_points,
            new_u_knots,
            self.v_knots.clone(),
        )
    }

    #[allow(clippy::needless_range_loop)]
    pub fn insert_knot_v(&self, knot: f64) -> Result<Self, GeometryError> {
        let knot = self.v_domain().canonicalize(knot)?;
        let mut new_columns_pts = vec![Vec::with_capacity(self.rows + 1); self.columns];
        let mut new_v_knots = Vec::new();
        for col in 0..self.columns {
            let col_pts = (0..self.rows)
                .map(|row| self.control_points[row * self.columns + col])
                .collect::<Vec<_>>();
            let curve = BSplineCurve3::new(self.v_degree, col_pts, self.v_knots.clone(), false)?;
            let inserted = curve.insert_knot(knot)?;
            new_columns_pts[col] = inserted.control_points().to_vec();
            if col == 0 {
                new_v_knots = inserted.knots().to_vec();
            }
        }
        let mut new_points = Vec::with_capacity((self.rows + 1) * self.columns);
        for row in 0..=self.rows {
            for col in 0..self.columns {
                new_points.push(new_columns_pts[col][row]);
            }
        }
        Self::new(
            self.u_degree,
            self.v_degree,
            self.rows + 1,
            self.columns,
            new_points,
            self.u_knots.clone(),
            new_v_knots,
        )
    }

    pub fn control_bounds(&self) -> Bounds3 {
        bounds(&self.control_points)
    }
}

impl ParametricSurface3 for BSplineSurface3 {
    fn u_domain(&self) -> ParameterDomain {
        Self::u_domain(self)
    }
    fn v_domain(&self) -> ParameterDomain {
        Self::v_domain(self)
    }
    fn evaluate(&self, u: f64, v: f64) -> Result<Point3, GeometryError> {
        Self::evaluate(self, u, v)
    }
    fn derivatives(&self, u: f64, v: f64) -> Result<SurfaceDerivatives, GeometryError> {
        Self::derivatives(self, u, v)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NurbsSurface3 {
    spline: BSplineSurface3,
    weights: Vec<f64>,
}

impl NurbsSurface3 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        u_degree: usize,
        v_degree: usize,
        rows: usize,
        columns: usize,
        control_points: Vec<Point3>,
        weights: Vec<f64>,
        u_knots: Vec<f64>,
        v_knots: Vec<f64>,
    ) -> Result<Self, GeometryError> {
        if weights.len() != control_points.len()
            || weights
                .iter()
                .any(|weight| !weight.is_finite() || *weight <= 0.0)
        {
            return Err(GeometryError::InvalidWeight);
        }
        Ok(Self {
            spline: BSplineSurface3::new(
                u_degree,
                v_degree,
                rows,
                columns,
                control_points,
                u_knots,
                v_knots,
            )?,
            weights,
        })
    }

    pub fn evaluate(&self, u: f64, v: f64) -> Result<Point3, GeometryError> {
        let u = self.spline.u_domain().canonicalize(u)?;
        let v = self.spline.v_domain().canonicalize(v)?;
        let u_basis = basis_values(
            self.spline.u_degree,
            &self.spline.u_knots,
            self.spline.columns,
            u,
        );
        let v_basis = basis_values(
            self.spline.v_degree,
            &self.spline.v_knots,
            self.spline.rows,
            v,
        );
        let mut numerator = Vector3::default();
        let mut denominator = 0.0;
        for (row, v_value) in v_basis.into_iter().enumerate() {
            for (column, u_value) in u_basis.iter().copied().enumerate() {
                let index = row * self.spline.columns + column;
                let factor = u_value * v_value * self.weights[index];
                numerator = numerator + point_vector(self.spline.control_points[index]) * factor;
                denominator += factor;
            }
        }
        if !denominator.is_finite() || denominator <= f64::MIN_POSITIVE {
            return Err(GeometryError::SingularEvaluation);
        }
        Ok(vector_point(numerator / denominator))
    }

    pub fn derivatives(&self, u: f64, v: f64) -> Result<SurfaceDerivatives, GeometryError> {
        let u = self.spline.u_domain().canonicalize(u)?;
        let v = self.spline.v_domain().canonicalize(v)?;
        let (u_b, u_db, u_ddb) = basis_derivatives(
            self.spline.u_degree,
            &self.spline.u_knots,
            self.spline.columns,
            u,
        );
        let (v_b, v_db, v_ddb) = basis_derivatives(
            self.spline.v_degree,
            &self.spline.v_knots,
            self.spline.rows,
            v,
        );

        let mut a = Vector3::default();
        let mut a_u = Vector3::default();
        let mut a_v = Vector3::default();
        let mut a_uu = Vector3::default();
        let mut a_vv = Vector3::default();
        let mut a_uv = Vector3::default();

        let mut w = 0.0;
        let mut w_u = 0.0;
        let mut w_v = 0.0;
        let mut w_uu = 0.0;
        let mut w_vv = 0.0;
        let mut w_uv = 0.0;

        for row in 0..self.spline.rows {
            let vb = v_b[row];
            let vdb = v_db[row];
            let vddb = v_ddb[row];
            for col in 0..self.spline.columns {
                let ub = u_b[col];
                let udb = u_db[col];
                let uddb = u_ddb[col];
                let idx = row * self.spline.columns + col;
                let weight = self.weights[idx];
                let p = point_vector(self.spline.control_points[idx]) * weight;

                a = a + p * (ub * vb);
                a_u = a_u + p * (udb * vb);
                a_v = a_v + p * (ub * vdb);
                a_uu = a_uu + p * (uddb * vb);
                a_vv = a_vv + p * (ub * vddb);
                a_uv = a_uv + p * (udb * vdb);

                w += weight * (ub * vb);
                w_u += weight * (udb * vb);
                w_v += weight * (ub * vdb);
                w_uu += weight * (uddb * vb);
                w_vv += weight * (ub * vddb);
                w_uv += weight * (udb * vdb);
            }
        }

        if !w.is_finite() || w <= f64::MIN_POSITIVE {
            return Err(GeometryError::SingularEvaluation);
        }

        let s = a / w;
        let s_u = (a_u - s * w_u) / w;
        let s_v = (a_v - s * w_v) / w;
        let s_uu = (a_uu - s_u * (2.0 * w_u) - s * w_uu) / w;
        let s_vv = (a_vv - s_v * (2.0 * w_v) - s * w_vv) / w;
        let s_uv = (a_uv - s_u * w_v - s_v * w_u - s * w_uv) / w;

        Ok(SurfaceDerivatives {
            point: vector_point(s),
            du: s_u,
            dv: s_v,
            duu: s_uu,
            dvv: s_vv,
            duv: s_uv,
        })
    }

    pub fn insert_knot_u(&self, knot: f64) -> Result<Self, GeometryError> {
        let knot = self.spline.u_domain().canonicalize(knot)?;
        let mut new_points = Vec::with_capacity(self.spline.rows * (self.spline.columns + 1));
        let mut new_weights = Vec::with_capacity(self.spline.rows * (self.spline.columns + 1));
        let mut new_u_knots = Vec::new();

        for row in 0..self.spline.rows {
            let row_pts = (0..self.spline.columns)
                .map(|col| {
                    let idx = row * self.spline.columns + col;
                    let w = self.weights[idx];
                    let pt = self.spline.control_points[idx];
                    Point3::new(pt.x * w, pt.y * w, pt.z * w)
                })
                .collect::<Vec<_>>();
            let row_w = (0..self.spline.columns)
                .map(|col| {
                    let idx = row * self.spline.columns + col;
                    let w = self.weights[idx];
                    Point3::new(w, 0.0, 0.0)
                })
                .collect::<Vec<_>>();

            let curve_pts = BSplineCurve3::new(
                self.spline.u_degree,
                row_pts,
                self.spline.u_knots.clone(),
                false,
            )?;
            let curve_w = BSplineCurve3::new(
                self.spline.u_degree,
                row_w,
                self.spline.u_knots.clone(),
                false,
            )?;

            let ins_pts = curve_pts.insert_knot(knot)?;
            let ins_w = curve_w.insert_knot(knot)?;

            if row == 0 {
                new_u_knots = ins_pts.knots().to_vec();
            }

            for col in 0..=self.spline.columns {
                let w = ins_w.control_points()[col].x;
                let pw = ins_pts.control_points()[col];
                new_weights.push(w);
                new_points.push(Point3::new(pw.x / w, pw.y / w, pw.z / w));
            }
        }

        Self::new(
            self.spline.u_degree,
            self.spline.v_degree,
            self.spline.rows,
            self.spline.columns + 1,
            new_points,
            new_weights,
            new_u_knots,
            self.spline.v_knots.clone(),
        )
    }

    #[allow(clippy::needless_range_loop)]
    pub fn insert_knot_v(&self, knot: f64) -> Result<Self, GeometryError> {
        let knot = self.spline.v_domain().canonicalize(knot)?;
        let mut new_col_pts = vec![Vec::with_capacity(self.spline.rows + 1); self.spline.columns];
        let mut new_col_w = vec![Vec::with_capacity(self.spline.rows + 1); self.spline.columns];
        let mut new_v_knots = Vec::new();

        for col in 0..self.spline.columns {
            let col_pts = (0..self.spline.rows)
                .map(|row| {
                    let idx = row * self.spline.columns + col;
                    let w = self.weights[idx];
                    let pt = self.spline.control_points[idx];
                    Point3::new(pt.x * w, pt.y * w, pt.z * w)
                })
                .collect::<Vec<_>>();
            let col_w = (0..self.spline.rows)
                .map(|row| {
                    let idx = row * self.spline.columns + col;
                    let w = self.weights[idx];
                    Point3::new(w, 0.0, 0.0)
                })
                .collect::<Vec<_>>();

            let curve_pts = BSplineCurve3::new(
                self.spline.v_degree,
                col_pts,
                self.spline.v_knots.clone(),
                false,
            )?;
            let curve_w = BSplineCurve3::new(
                self.spline.v_degree,
                col_w,
                self.spline.v_knots.clone(),
                false,
            )?;

            let ins_pts = curve_pts.insert_knot(knot)?;
            let ins_w = curve_w.insert_knot(knot)?;

            if col == 0 {
                new_v_knots = ins_pts.knots().to_vec();
            }

            for row in 0..=self.spline.rows {
                let w = ins_w.control_points()[row].x;
                let pw = ins_pts.control_points()[row];
                new_col_w[col].push(w);
                new_col_pts[col].push(Point3::new(pw.x / w, pw.y / w, pw.z / w));
            }
        }

        let mut new_points = Vec::with_capacity((self.spline.rows + 1) * self.spline.columns);
        let mut new_weights = Vec::with_capacity((self.spline.rows + 1) * self.spline.columns);
        for row in 0..=self.spline.rows {
            for col in 0..self.spline.columns {
                new_points.push(new_col_pts[col][row]);
                new_weights.push(new_col_w[col][row]);
            }
        }

        Self::new(
            self.spline.u_degree,
            self.spline.v_degree,
            self.spline.rows + 1,
            self.spline.columns,
            new_points,
            new_weights,
            self.spline.u_knots.clone(),
            new_v_knots,
        )
    }
}

impl ParametricSurface3 for NurbsSurface3 {
    fn u_domain(&self) -> ParameterDomain {
        self.spline.u_domain()
    }
    fn v_domain(&self) -> ParameterDomain {
        self.spline.v_domain()
    }
    fn evaluate(&self, u: f64, v: f64) -> Result<Point3, GeometryError> {
        Self::evaluate(self, u, v)
    }
    fn derivatives(&self, u: f64, v: f64) -> Result<SurfaceDerivatives, GeometryError> {
        Self::derivatives(self, u, v)
    }
}

impl BezierSurface3 {
    pub fn new(
        rows: usize,
        columns: usize,
        control_points: Vec<Point3>,
    ) -> Result<Self, GeometryError> {
        if rows == 0 || columns == 0 || rows.checked_mul(columns) != Some(control_points.len()) {
            return Err(GeometryError::EmptyControlNet);
        }
        if control_points.iter().any(|point| !point.is_finite()) {
            return Err(GeometryError::NonFinite);
        }
        Ok(Self {
            rows,
            columns,
            control_points,
        })
    }
    pub fn evaluate(&self, u: f64, v: f64) -> Result<Point3, GeometryError> {
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return Err(GeometryError::ParameterOutsideDomain);
        }
        let row_points = self
            .control_points
            .chunks(self.columns)
            .map(|row| de_casteljau(row, u))
            .collect::<Vec<_>>();
        Ok(de_casteljau(&row_points, v))
    }
    pub fn derivatives(&self, u: f64, v: f64) -> Result<SurfaceDerivatives, GeometryError> {
        numerical_surface_derivatives(
            |first, second| self.evaluate(first, second),
            self.u_domain(),
            self.v_domain(),
            u,
            v,
        )
    }
    pub fn control_bounds(&self) -> Bounds3 {
        bounds(&self.control_points)
    }

    pub fn split_u(&self, parameter: f64) -> Result<(Self, Self), GeometryError> {
        if !(0.0..=1.0).contains(&parameter) {
            return Err(GeometryError::ParameterOutsideDomain);
        }
        let splits = self
            .control_points
            .chunks(self.columns)
            .map(|row| BezierCurve3::new(row.to_vec())?.split(parameter))
            .collect::<Result<Vec<_>, GeometryError>>()?;
        let left = splits
            .iter()
            .flat_map(|pair| pair.0.control_points.iter().copied())
            .collect();
        let right = splits
            .iter()
            .flat_map(|pair| pair.1.control_points.iter().copied())
            .collect();
        Ok((
            Self::new(self.rows, self.columns, left)?,
            Self::new(self.rows, self.columns, right)?,
        ))
    }

    pub fn split_v(&self, parameter: f64) -> Result<(Self, Self), GeometryError> {
        if !(0.0..=1.0).contains(&parameter) {
            return Err(GeometryError::ParameterOutsideDomain);
        }
        let mut left = vec![Point3::default(); self.control_points.len()];
        let mut right = left.clone();
        for column in 0..self.columns {
            let controls = (0..self.rows)
                .map(|row| self.control_points[row * self.columns + column])
                .collect::<Vec<_>>();
            let (first, second) = BezierCurve3::new(controls)?.split(parameter)?;
            for row in 0..self.rows {
                left[row * self.columns + column] = first.control_points[row];
                right[row * self.columns + column] = second.control_points[row];
            }
        }
        Ok((
            Self::new(self.rows, self.columns, left)?,
            Self::new(self.rows, self.columns, right)?,
        ))
    }
}

impl ParametricSurface3 for BezierSurface3 {
    fn u_domain(&self) -> ParameterDomain {
        ParameterDomain {
            start: 0.0,
            end: 1.0,
            periodic: false,
        }
    }
    fn v_domain(&self) -> ParameterDomain {
        ParameterDomain {
            start: 0.0,
            end: 1.0,
            periodic: false,
        }
    }
    fn evaluate(&self, u: f64, v: f64) -> Result<Point3, GeometryError> {
        Self::evaluate(self, u, v)
    }
    fn derivatives(&self, u: f64, v: f64) -> Result<SurfaceDerivatives, GeometryError> {
        Self::derivatives(self, u, v)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnalyticSurface {
    Plane {
        origin: Point3,
        u: Direction3,
        v: Direction3,
    },
    Cylinder {
        origin: Point3,
        axis: Direction3,
        radial: Direction3,
        radius: f64,
    },
    Cone {
        apex: Point3,
        axis: Direction3,
        radial: Direction3,
        half_angle: f64,
    },
    Sphere {
        center: Point3,
        radius: f64,
    },
    Torus {
        center: Point3,
        axis: Direction3,
        radial: Direction3,
        major_radius: f64,
        minor_radius: f64,
    },
}

impl AnalyticSurface {
    pub fn evaluate(self, u: f64, v: f64) -> Result<Point3, GeometryError> {
        if !u.is_finite() || !v.is_finite() {
            return Err(GeometryError::NonFinite);
        }
        match self {
            Self::Plane {
                origin,
                u: axis_u,
                v: axis_v,
            } => Ok(origin + axis_u.vector() * u + axis_v.vector() * v),
            Self::Cylinder {
                origin,
                axis,
                radial,
                radius,
            } => {
                valid_positive(radius)?;
                let tangent = axis.vector().cross(radial.vector());
                Ok(origin
                    + radial.vector() * (radius * u.cos())
                    + tangent * (radius * u.sin())
                    + axis.vector() * v)
            }
            Self::Cone {
                apex,
                axis,
                radial,
                half_angle,
            } => {
                if !half_angle.is_finite()
                    || !(0.0..std::f64::consts::FRAC_PI_2).contains(&half_angle)
                {
                    return Err(GeometryError::InvalidWeight);
                }
                let tangent = axis.vector().cross(radial.vector());
                let radius = v * half_angle.tan();
                Ok(apex
                    + axis.vector() * v
                    + radial.vector() * (radius * u.cos())
                    + tangent * (radius * u.sin()))
            }
            Self::Sphere { center, radius } => {
                valid_positive(radius)?;
                Ok(center
                    + Vector3::new(
                        radius * v.cos() * u.cos(),
                        radius * v.cos() * u.sin(),
                        radius * v.sin(),
                    ))
            }
            Self::Torus {
                center,
                axis,
                radial,
                major_radius,
                minor_radius,
            } => {
                valid_positive(major_radius)?;
                valid_positive(minor_radius)?;
                if minor_radius >= major_radius {
                    return Err(GeometryError::InvalidWeight);
                }
                let tangent = axis.vector().cross(radial.vector());
                let ring = radial.vector() * u.cos() + tangent * u.sin();
                Ok(center
                    + ring * (major_radius + minor_radius * v.cos())
                    + axis.vector() * (minor_radius * v.sin()))
            }
        }
    }
}

fn valid_positive(value: f64) -> Result<(), GeometryError> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(GeometryError::InvalidWeight)
    }
}

fn numerical_surface_derivatives(
    evaluate: impl Fn(f64, f64) -> Result<Point3, GeometryError>,
    u_domain: ParameterDomain,
    v_domain: ParameterDomain,
    u: f64,
    v: f64,
) -> Result<SurfaceDerivatives, GeometryError> {
    let u = u_domain.canonicalize(u)?;
    let v = v_domain.canonicalize(v)?;
    let u_h = ((u_domain.end - u_domain.start) * 1.0e-5).max(f64::EPSILON.sqrt());
    let v_h = ((v_domain.end - v_domain.start) * 1.0e-5).max(f64::EPSILON.sqrt());
    let u_lo = (u - u_h).max(u_domain.start);
    let u_hi = (u + u_h).min(u_domain.end);
    let v_lo = (v - v_h).max(v_domain.start);
    let v_hi = (v + v_h).min(v_domain.end);

    let p00 = evaluate(u, v)?;
    let pu_hi = evaluate(u_hi, v)?;
    let pu_lo = evaluate(u_lo, v)?;
    let pv_hi = evaluate(u, v_hi)?;
    let pv_lo = evaluate(u, v_lo)?;
    let puv_hi = evaluate(u_hi, v_hi)?;
    let puv_lo = evaluate(u_lo, v_lo)?;
    let pu_hi_v_lo = evaluate(u_hi, v_lo)?;
    let pu_lo_v_hi = evaluate(u_lo, v_hi)?;

    let du = (pu_hi - pu_lo) / (u_hi - u_lo);
    let dv = (pv_hi - pv_lo) / (v_hi - v_lo);

    let u_denom = (u_hi - u) * (u - u_lo);
    let v_denom = (v_hi - v) * (v - v_lo);
    let duu = if u_denom > 0.0 {
        ((pu_hi - p00) - (p00 - pu_lo)) / u_denom
    } else {
        Vector3::default()
    };
    let dvv = if v_denom > 0.0 {
        ((pv_hi - p00) - (p00 - pv_lo)) / v_denom
    } else {
        Vector3::default()
    };
    let duv = ((puv_hi - pu_hi_v_lo) - (pu_lo_v_hi - puv_lo)) / ((u_hi - u_lo) * (v_hi - v_lo));

    Ok(SurfaceDerivatives {
        point: p00,
        du,
        dv,
        duu,
        dvv,
        duv,
    })
}

fn project_curve<C: ParametricCurve3 + ?Sized>(
    curve: &C,
    point: Point3,
) -> Result<(f64, f64), GeometryError> {
    if !point.is_finite() {
        return Err(GeometryError::NonFinite);
    }
    let domain = curve.domain();
    let mut best = (domain.start, f64::INFINITY);
    for index in 0..=128 {
        let parameter = domain.start + (domain.end - domain.start) * index as f64 / 128.0;
        let delta = curve.evaluate(parameter)? - point;
        let distance = delta.dot(delta);
        if distance < best.1 {
            best = (parameter, distance);
        }
    }
    let mut parameter = best.0;
    for _ in 0..12 {
        let curve_point = curve.evaluate(parameter)?;
        let tangent = curve.derivative(parameter)?;
        let denominator = tangent.dot(tangent);
        if denominator <= f64::EPSILON {
            break;
        }
        let next = (parameter - (curve_point - point).dot(tangent) / denominator)
            .clamp(domain.start, domain.end);
        if (next - parameter).abs() <= 1.0e-14 {
            break;
        }
        parameter = next;
    }
    let delta = curve.evaluate(parameter)? - point;
    Ok((parameter, delta.dot(delta)))
}

fn curve_extrema<C: ParametricCurve3 + ?Sized>(
    curve: &C,
    axis: usize,
) -> Result<Vec<f64>, GeometryError> {
    if axis > 2 {
        return Err(GeometryError::InvalidDegree);
    }
    let component = |vector: Vector3| match axis {
        0 => vector.x,
        1 => vector.y,
        _ => vector.z,
    };
    let domain = curve.domain();
    let mut result = vec![domain.start];
    let mut prior_t = domain.start;
    let mut prior = component(curve.derivative(prior_t)?);
    for index in 1..=256 {
        let parameter = domain.start + (domain.end - domain.start) * index as f64 / 256.0;
        let value = component(curve.derivative(parameter)?);
        if value == 0.0 {
            result.push(parameter);
        } else if prior.signum() != value.signum() {
            let (mut low, mut high, mut low_value) = (prior_t, parameter, prior);
            for _ in 0..60 {
                let middle = (low + high) * 0.5;
                let middle_value = component(curve.derivative(middle)?);
                if middle_value == 0.0 {
                    low = middle;
                    high = middle;
                    break;
                }
                if low_value.signum() == middle_value.signum() {
                    low = middle;
                    low_value = middle_value;
                } else {
                    high = middle;
                }
            }
            result.push((low + high) * 0.5);
        }
        prior_t = parameter;
        prior = value;
    }
    result.push(domain.end);
    result.sort_by(f64::total_cmp);
    result.dedup_by(|left, right| (*left - *right).abs() <= 1.0e-12 * (domain.end - domain.start));
    Ok(result)
}

fn project_surface<S: ParametricSurface3 + ?Sized>(
    surface: &S,
    point: Point3,
) -> Result<([f64; 2], f64), GeometryError> {
    if !point.is_finite() {
        return Err(GeometryError::NonFinite);
    }
    let (u_domain, v_domain) = (surface.u_domain(), surface.v_domain());
    let mut best = ([u_domain.start, v_domain.start], f64::INFINITY);
    for row in 0..=16 {
        for column in 0..=16 {
            let uv = [
                u_domain.start + (u_domain.end - u_domain.start) * column as f64 / 16.0,
                v_domain.start + (v_domain.end - v_domain.start) * row as f64 / 16.0,
            ];
            let delta = surface.evaluate(uv[0], uv[1])? - point;
            let distance = delta.dot(delta);
            if distance < best.1 {
                best = (uv, distance);
            }
        }
    }
    let mut uv = best.0;
    for _ in 0..12 {
        let derivatives = surface.derivatives(uv[0], uv[1])?;
        let residual = derivatives.point - point;
        let (a, b, c) = (
            derivatives.du.dot(derivatives.du),
            derivatives.du.dot(derivatives.dv),
            derivatives.dv.dot(derivatives.dv),
        );
        let determinant = a * c - b * b;
        if determinant.abs() <= f64::EPSILON {
            break;
        }
        let (rhs_u, rhs_v) = (-residual.dot(derivatives.du), -residual.dot(derivatives.dv));
        let next = [
            (uv[0] + (rhs_u * c - rhs_v * b) / determinant).clamp(u_domain.start, u_domain.end),
            (uv[1] + (rhs_v * a - rhs_u * b) / determinant).clamp(v_domain.start, v_domain.end),
        ];
        if (next[0] - uv[0]).abs().max((next[1] - uv[1]).abs()) <= 1.0e-13 {
            uv = next;
            break;
        }
        uv = next;
    }
    let delta = surface.evaluate(uv[0], uv[1])? - point;
    Ok((uv, delta.dot(delta)))
}
fn validate_points(points: &[Point3]) -> Result<(), GeometryError> {
    if points.is_empty() {
        Err(GeometryError::EmptyControlNet)
    } else if points.iter().any(|point| !point.is_finite()) {
        Err(GeometryError::NonFinite)
    } else {
        Ok(())
    }
}
fn validate_spline(degree: usize, points: &[Point3], knots: &[f64]) -> Result<(), GeometryError> {
    validate_points(points)?;
    if degree == 0 || degree >= points.len() {
        return Err(GeometryError::InvalidDegree);
    }
    if knots.len() != points.len() + degree + 1
        || knots.iter().any(|knot| !knot.is_finite())
        || knots.windows(2).any(|pair| pair[0] > pair[1])
        || knots[degree] >= knots[points.len()]
    {
        return Err(GeometryError::InvalidKnotVector);
    }
    Ok(())
}
fn lerp(a: Point3, b: Point3, t: f64) -> Point3 {
    a + (b - a) * t
}
fn de_casteljau(points: &[Point3], parameter: f64) -> Point3 {
    let mut layer = points.to_vec();
    for width in (1..layer.len()).rev() {
        for index in 0..width {
            layer[index] = lerp(layer[index], layer[index + 1], parameter);
        }
    }
    layer[0]
}
fn evaluate_vectors(points: &[Vector3], parameter: f64) -> Result<Vector3, GeometryError> {
    if !(0.0..=1.0).contains(&parameter) {
        return Err(GeometryError::ParameterOutsideDomain);
    }
    let mut layer = points.to_vec();
    for width in (1..layer.len()).rev() {
        for index in 0..width {
            layer[index] = layer[index] * (1.0 - parameter) + layer[index + 1] * parameter;
        }
    }
    Ok(layer[0])
}
fn bounds(points: &[Point3]) -> Bounds3 {
    let mut min = points[0];
    let mut max = points[0];
    for point in &points[1..] {
        min.x = min.x.min(point.x);
        min.y = min.y.min(point.y);
        min.z = min.z.min(point.z);
        max.x = max.x.max(point.x);
        max.y = max.y.max(point.y);
        max.z = max.z.max(point.z);
    }
    Bounds3::new(min, max).expect("validated finite points yield bounds")
}
fn find_span(degree: usize, count: usize, knots: &[f64], parameter: f64) -> usize {
    if parameter == knots[count] {
        return count - 1;
    }
    let mut low = degree;
    let mut high = count;
    while high - low > 1 {
        let mid = (low + high) / 2;
        if parameter < knots[mid] {
            high = mid;
        } else {
            low = mid;
        }
    }
    low
}
fn de_boor(degree: usize, points: &[Point3], knots: &[f64], parameter: f64) -> Point3 {
    let span = find_span(degree, points.len(), knots, parameter);
    let mut work = (0..=degree)
        .map(|index| points[span - degree + index])
        .collect::<Vec<_>>();
    for level in 1..=degree {
        for index in (level..=degree).rev() {
            let source = span - degree + index;
            let denominator = knots[source + degree - level + 1] - knots[source];
            let alpha = if denominator == 0.0 {
                0.0
            } else {
                (parameter - knots[source]) / denominator
            };
            work[index] = lerp(work[index - 1], work[index], alpha);
        }
    }
    work[degree]
}
fn evaluate_vector_spline(
    degree: usize,
    points: &[Vector3],
    knots: &[f64],
    parameter: f64,
    periodic: bool,
) -> Result<Vector3, GeometryError> {
    let point_controls = points.iter().map(|v| vector_point(*v)).collect::<Vec<_>>();
    let spline = BSplineCurve3::new(degree, point_controls, knots.to_vec(), periodic)?;
    Ok(point_vector(spline.evaluate(parameter)?))
}
#[must_use]
pub fn basis_values(degree: usize, knots: &[f64], count: usize, parameter: f64) -> Vec<f64> {
    let mut basis = vec![0.0; count];
    let span = find_span(degree, count, knots, parameter);
    basis[span] = 1.0;
    for level in 1..=degree {
        let prior = basis.clone();
        for index in span.saturating_sub(level)..=span {
            let left_den = knots[index + level] - knots[index];
            let right_den = knots[index + level + 1] - knots[index + 1];
            let left = if left_den == 0.0 {
                0.0
            } else {
                (parameter - knots[index]) / left_den * prior[index]
            };
            let right = if index + 1 >= count || right_den == 0.0 {
                0.0
            } else {
                (knots[index + level + 1] - parameter) / right_den * prior[index + 1]
            };
            basis[index] = left + right;
        }
    }
    basis
}
#[derive(Clone, Debug, PartialEq)]
pub struct BSplineCurve2 {
    degree: usize,
    control_points: Vec<Point2>,
    knots: Vec<f64>,
    periodic: bool,
}

impl BSplineCurve2 {
    pub fn new(
        degree: usize,
        control_points: Vec<Point2>,
        knots: Vec<f64>,
        periodic: bool,
    ) -> Result<Self, GeometryError> {
        validate_spline_2d(degree, &control_points, &knots)?;
        Ok(Self {
            degree,
            control_points,
            knots,
            periodic,
        })
    }

    #[must_use]
    pub const fn degree(&self) -> usize {
        self.degree
    }

    #[must_use]
    pub fn control_points(&self) -> &[Point2] {
        &self.control_points
    }

    #[must_use]
    pub fn knots(&self) -> &[f64] {
        &self.knots
    }

    #[must_use]
    pub fn domain(&self) -> ParameterDomain {
        ParameterDomain {
            start: self.knots[self.degree],
            end: self.knots[self.control_points.len()],
            periodic: self.periodic,
        }
    }

    pub fn evaluate(&self, parameter: f64) -> Result<Point2, GeometryError> {
        let parameter = self.domain().canonicalize(parameter)?;
        Ok(de_boor_2d(
            self.degree,
            &self.control_points,
            &self.knots,
            parameter,
        ))
    }

    pub fn derivative(&self, parameter: f64) -> Result<Vector2, GeometryError> {
        let parameter = self.domain().canonicalize(parameter)?;
        let (_, db, _) = basis_derivatives(
            self.degree,
            &self.knots,
            self.control_points.len(),
            parameter,
        );
        let mut d = Vector2::default();
        for (pt, factor) in self.control_points.iter().zip(db) {
            d.x += pt.x * factor;
            d.y += pt.y * factor;
        }
        Ok(d)
    }

    pub fn second_derivative(&self, parameter: f64) -> Result<Vector2, GeometryError> {
        let parameter = self.domain().canonicalize(parameter)?;
        let (_, _, ddb) = basis_derivatives(
            self.degree,
            &self.knots,
            self.control_points.len(),
            parameter,
        );
        let mut dd = Vector2::default();
        for (pt, factor) in self.control_points.iter().zip(ddb) {
            dd.x += pt.x * factor;
            dd.y += pt.y * factor;
        }
        Ok(dd)
    }

    pub fn insert_knot(&self, knot: f64) -> Result<Self, GeometryError> {
        let knot = self.domain().canonicalize(knot)?;
        let span = find_span(self.degree, self.control_points.len(), &self.knots, knot);
        let multiplicity = self.knots.iter().filter(|value| **value == knot).count();
        if multiplicity > self.degree {
            return Err(GeometryError::InvalidKnotVector);
        }
        let mut points = vec![Point2::default(); self.control_points.len() + 1];
        points[..=span - self.degree].copy_from_slice(&self.control_points[..=span - self.degree]);
        points[span - multiplicity + 1..]
            .copy_from_slice(&self.control_points[span - multiplicity..]);
        for (index, point) in points
            .iter_mut()
            .enumerate()
            .take(span - multiplicity + 1)
            .skip(span - self.degree + 1)
        {
            let alpha =
                (knot - self.knots[index]) / (self.knots[index + self.degree] - self.knots[index]);
            *point = lerp_2d(
                self.control_points[index - 1],
                self.control_points[index],
                alpha,
            );
        }
        let mut knots = self.knots.clone();
        knots.insert(span + 1, knot);
        Self::new(self.degree, points, knots, self.periodic)
    }
}

impl ParametricCurve2 for BSplineCurve2 {
    fn domain(&self) -> ParameterDomain {
        Self::domain(self)
    }
    fn evaluate(&self, parameter: f64) -> Result<Point2, GeometryError> {
        Self::evaluate(self, parameter)
    }
    fn derivative(&self, parameter: f64) -> Result<Vector2, GeometryError> {
        Self::derivative(self, parameter)
    }
    fn second_derivative(&self, parameter: f64) -> Result<Vector2, GeometryError> {
        Self::second_derivative(self, parameter)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NurbsCurve2 {
    spline: BSplineCurve2,
    weights: Vec<f64>,
}

impl NurbsCurve2 {
    pub fn new(
        degree: usize,
        control_points: Vec<Point2>,
        weights: Vec<f64>,
        knots: Vec<f64>,
        periodic: bool,
    ) -> Result<Self, GeometryError> {
        if weights.len() != control_points.len()
            || weights
                .iter()
                .any(|weight| !weight.is_finite() || *weight <= 0.0)
        {
            return Err(GeometryError::InvalidWeight);
        }
        Ok(Self {
            spline: BSplineCurve2::new(degree, control_points, knots, periodic)?,
            weights,
        })
    }

    #[must_use]
    pub const fn degree(&self) -> usize {
        self.spline.degree()
    }

    #[must_use]
    pub fn control_points(&self) -> &[Point2] {
        self.spline.control_points()
    }

    #[must_use]
    pub fn weights(&self) -> &[f64] {
        &self.weights
    }

    #[must_use]
    pub fn knots(&self) -> &[f64] {
        self.spline.knots()
    }

    #[must_use]
    pub fn domain(&self) -> ParameterDomain {
        self.spline.domain()
    }

    pub fn evaluate(&self, parameter: f64) -> Result<Point2, GeometryError> {
        let parameter = self.domain().canonicalize(parameter)?;
        let basis = basis_values(
            self.spline.degree,
            &self.spline.knots,
            self.spline.control_points.len(),
            parameter,
        );
        let mut x_num = 0.0;
        let mut y_num = 0.0;
        let mut denominator = 0.0;
        for ((point, weight), b) in self
            .spline
            .control_points
            .iter()
            .zip(&self.weights)
            .zip(basis)
        {
            let factor = weight * b;
            x_num += point.x * factor;
            y_num += point.y * factor;
            denominator += factor;
        }
        if !denominator.is_finite() || denominator <= f64::MIN_POSITIVE {
            return Err(GeometryError::SingularEvaluation);
        }
        Ok(Point2::new(x_num / denominator, y_num / denominator))
    }

    pub fn derivative(&self, parameter: f64) -> Result<Vector2, GeometryError> {
        let parameter = self.domain().canonicalize(parameter)?;
        let (basis, d_basis, _) = basis_derivatives(
            self.spline.degree,
            &self.spline.knots,
            self.spline.control_points.len(),
            parameter,
        );
        let mut a = Vector2::default();
        let mut a_prime = Vector2::default();
        let mut w = 0.0;
        let mut w_prime = 0.0;

        for ((point, weight), (b, db)) in self
            .spline
            .control_points
            .iter()
            .zip(&self.weights)
            .zip(basis.into_iter().zip(d_basis))
        {
            let wb = weight * b;
            let wdb = weight * db;
            a.x += point.x * wb;
            a.y += point.y * wb;
            a_prime.x += point.x * wdb;
            a_prime.y += point.y * wdb;
            w += wb;
            w_prime += wdb;
        }

        if !w.is_finite() || w <= f64::MIN_POSITIVE {
            return Err(GeometryError::SingularEvaluation);
        }

        Ok((a_prime * w - a * w_prime) / (w * w))
    }

    pub fn second_derivative(&self, parameter: f64) -> Result<Vector2, GeometryError> {
        let parameter = self.domain().canonicalize(parameter)?;
        let (basis, d_basis, dd_basis) = basis_derivatives(
            self.spline.degree,
            &self.spline.knots,
            self.spline.control_points.len(),
            parameter,
        );
        let mut a = Vector2::default();
        let mut a_p = Vector2::default();
        let mut a_pp = Vector2::default();
        let mut w = 0.0;
        let mut w_p = 0.0;
        let mut w_pp = 0.0;

        for ((point, weight), ((b, db), ddb)) in self
            .spline
            .control_points
            .iter()
            .zip(&self.weights)
            .zip(basis.into_iter().zip(d_basis).zip(dd_basis))
        {
            let wb = weight * b;
            let wdb = weight * db;
            let wddb = weight * ddb;

            a.x += point.x * wb;
            a.y += point.y * wb;
            a_p.x += point.x * wdb;
            a_p.y += point.y * wdb;
            a_pp.x += point.x * wddb;
            a_pp.y += point.y * wddb;

            w += wb;
            w_p += wdb;
            w_pp += wddb;
        }

        if !w.is_finite() || w <= f64::MIN_POSITIVE {
            return Err(GeometryError::SingularEvaluation);
        }

        let c = a / w;
        let c_p = (a_p - c * w_p) / w;
        let c_pp = (a_pp - c_p * (2.0 * w_p) - c * w_pp) / w;
        Ok(c_pp)
    }

    pub fn insert_knot(&self, knot: f64) -> Result<Self, GeometryError> {
        let knot = self.domain().canonicalize(knot)?;
        let pts_w = self
            .spline
            .control_points
            .iter()
            .zip(&self.weights)
            .map(|(pt, w)| Point3::new(pt.x * w, pt.y * w, *w))
            .collect::<Vec<_>>();
        let curve_3d =
            BSplineCurve3::new(self.spline.degree, pts_w, self.spline.knots.clone(), false)?;
        let inserted = curve_3d.insert_knot(knot)?;

        let mut new_points = Vec::with_capacity(inserted.control_points().len());
        let mut new_weights = Vec::with_capacity(inserted.control_points().len());
        for pt3 in inserted.control_points() {
            let w = pt3.z;
            new_weights.push(w);
            new_points.push(Point2::new(pt3.x / w, pt3.y / w));
        }

        Self::new(
            self.spline.degree,
            new_points,
            new_weights,
            inserted.knots().to_vec(),
            self.spline.periodic,
        )
    }
}

impl ParametricCurve2 for NurbsCurve2 {
    fn domain(&self) -> ParameterDomain {
        Self::domain(self)
    }
    fn evaluate(&self, parameter: f64) -> Result<Point2, GeometryError> {
        Self::evaluate(self, parameter)
    }
    fn derivative(&self, parameter: f64) -> Result<Vector2, GeometryError> {
        Self::derivative(self, parameter)
    }
    fn second_derivative(&self, parameter: f64) -> Result<Vector2, GeometryError> {
        Self::second_derivative(self, parameter)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParametricMesh3 {
    pub vertices: Vec<Point3>,
    pub normals: Vec<Vector3>,
    pub uvs: Vec<[f64; 2]>,
    pub indices: Vec<[usize; 3]>,
}

pub fn tessellate_surface<S: ParametricSurface3 + ?Sized>(
    surface: &S,
    u_segments: usize,
    v_segments: usize,
) -> Result<ParametricMesh3, GeometryError> {
    let u_domain = surface.u_domain();
    let v_domain = surface.v_domain();
    let u_count = u_segments.max(2);
    let v_count = v_segments.max(2);
    let mut vertices = Vec::with_capacity((u_count + 1) * (v_count + 1));
    let mut normals = Vec::with_capacity((u_count + 1) * (v_count + 1));
    let mut uvs = Vec::with_capacity((u_count + 1) * (v_count + 1));
    let mut indices = Vec::with_capacity(u_count * v_count * 2);

    for j in 0..=v_count {
        let v_frac = j as f64 / v_count as f64;
        let v = v_domain.start + (v_domain.end - v_domain.start) * v_frac;
        for i in 0..=u_count {
            let u_frac = i as f64 / u_count as f64;
            let u = u_domain.start + (u_domain.end - u_domain.start) * u_frac;
            let derivs = surface.derivatives(u, v)?;
            vertices.push(derivs.point);
            normals.push(
                derivs
                    .normal()
                    .unwrap_or_else(|| Vector3::new(0.0, 0.0, 1.0)),
            );
            uvs.push([u, v]);
        }
    }

    for j in 0..v_count {
        for i in 0..u_count {
            let row1 = j * (u_count + 1);
            let row2 = (j + 1) * (u_count + 1);
            let p0 = row1 + i;
            let p1 = row1 + i + 1;
            let p2 = row2 + i + 1;
            let p3 = row2 + i;
            indices.push([p0, p1, p2]);
            indices.push([p0, p2, p3]);
        }
    }

    Ok(ParametricMesh3 {
        vertices,
        normals,
        uvs,
        indices,
    })
}

pub fn adaptive_tessellate_surface<S: ParametricSurface3 + ?Sized>(
    surface: &S,
    chordal_tolerance: f64,
    max_depth: usize,
) -> Result<ParametricMesh3, GeometryError> {
    let u_dom = surface.u_domain();
    let v_dom = surface.v_domain();
    let mut mesh = ParametricMesh3::default();

    #[allow(clippy::too_many_arguments)]
    fn subdivide<S: ParametricSurface3 + ?Sized>(
        surface: &S,
        u_min: f64,
        u_max: f64,
        v_min: f64,
        v_max: f64,
        tol: f64,
        depth: usize,
        max_d: usize,
        mesh: &mut ParametricMesh3,
    ) -> Result<(), GeometryError> {
        let u_mid = (u_min + u_max) * 0.5;
        let v_mid = (v_min + v_max) * 0.5;

        let d00 = surface.derivatives(u_min, v_min)?;
        let d10 = surface.derivatives(u_max, v_min)?;
        let d11 = surface.derivatives(u_max, v_max)?;
        let d01 = surface.derivatives(u_min, v_max)?;
        let d_mid = surface.derivatives(u_mid, v_mid)?;

        let approx_mid = (point_vector(d00.point)
            + point_vector(d10.point)
            + point_vector(d11.point)
            + point_vector(d01.point))
            * 0.25;
        let diff = (point_vector(d_mid.point) - approx_mid).length();

        if diff > tol && depth < max_d {
            subdivide(
                surface,
                u_min,
                u_mid,
                v_min,
                v_mid,
                tol,
                depth + 1,
                max_d,
                mesh,
            )?;
            subdivide(
                surface,
                u_mid,
                u_max,
                v_min,
                v_mid,
                tol,
                depth + 1,
                max_d,
                mesh,
            )?;
            subdivide(
                surface,
                u_min,
                u_mid,
                v_mid,
                v_max,
                tol,
                depth + 1,
                max_d,
                mesh,
            )?;
            subdivide(
                surface,
                u_mid,
                u_max,
                v_mid,
                v_max,
                tol,
                depth + 1,
                max_d,
                mesh,
            )?;
        } else {
            let base = mesh.vertices.len();
            mesh.vertices.push(d00.point);
            mesh.vertices.push(d10.point);
            mesh.vertices.push(d11.point);
            mesh.vertices.push(d01.point);

            mesh.normals
                .push(d00.normal().unwrap_or_else(|| Vector3::new(0.0, 0.0, 1.0)));
            mesh.normals
                .push(d10.normal().unwrap_or_else(|| Vector3::new(0.0, 0.0, 1.0)));
            mesh.normals
                .push(d11.normal().unwrap_or_else(|| Vector3::new(0.0, 0.0, 1.0)));
            mesh.normals
                .push(d01.normal().unwrap_or_else(|| Vector3::new(0.0, 0.0, 1.0)));

            mesh.uvs.push([u_min, v_min]);
            mesh.uvs.push([u_max, v_min]);
            mesh.uvs.push([u_max, v_max]);
            mesh.uvs.push([u_min, v_max]);

            mesh.indices.push([base, base + 1, base + 2]);
            mesh.indices.push([base, base + 2, base + 3]);
        }
        Ok(())
    }

    subdivide(
        surface,
        u_dom.start,
        u_dom.end,
        v_dom.start,
        v_dom.end,
        chordal_tolerance,
        0,
        max_depth,
        &mut mesh,
    )?;
    Ok(mesh)
}

fn validate_points_2d(points: &[Point2]) -> Result<(), GeometryError> {
    if points.is_empty() {
        Err(GeometryError::EmptyControlNet)
    } else if points.iter().any(|point| !point.is_finite()) {
        Err(GeometryError::NonFinite)
    } else {
        Ok(())
    }
}

fn validate_spline_2d(
    degree: usize,
    points: &[Point2],
    knots: &[f64],
) -> Result<(), GeometryError> {
    validate_points_2d(points)?;
    if degree == 0 || degree >= points.len() {
        return Err(GeometryError::InvalidDegree);
    }
    if knots.len() != points.len() + degree + 1
        || knots.iter().any(|knot| !knot.is_finite())
        || knots.windows(2).any(|pair| pair[0] > pair[1])
        || knots[degree] >= knots[points.len()]
    {
        return Err(GeometryError::InvalidKnotVector);
    }
    Ok(())
}

fn lerp_2d(a: Point2, b: Point2, t: f64) -> Point2 {
    Point2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

fn de_boor_2d(degree: usize, points: &[Point2], knots: &[f64], parameter: f64) -> Point2 {
    let span = find_span(degree, points.len(), knots, parameter);
    let mut work = (0..=degree)
        .map(|index| points[span - degree + index])
        .collect::<Vec<_>>();
    for level in 1..=degree {
        for index in (level..=degree).rev() {
            let source = span - degree + index;
            let denominator = knots[source + degree - level + 1] - knots[source];
            let alpha = if denominator == 0.0 {
                0.0
            } else {
                (parameter - knots[source]) / denominator
            };
            work[index] = lerp_2d(work[index - 1], work[index], alpha);
        }
    }
    work[degree]
}

fn project_curve_2d<C: ParametricCurve2 + ?Sized>(
    curve: &C,
    point: Point2,
) -> Result<(f64, f64), GeometryError> {
    if !point.is_finite() {
        return Err(GeometryError::NonFinite);
    }
    let domain = curve.domain();
    let mut best = (domain.start, f64::INFINITY);
    for index in 0..=128 {
        let parameter = domain.start + (domain.end - domain.start) * index as f64 / 128.0;
        let delta = curve.evaluate(parameter)? - point;
        let distance = delta.length_squared();
        if distance < best.1 {
            best = (parameter, distance);
        }
    }
    let mut parameter = best.0;
    for _ in 0..12 {
        let curve_point = curve.evaluate(parameter)?;
        let tangent = curve.derivative(parameter)?;
        let denominator = tangent.length_squared();
        if denominator <= f64::EPSILON {
            break;
        }
        let delta = curve_point - point;
        let next = (parameter - delta.dot(tangent) / denominator).clamp(domain.start, domain.end);
        if (next - parameter).abs() <= 1.0e-14 {
            break;
        }
        parameter = next;
    }
    let delta = curve.evaluate(parameter)? - point;
    Ok((parameter, delta.length_squared()))
}

fn basis_derivatives(
    degree: usize,
    knots: &[f64],
    count: usize,
    parameter: f64,
) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let span = find_span(degree, count, knots, parameter);
    let mut n_table = vec![vec![0.0; count]; degree + 1];
    n_table[0][span] = 1.0;

    for level in 1..=degree {
        for index in span.saturating_sub(level)..=span {
            let left_den = knots[index + level] - knots[index];
            let right_den = knots[index + level + 1] - knots[index + 1];
            let left = if left_den == 0.0 {
                0.0
            } else {
                (parameter - knots[index]) / left_den * n_table[level - 1][index]
            };
            let right = if index + 1 >= count || right_den == 0.0 {
                0.0
            } else {
                (knots[index + level + 1] - parameter) / right_den * n_table[level - 1][index + 1]
            };
            n_table[level][index] = left + right;
        }
    }
    let basis = n_table[degree].clone();

    let mut d_table = vec![vec![0.0; count]; degree + 1];
    for level in 1..=degree {
        let p = level as f64;
        for index in span.saturating_sub(level)..=span {
            let left_den = knots[index + level] - knots[index];
            let right_den = knots[index + level + 1] - knots[index + 1];
            let left = if left_den == 0.0 {
                0.0
            } else {
                p / left_den * n_table[level - 1][index]
            };
            let right = if index + 1 >= count || right_den == 0.0 {
                0.0
            } else {
                p / right_den * n_table[level - 1][index + 1]
            };
            d_table[level][index] = left - right;
        }
    }
    let d_basis = d_table[degree].clone();

    let mut dd_basis = vec![0.0; count];
    if degree >= 2 {
        let p = degree as f64;
        for index in span.saturating_sub(degree)..=span {
            let left_den = knots[index + degree] - knots[index];
            let right_den = knots[index + degree + 1] - knots[index + 1];
            let left = if left_den == 0.0 {
                0.0
            } else {
                p / left_den * d_table[degree - 1][index]
            };
            let right = if index + 1 >= count || right_den == 0.0 {
                0.0
            } else {
                p / right_den * d_table[degree - 1][index + 1]
            };
            dd_basis[index] = left - right;
        }
    }

    (basis, d_basis, dd_basis)
}

fn point_vector(point: Point3) -> Vector3 {
    Vector3::new(point.x, point.y, point.z)
}
fn vector_point(vector: Vector3) -> Point3 {
    Point3::new(vector.x, vector.y, vector.z)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn point(x: f64, y: f64, z: f64) -> Point3 {
        Point3::new(x, y, z)
    }
    #[test]
    fn bezier_split_and_degree_elevation_preserve_geometry() {
        let curve = BezierCurve3::new(vec![
            point(0., 0., 0.),
            point(1., 2., 0.),
            point(3., 0., 0.),
        ])
        .unwrap();
        let elevated = curve.elevate_degree().unwrap();
        for i in 0..=100 {
            let t = i as f64 / 100.;
            let a = curve.evaluate(t).unwrap();
            let b = elevated.evaluate(t).unwrap();
            assert!((a - b).length() < 1e-12);
        }
        let (left, right) = curve.split(0.4).unwrap();
        assert!((left.evaluate(1.).unwrap() - right.evaluate(0.).unwrap()).length() < 1e-14);
    }
    #[test]
    fn knot_insertion_preserves_bspline() {
        let curve = BSplineCurve3::new(
            2,
            vec![
                point(0., 0., 0.),
                point(1., 2., 0.),
                point(3., 2., 0.),
                point(4., 0., 0.),
            ],
            vec![0., 0., 0., 1., 2., 2., 2.],
            false,
        )
        .unwrap();
        let inserted = curve.insert_knot(0.75).unwrap();
        for i in 0..=100 {
            let t = 2. * i as f64 / 100.;
            assert!((curve.evaluate(t).unwrap() - inserted.evaluate(t).unwrap()).length() < 1e-11);
        }
    }
    #[test]
    fn rational_quarter_circle_and_analytic_surfaces_evaluate() {
        let weight = 2f64.sqrt() / 2.;
        let curve = NurbsCurve3::new(
            2,
            vec![point(1., 0., 0.), point(1., 1., 0.), point(0., 1., 0.)],
            vec![1., weight, 1.],
            vec![0., 0., 0., 1., 1., 1.],
            false,
        )
        .unwrap();
        let middle = curve.evaluate(0.5).unwrap();
        assert!((middle.x - weight).abs() < 1e-12 && (middle.y - weight).abs() < 1e-12);
        let sphere = AnalyticSurface::Sphere {
            center: point(0., 0., 0.),
            radius: 2.,
        };
        assert!((sphere.evaluate(0., 0.).unwrap() - point(2., 0., 0.)).length() < 1e-12);
    }
    #[test]
    fn malformed_parameterizations_fail_closed() {
        assert_eq!(
            BezierCurve3::new(vec![]),
            Err(GeometryError::EmptyControlNet)
        );
        assert_eq!(
            BSplineCurve3::new(3, vec![point(0., 0., 0.)], vec![0., 1.], false),
            Err(GeometryError::InvalidDegree)
        );
    }

    #[test]
    fn polynomial_and_rational_tensor_surfaces_agree_for_unit_weights() {
        let controls = vec![
            point(0., 0., 0.),
            point(1., 0., 0.),
            point(0., 1., 0.),
            point(1., 1., 1.),
        ];
        let knots = vec![0., 0., 1., 1.];
        let polynomial =
            BSplineSurface3::new(1, 1, 2, 2, controls.clone(), knots.clone(), knots.clone())
                .unwrap();
        let rational =
            NurbsSurface3::new(1, 1, 2, 2, controls, vec![1.; 4], knots.clone(), knots).unwrap();
        for u in [0., 0.25, 0.75, 1.] {
            for v in [0., 0.4, 1.] {
                assert!(
                    (polynomial.evaluate(u, v).unwrap() - rational.evaluate(u, v).unwrap())
                        .length()
                        < 1e-12
                );
            }
        }
    }

    #[test]
    fn common_projection_extrema_and_surface_split_contracts_hold() {
        let curve = BezierCurve3::new(vec![
            point(0., 0., 0.),
            point(1., 2., 0.),
            point(2., 0., 0.),
        ])
        .unwrap();
        let (parameter, distance) = ParametricCurve3::project(&curve, point(1., 1., 0.)).unwrap();
        assert!((parameter - 0.5).abs() < 1e-10 && distance < 1e-20);
        assert!(
            curve
                .extrema_parameters(1)
                .unwrap()
                .iter()
                .any(|value| (*value - 0.5).abs() < 1e-10)
        );
        let surface = BezierSurface3::new(
            2,
            2,
            vec![
                point(0., 0., 0.),
                point(1., 0., 0.),
                point(0., 1., 0.),
                point(1., 1., 0.),
            ],
        )
        .unwrap();
        let (left, right) = surface.split_u(0.4).unwrap();
        assert!(
            (left.evaluate(1., 0.3).unwrap() - right.evaluate(0., 0.3).unwrap()).length() < 1e-14
        );
        let (uv, distance) = ParametricSurface3::project(&surface, point(0.25, 0.75, 2.)).unwrap();
        assert!(
            (uv[0] - 0.25).abs() < 1e-8
                && (uv[1] - 0.75).abs() < 1e-8
                && (distance - 4.).abs() < 1e-8
        );
    }

    #[test]
    fn bspline_2d_and_nurbs_2d_evaluate_derivatives_and_curvatures() {
        let bspline = BSplineCurve2::new(
            2,
            vec![
                Point2::new(0.0, 0.0),
                Point2::new(1.0, 2.0),
                Point2::new(2.0, 0.0),
            ],
            vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            false,
        )
        .unwrap();

        let mid = bspline.evaluate(0.5).unwrap();
        assert!((mid.x - 1.0).abs() < 1e-12);
        assert!((mid.y - 1.0).abs() < 1e-12);

        let d_mid = bspline.derivative(0.5).unwrap();
        assert!((d_mid.x - 2.0).abs() < 1e-12);
        assert!(d_mid.y.abs() < 1e-12);

        let dd_mid = bspline.second_derivative(0.5).unwrap();
        assert!(dd_mid.x.abs() < 1e-12);
        assert!((dd_mid.y - (-8.0)).abs() < 1e-12);

        let kappa = bspline.curvature(0.5).unwrap();
        assert!((kappa - (-2.0)).abs() < 1e-12);

        let inserted = bspline.insert_knot(0.5).unwrap();
        for i in 0..=20 {
            let t = i as f64 / 20.0;
            let p1 = bspline.evaluate(t).unwrap();
            let p2 = inserted.evaluate(t).unwrap();
            assert!((p1.x - p2.x).hypot(p1.y - p2.y) < 1e-12);
        }

        // NURBS 2D quarter circle of radius 1
        let weight = 2.0_f64.sqrt() / 2.0;
        let nurbs2d = NurbsCurve2::new(
            2,
            vec![
                Point2::new(1.0, 0.0),
                Point2::new(1.0, 1.0),
                Point2::new(0.0, 1.0),
            ],
            vec![1.0, weight, 1.0],
            vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            false,
        )
        .unwrap();

        for i in 0..=20 {
            let t = i as f64 / 20.0;
            let pt = nurbs2d.evaluate(t).unwrap();
            let r = (pt.x * pt.x + pt.y * pt.y).sqrt();
            assert!(
                (r - 1.0).abs() < 1e-12,
                "points on NURBS quarter circle must have radius 1"
            );
            let k = nurbs2d.curvature(t).unwrap();
            assert!(
                (k - 1.0).abs() < 1e-10,
                "unit circle curvature must be exactly 1.0"
            );
        }
    }

    #[test]
    fn bspline_and_nurbs_surface_knot_insertion_and_derivatives() {
        let controls = vec![
            point(0.0, 0.0, 0.0),
            point(1.0, 0.0, 0.5),
            point(2.0, 0.0, 0.0),
            point(0.0, 1.0, 0.5),
            point(1.0, 1.0, 1.5),
            point(2.0, 1.0, 0.5),
            point(0.0, 2.0, 0.0),
            point(1.0, 2.0, 0.5),
            point(2.0, 2.0, 0.0),
        ];
        let knots = vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let bspline_surf =
            BSplineSurface3::new(2, 2, 3, 3, controls.clone(), knots.clone(), knots.clone())
                .unwrap();

        let deriv = bspline_surf.derivatives(0.5, 0.5).unwrap();
        assert!(deriv.normal().is_some());
        let curv = deriv.curvature().unwrap();
        assert!(curv.gaussian.is_finite());
        assert!(curv.mean.is_finite());

        // Test knot insertion in U
        let ins_u = bspline_surf.insert_knot_u(0.5).unwrap();
        for u_step in 0..=10 {
            for v_step in 0..=10 {
                let u = u_step as f64 / 10.0;
                let v = v_step as f64 / 10.0;
                let p1 = bspline_surf.evaluate(u, v).unwrap();
                let p2 = ins_u.evaluate(u, v).unwrap();
                assert!((p1 - p2).length() < 1e-12);
            }
        }

        // Test knot insertion in V
        let ins_v = bspline_surf.insert_knot_v(0.5).unwrap();
        for u_step in 0..=10 {
            for v_step in 0..=10 {
                let u = u_step as f64 / 10.0;
                let v = v_step as f64 / 10.0;
                let p1 = bspline_surf.evaluate(u, v).unwrap();
                let p2 = ins_v.evaluate(u, v).unwrap();
                assert!((p1 - p2).length() < 1e-12);
            }
        }

        // NURBS surface knot insertion
        let weights = vec![1.0; 9];
        let nurbs_surf =
            NurbsSurface3::new(2, 2, 3, 3, controls, weights, knots.clone(), knots).unwrap();
        let ins_nurbs_u = nurbs_surf.insert_knot_u(0.5).unwrap();
        for u_step in 0..=10 {
            for v_step in 0..=10 {
                let u = u_step as f64 / 10.0;
                let v = v_step as f64 / 10.0;
                let p1 = nurbs_surf.evaluate(u, v).unwrap();
                let p2 = ins_nurbs_u.evaluate(u, v).unwrap();
                assert!((p1 - p2).length() < 1e-12);
            }
        }
    }

    #[test]
    fn surface_tessellation_produces_valid_meshes() {
        let controls = vec![
            point(0.0, 0.0, 0.0),
            point(1.0, 0.0, 0.0),
            point(0.0, 1.0, 0.0),
            point(1.0, 1.0, 0.0),
        ];
        let knots = vec![0.0, 0.0, 1.0, 1.0];
        let surf = BSplineSurface3::new(1, 1, 2, 2, controls, knots.clone(), knots).unwrap();

        let mesh = tessellate_surface(&surf, 4, 4).unwrap();
        assert_eq!(mesh.vertices.len(), 25);
        assert_eq!(mesh.indices.len(), 32);

        let adaptive_mesh = adaptive_tessellate_surface(&surf, 0.01, 3).unwrap();
        assert!(!adaptive_mesh.vertices.is_empty());
        assert!(!adaptive_mesh.indices.is_empty());
    }
}
