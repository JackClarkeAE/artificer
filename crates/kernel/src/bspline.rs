//! B-spline curves and surfaces (ADR 0050; stage K-B of ADR 0049).
//!
//! A B-spline here is non-rational and clamped: a degree from one to
//! [`MAX_DEGREE`], a knot vector whose two ends each repeat `degree + 1`
//! times, and a control polygon or net. Evaluation and its derivatives are
//! the Cox–de Boor recurrence, exact to the arithmetic, and a point at either
//! end of a clamped domain is its end control point to the bit. What has no
//! closed form is kept here and bounded, as it is for the ruled surface:
//! inversion is Newton's method with a fixed iteration limit that refuses
//! rather than guesses, and every face and contour integral is ten-point
//! Gauss–Legendre on each knot span. On one span every integrand a volume or
//! a centroid needs is a polynomial of degree at most `4p − 1` in each
//! parameter, which the rule integrates exactly up to degree five; an area is
//! the integral of a square root, analytic on each span, where the rule
//! converges exponentially — the standing ADR 0026 gave an ellipse's arc
//! length.
//!
//! # Where a spline lives
//!
//! A knot vector and a control net have no fixed size, and `Curve3`,
//! `Curve2` and `Surface` are `Copy`. So a spline is interned: its content is
//! validated, made canonical (a negative zero becomes zero), and stored once
//! for the life of the process, and the carriers hold a `&'static` reference
//! to it. Two carriers with the same content hold the same reference, so two
//! handles are equal exactly when their contents are, whichever was made
//! first. What a handle must never be is ordered or hashed by its address:
//! the address depends on the order splines were first made in, and a digest
//! or a sort that read it would depend on that order too. The digest hashes
//! the content, and nothing sorts handles.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Mutex, OnceLock, PoisonError};

use crate::ruled::GAUSS_NODES;
use crate::topology::{Point2, Point3, Vector2, Vector3};

/// The highest degree the kernel carries. Ten-point Gauss–Legendre is exact
/// for polynomials up to degree nineteen, and the highest-order integrand a
/// measure needs on one knot span — the first moment of a surface, or the
/// polar moment of a contour, `4p − 1` in each parameter — stays inside that
/// up to degree five.
pub(crate) const MAX_DEGREE: usize = 5;
const ORDER: usize = MAX_DEGREE + 1;

/// Why spline data was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SplineError {
    /// A degree of zero, or above [`MAX_DEGREE`].
    Degree,
    /// Weights that are not all equal: a rational spline.
    Rational,
    /// A knot vector whose ends do not each repeat `degree + 1` times.
    Unclamped,
    /// A knot vector of the wrong length, falling anywhere, with an interior
    /// knot outside the domain or repeated more than `degree` times, or an
    /// empty domain; or too few control points for the degree.
    Knots,
    /// A coordinate or a knot that is not finite.
    NonFinite,
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// Content that is stored once and shared by reference.
pub(crate) trait Interned: PartialEq + Sized + Send + Sync + 'static {
    /// A digest of the content, used only to find the bucket to search: two
    /// values in one bucket are still compared in full.
    fn fingerprint(&self) -> u64;
    fn store() -> &'static Mutex<HashMap<u64, Vec<&'static Self>>>;
}

/// The one stored copy of `value`, stored now if it is new.
fn intern<T: Interned>(value: T) -> &'static T {
    let fingerprint = value.fingerprint();
    // A panic elsewhere while the lock was held cannot leave the map
    // half-written — insertion is one push — so a poisoned lock is still a
    // sound one.
    let mut store = T::store().lock().unwrap_or_else(PoisonError::into_inner);
    let bucket = store.entry(fingerprint).or_default();
    if let Some(existing) = bucket.iter().find(|existing| ***existing == value) {
        return existing;
    }
    let stored: &'static T = Box::leak(Box::new(value));
    bucket.push(stored);
    stored
}

macro_rules! interned {
    ($data:ty) => {
        impl Interned for $data {
            fn fingerprint(&self) -> u64 {
                let mut hasher = DefaultHasher::new();
                self.hash_content(&mut hasher);
                hasher.finish()
            }

            fn store() -> &'static Mutex<HashMap<u64, Vec<&'static Self>>> {
                static STORE: OnceLock<Mutex<HashMap<u64, Vec<&'static $data>>>> = OnceLock::new();
                STORE.get_or_init(Default::default)
            }
        }
    };
}

/// A negative zero is a zero: two splines that differ only there are one
/// spline, as they are to the digest.
fn canonical(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

/// The checks every knot vector passes before it is stored.
fn validate_knots(degree: usize, knots: &[f64], count: usize) -> Result<(), SplineError> {
    if degree == 0 || degree > MAX_DEGREE {
        return Err(SplineError::Degree);
    }
    if knots.iter().any(|knot| !knot.is_finite()) {
        return Err(SplineError::NonFinite);
    }
    if count < degree + 1 || knots.len() != count + degree + 1 {
        return Err(SplineError::Knots);
    }
    if knots.windows(2).any(|pair| pair[1] < pair[0]) {
        return Err(SplineError::Knots);
    }
    let (start, end) = (knots[degree], knots[count]);
    if knots[..=degree].iter().any(|knot| *knot != start)
        || knots[count..].iter().any(|knot| *knot != end)
    {
        return Err(SplineError::Unclamped);
    }
    if start >= end {
        return Err(SplineError::Knots);
    }
    // Interior knots lie strictly inside the domain, so the first and last
    // spans are never empty, and none repeats more than the degree, so the
    // curve never comes apart at one.
    let interior = &knots[degree + 1..count];
    if interior.iter().any(|knot| *knot <= start || *knot >= end) {
        return Err(SplineError::Knots);
    }
    let mut run = 1;
    for pair in interior.windows(2) {
        if pair[1] == pair[0] {
            run += 1;
            if run > degree {
                return Err(SplineError::Knots);
            }
        } else {
            run = 1;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Basis functions
// ---------------------------------------------------------------------------

/// The index `i` of the nonempty span `[knots[i], knots[i + 1])` holding `t`,
/// with the domain's end in the last span. `t` is clamped into the domain.
fn find_span(degree: usize, knots: &[f64], count: usize, t: f64) -> usize {
    if t >= knots[count] {
        // Interior knots lie strictly inside the domain, so the last span,
        // `count − 1`, is never empty.
        return count - 1;
    }
    if t.is_nan() || t <= knots[degree] {
        return degree;
    }
    let (mut low, mut high) = (degree, count);
    while high - low > 1 {
        let middle = (low + high) / 2;
        if t < knots[middle] {
            high = middle;
        } else {
            low = middle;
        }
    }
    low
}

/// The nonzero basis functions on `span` at `t` and their derivatives up to
/// `order` (at most two): `ders[k][r]` is the `k`-th derivative of
/// `N_{span − degree + r}`. The recurrence is the one in Piegl and Tiller,
/// *The NURBS Book*, algorithm A2.3, over fixed-size arrays.
fn basis(degree: usize, knots: &[f64], span: usize, t: f64, order: usize) -> [[f64; ORDER]; 3] {
    let p = degree;
    let mut ndu = [[0.0; ORDER]; ORDER];
    let mut left = [0.0; ORDER];
    let mut right = [0.0; ORDER];
    ndu[0][0] = 1.0;
    for j in 1..=p {
        left[j] = t - knots[span + 1 - j];
        right[j] = knots[span + j] - t;
        let mut saved = 0.0;
        for r in 0..j {
            // Every lower-triangle entry is a knot difference that spans the
            // nonempty span, so it is never zero.
            ndu[j][r] = right[r + 1] + left[j - r];
            let temp = ndu[r][j - 1] / ndu[j][r];
            ndu[r][j] = right[r + 1].mul_add(temp, saved);
            saved = left[j - r] * temp;
        }
        ndu[j][j] = saved;
    }
    let mut ders = [[0.0; ORDER]; 3];
    for (r, value) in ders[0].iter_mut().enumerate().take(p + 1) {
        *value = ndu[r][p];
    }
    let order = order.min(p).min(2);
    let mut a = [[0.0; ORDER]; 2];
    for r in 0..=p {
        let (mut s1, mut s2) = (0, 1);
        a[0][0] = 1.0;
        for k in 1..=order {
            let mut d = 0.0;
            let rk = r as isize - k as isize;
            let pk = p - k;
            if r >= k {
                a[s2][0] = a[s1][0] / ndu[pk + 1][rk as usize];
                d = a[s2][0] * ndu[rk as usize][pk];
            }
            let j1 = if rk >= -1 { 1 } else { (-rk) as usize };
            let j2 = if r as isize - 1 <= pk as isize {
                k - 1
            } else {
                p - r
            };
            for j in j1..=j2 {
                let index = (rk + j as isize) as usize;
                a[s2][j] = (a[s1][j] - a[s1][j - 1]) / ndu[pk + 1][index];
                d = a[s2][j].mul_add(ndu[index][pk], d);
            }
            if r <= pk {
                a[s2][k] = -a[s1][k - 1] / ndu[pk + 1][r];
                d = a[s2][k].mul_add(ndu[r][pk], d);
            }
            ders[k][r] = d;
            std::mem::swap(&mut s1, &mut s2);
        }
    }
    let mut factor = p as f64;
    for (k, row) in ders.iter_mut().enumerate().take(order + 1).skip(1) {
        for value in row.iter_mut().take(p + 1) {
            *value *= factor;
        }
        factor *= (p - k) as f64;
    }
    ders
}

/// `Σ weights[r]·point(first + r)`, in one fixed order.
fn combine<const D: usize>(
    weights: &[f64; ORDER],
    degree: usize,
    first: usize,
    point: &impl Fn(usize) -> [f64; D],
) -> [f64; D] {
    let mut sum = [0.0; D];
    for (r, weight) in weights.iter().enumerate().take(degree + 1) {
        let control = point(first + r);
        for axis in 0..D {
            sum[axis] = weight.mul_add(control[axis], sum[axis]);
        }
    }
    sum
}

/// A point of the curve with control points `point(i)`.
///
/// At either end of the domain the point is the end control point itself, to
/// the bit. That is what lets an edge along a surface's boundary and the
/// surface agree on every point they share: both are evaluated here, from the
/// same numbers.
fn curve_point<const D: usize>(
    degree: usize,
    knots: &[f64],
    count: usize,
    t: f64,
    point: &impl Fn(usize) -> [f64; D],
) -> [f64; D] {
    if t.is_nan() || t <= knots[degree] {
        return point(0);
    }
    if t >= knots[count] {
        return point(count - 1);
    }
    let span = find_span(degree, knots, count, t);
    let ders = basis(degree, knots, span, t, 0);
    combine(&ders[0], degree, span - degree, point)
}

/// The point, first and second derivatives of a curve at `t`.
///
/// The derivatives are taken of the control points less the span's first
/// one. The derivatives of the basis sum to zero, so that changes nothing
/// but the rounding: the weights grow as the knots close up, and combined
/// with coordinates far from the origin they would cancel to a rate that
/// kept only the last few bits of those coordinates.
fn curve_derivatives<const D: usize>(
    degree: usize,
    knots: &[f64],
    count: usize,
    t: f64,
    point: &impl Fn(usize) -> [f64; D],
) -> [[f64; D]; 3] {
    let clamped = t.clamp(knots[degree], knots[count]);
    let span = find_span(degree, knots, count, clamped);
    let ders = basis(degree, knots, span, clamped, 2);
    let first = span - degree;
    let anchor = point(first);
    let relative = |index: usize| sub(point(index), anchor);
    [
        curve_point(degree, knots, count, t, point),
        combine(&ders[1], degree, first, &relative),
        combine(&ders[2], degree, first, &relative),
    ]
}

/// The nonempty spans of a clamped knot vector that meet `[from, to]`,
/// clipped to it, in rising order.
fn spans_within(degree: usize, knots: &[f64], count: usize, from: f64, to: f64) -> Vec<(f64, f64)> {
    let (low, high) = (from.min(to), from.max(to));
    (degree..count)
        .filter(|index| knots[*index] < knots[index + 1])
        .filter_map(|index| {
            let start = knots[index].max(low);
            let end = knots[index + 1].min(high);
            (start < end).then_some((start, end))
        })
        .collect()
}

/// The ten-point rule's nodes and weights on `[from, to]`, the weights
/// carrying the half-width Jacobian.
fn nodes(from: f64, to: f64) -> impl Iterator<Item = (f64, f64)> {
    let half = 0.5 * (to - from);
    let middle = 0.5 * (from + to);
    GAUSS_NODES
        .iter()
        .map(move |(node, weight)| (half.mul_add(*node, middle), weight * half))
}

fn add<const D: usize>(first: [f64; D], second: [f64; D]) -> [f64; D] {
    std::array::from_fn(|axis| first[axis] + second[axis])
}

fn sub<const D: usize>(first: [f64; D], second: [f64; D]) -> [f64; D] {
    std::array::from_fn(|axis| first[axis] - second[axis])
}

fn scale<const D: usize>(value: [f64; D], factor: f64) -> [f64; D] {
    value.map(|component| component * factor)
}

/// `first + (second − first)·alpha`, the blend knot insertion and degree
/// elevation both use.
fn blend<const D: usize>(first: [f64; D], second: [f64; D], alpha: f64) -> [f64; D] {
    std::array::from_fn(|axis| (second[axis] - first[axis]).mul_add(alpha, first[axis]))
}

fn norm<const D: usize>(value: [f64; D]) -> f64 {
    value
        .iter()
        .map(|component| component * component)
        .sum::<f64>()
        .sqrt()
}

pub(crate) const fn point3(value: [f64; 3]) -> Point3 {
    Point3::new(value[0], value[1], value[2])
}

pub(crate) const fn vector3(value: [f64; 3]) -> Vector3 {
    Vector3::new(value[0], value[1], value[2])
}

pub(crate) const fn array3(point: Point3) -> [f64; 3] {
    [point.x, point.y, point.z]
}

pub(crate) const fn point2(value: [f64; 2]) -> Point2 {
    Point2::new(value[0], value[1])
}

pub(crate) const fn array2(point: Point2) -> [f64; 2] {
    [point.x, point.y]
}

/// The least step, in model space, a nearest-point walk near `point` can
/// still tell from rounding: a few units in the last place of the point's
/// largest coordinate, and never less than a few of a unit length's.
///
/// A point on a surface is a sum of a dozen or more products of control
/// points and basis functions, each rounded in the last place of its
/// coordinates, so where those coordinates are large the point — and the
/// distance to it, and the Newton step that distance gives — carries an
/// error of that size, and a step no longer than it is noise.
pub(crate) fn settled_length(point: Point3) -> f64 {
    let size = point.x.abs().max(point.y.abs()).max(point.z.abs());
    16.0 * f64::EPSILON * size.max(1.0)
}

// ---------------------------------------------------------------------------
// Curves
// ---------------------------------------------------------------------------

/// The stored content of a B-spline curve in `D` dimensions.
#[derive(Debug, PartialEq)]
pub(crate) struct CurveData<const D: usize> {
    degree: usize,
    knots: Vec<f64>,
    points: Vec<[f64; D]>,
}

impl<const D: usize> CurveData<D> {
    fn hash_content(&self, hasher: &mut DefaultHasher) {
        self.degree.hash(hasher);
        self.knots.len().hash(hasher);
        for knot in &self.knots {
            knot.to_bits().hash(hasher);
        }
        for point in &self.points {
            for component in point {
                component.to_bits().hash(hasher);
            }
        }
    }
}

interned!(CurveData<2>);
interned!(CurveData<3>);

/// A B-spline curve: a `Copy` handle to interned content.
#[derive(Clone, Copy)]
pub(crate) struct SplineCurve<const D: usize>(&'static CurveData<D>);

/// A space curve: the edge of a B-spline wall, or a section of a loft.
pub(crate) type SplineCurve3 = SplineCurve<3>;
/// A plane curve: a sketch spline, or a B-spline edge's curve on a plane.
pub(crate) type SplineCurve2 = SplineCurve<2>;

impl<const D: usize> PartialEq for SplineCurve<D> {
    /// Equal content is one stored value, so this is almost always the
    /// address comparison; the content comparison behind it keeps it true
    /// for any two handles, however they were made.
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0) || self.0 == other.0
    }
}

impl<const D: usize> std::fmt::Debug for SplineCurve<D> {
    /// The content's shape, never its address, which depends on the order
    /// splines were made in.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (start, end) = (
            self.0.knots[self.0.degree],
            self.0.knots[self.0.points.len()],
        );
        write!(
            formatter,
            "SplineCurve{{degree: {}, points: {}, domain: [{start}, {end}]}}",
            self.0.degree,
            self.0.points.len()
        )
    }
}

impl<const D: usize> SplineCurve<D>
where
    CurveData<D>: Interned,
{
    /// A clamped, non-rational curve, validated and interned.
    pub(crate) fn new(
        degree: usize,
        knots: Vec<f64>,
        points: Vec<[f64; D]>,
    ) -> Result<Self, SplineError> {
        if points
            .iter()
            .flatten()
            .any(|component| !component.is_finite())
        {
            return Err(SplineError::NonFinite);
        }
        validate_knots(degree, &knots, points.len())?;
        Ok(Self(intern(CurveData {
            degree,
            knots: knots.into_iter().map(canonical).collect(),
            points: points
                .into_iter()
                .map(|point| point.map(canonical))
                .collect(),
        })))
    }

    pub(crate) fn degree(self) -> usize {
        self.0.degree
    }

    pub(crate) fn knots(self) -> &'static [f64] {
        &self.0.knots
    }

    pub(crate) fn points(self) -> &'static [[f64; D]] {
        &self.0.points
    }

    pub(crate) fn count(self) -> usize {
        self.0.points.len()
    }

    /// The parameter interval the curve is defined over.
    pub(crate) fn domain(self) -> (f64, f64) {
        (
            self.0.knots[self.0.degree],
            self.0.knots[self.0.points.len()],
        )
    }

    pub(crate) fn first(self) -> [f64; D] {
        self.0.points[0]
    }

    pub(crate) fn last(self) -> [f64; D] {
        self.0.points[self.0.points.len() - 1]
    }

    pub(crate) fn evaluate(self, t: f64) -> [f64; D] {
        let points = self.points();
        curve_point(self.degree(), self.knots(), self.count(), t, &|index| {
            points[index]
        })
    }

    /// The point, first and second derivatives at `t`.
    pub(crate) fn derivatives(self, t: f64) -> [[f64; D]; 3] {
        let points = self.points();
        curve_derivatives(self.degree(), self.knots(), self.count(), t, &|index| {
            points[index]
        })
    }

    /// The nonempty knot spans meeting `[from, to]`, clipped to it.
    pub(crate) fn spans(self, from: f64, to: f64) -> Vec<(f64, f64)> {
        spans_within(self.degree(), self.knots(), self.count(), from, to)
    }

    /// The distinct interior knots with their multiplicities.
    fn interior_knots(self) -> Vec<(f64, usize)> {
        let degree = self.degree();
        let mut distinct: Vec<(f64, usize)> = Vec::new();
        for knot in &self.knots()[degree + 1..self.count()] {
            match distinct.last_mut() {
                Some((value, multiplicity)) if *value == *knot => *multiplicity += 1,
                _ => distinct.push((*knot, 1)),
            }
        }
        distinct
    }

    /// The same curve with its control points mapped, as an affine map
    /// carries a B-spline: the image of the curve is the curve of the images.
    pub(crate) fn mapped<const E: usize>(
        self,
        map: impl Fn([f64; D]) -> [f64; E],
    ) -> Option<SplineCurve<E>>
    where
        CurveData<E>: Interned,
    {
        SplineCurve::new(
            self.degree(),
            self.knots().to_vec(),
            self.points().iter().map(|point| map(*point)).collect(),
        )
        .ok()
    }

    /// The same curve with its end control points — which a clamped curve
    /// starts and ends on — set to `first` and `last`: how a curve built by
    /// arithmetic of its own is made to end on the very points its
    /// neighbours do.
    pub(crate) fn with_ends(self, first: [f64; D], last: [f64; D]) -> Option<Self> {
        if self.first() == first && self.last() == last {
            return Some(self);
        }
        let mut points = self.points().to_vec();
        let count = points.len();
        points[0] = first;
        points[count - 1] = last;
        Self::new(self.degree(), self.knots().to_vec(), points).ok()
    }

    /// The same locus walked the other way, over the negated domain: `t` on
    /// this curve is `−t` on that one. Negation is exact, so reversing twice
    /// gives back the same knots to the bit.
    pub(crate) fn reversed(self) -> Self {
        Self::new(
            self.degree(),
            self.knots().iter().rev().map(|knot| -knot).collect(),
            self.points().iter().rev().copied().collect(),
        )
        .unwrap_or(self)
    }

    /// The same curve over `[from, to]`, the knots mapped affinely and the
    /// two ends set exactly.
    pub(crate) fn reparameterized(self, from: f64, to: f64) -> Option<Self> {
        let (start, end) = self.domain();
        if start == from && end == to {
            return Some(self);
        }
        let degree = self.degree();
        let count = self.count();
        let knots = self
            .knots()
            .iter()
            .enumerate()
            .map(|(index, knot)| {
                if index <= degree {
                    from
                } else if index >= count {
                    to
                } else {
                    (to - from).mul_add((knot - start) / (end - start), from)
                }
            })
            .collect();
        Self::new(degree, knots, self.points().to_vec()).ok()
    }

    /// The same curve with `t` inserted as a knot `times` more times, or as
    /// often as it may be: Boehm's insertion, which moves no point of the
    /// curve. An end of the domain is never inserted.
    pub(crate) fn inserted(self, t: f64, times: usize) -> Self {
        let (start, end) = self.domain();
        if !(t > start && t < end) {
            return self;
        }
        let degree = self.degree();
        let mut knots = self.knots().to_vec();
        let mut points = self.points().to_vec();
        for _ in 0..times {
            let multiplicity = knots.iter().filter(|knot| **knot == t).count();
            if multiplicity >= degree {
                break;
            }
            let count = points.len();
            let span = find_span(degree, &knots, count, t);
            let mut next = Vec::with_capacity(count + 1);
            next.extend_from_slice(&points[..=span - degree]);
            for index in span - degree + 1..=span - multiplicity {
                let alpha = (t - knots[index]) / (knots[index + degree] - knots[index]);
                next.push(blend(points[index - 1], points[index], alpha));
            }
            next.extend_from_slice(&points[span - multiplicity..]);
            knots.insert(span + 1, t);
            points = next;
        }
        Self::new(degree, knots, points).unwrap_or(self)
    }

    /// The two halves either side of an interior parameter, each clamped,
    /// sharing the point at `t` to the bit.
    pub(crate) fn split(self, t: f64) -> Option<(Self, Self)> {
        let (start, end) = self.domain();
        if !(t > start && t < end) {
            return None;
        }
        let degree = self.degree();
        let full = self.inserted(t, degree);
        let knots = full.knots();
        let first = knots.iter().position(|knot| *knot == t)?;
        if knots[first..first + degree].iter().any(|knot| *knot != t) {
            return None;
        }
        let points = full.points();
        let mut left_knots = knots[..first + degree].to_vec();
        left_knots.push(t);
        let mut right_knots = vec![t];
        right_knots.extend_from_slice(&knots[first..]);
        let left = Self::new(degree, left_knots, points[..first].to_vec()).ok()?;
        let right = Self::new(degree, right_knots, points[first - 1..].to_vec()).ok()?;
        Some((left, right))
    }

    /// The curve as Bézier segments: every interior knot raised to the
    /// degree, each segment's `degree + 1` control points with the ends it
    /// shares with its neighbours.
    pub(crate) fn bezier_segments(self) -> (Vec<f64>, Vec<Vec<[f64; D]>>) {
        let degree = self.degree();
        let mut full = self;
        let interior = self.interior_knots();
        for (knot, _) in &interior {
            full = full.inserted(*knot, degree);
        }
        let (start, end) = self.domain();
        let mut breaks = vec![start];
        breaks.extend(interior.iter().map(|(knot, _)| *knot));
        breaks.push(end);
        let points = full.points();
        let segments = (0..breaks.len() - 1)
            .map(|segment| points[segment * degree..=segment * degree + degree].to_vec())
            .collect();
        (breaks, segments)
    }

    /// The same curve at a higher degree. Each Bézier segment is raised on
    /// its own — the new control points are the classical convex blends of
    /// the old — and the segments are laid end to end with every interior
    /// knot at full multiplicity. The locus and its parameterisation are
    /// unchanged; only the representation grows.
    pub(crate) fn elevated(self, degree: usize) -> Option<Self> {
        if degree == self.degree() {
            return Some(self);
        }
        if degree < self.degree() || degree > MAX_DEGREE {
            return None;
        }
        let (breaks, mut segments) = self.bezier_segments();
        let mut current = self.degree();
        while current < degree {
            segments = segments
                .iter()
                .map(|segment| {
                    let next = current + 1;
                    let mut raised = Vec::with_capacity(next + 1);
                    raised.push(segment[0]);
                    for index in 1..next {
                        let alpha = index as f64 / next as f64;
                        raised.push(blend(segment[index], segment[index - 1], alpha));
                    }
                    raised.push(segment[current]);
                    raised
                })
                .collect();
            current += 1;
        }
        let mut knots = vec![breaks[0]; degree + 1];
        let mut points = segments[0].clone();
        for (segment, knot) in segments[1..].iter().zip(&breaks[1..]) {
            knots.extend(std::iter::repeat_n(*knot, degree));
            points.extend_from_slice(&segment[1..]);
        }
        knots.extend(std::iter::repeat_n(breaks[breaks.len() - 1], degree + 1));
        Self::new(degree, knots, points).ok()
    }

    /// The same curve on the knot vector `target`, which must hold every one
    /// of its own knots at least as often, over the same domain.
    pub(crate) fn refined(self, target: &[f64]) -> Option<Self> {
        let mut refined = self;
        let degree = self.degree();
        if target.len() < degree + 2 {
            return None;
        }
        for knot in &target[degree + 1..target.len() - degree - 1] {
            let wanted = target.iter().filter(|value| *value == knot).count();
            let has = refined
                .knots()
                .iter()
                .filter(|value| *value == knot)
                .count();
            if has < wanted {
                refined = refined.inserted(*knot, wanted - has);
            }
        }
        (refined.knots() == target).then_some(refined)
    }

    /// Arc length between two parameters, by Gauss–Legendre on each span
    /// halved: the square root of a polynomial, analytic on every span.
    pub(crate) fn length(self, from: f64, to: f64) -> f64 {
        let mut total = 0.0;
        for (start, end) in self.spans(from, to) {
            let middle = 0.5 * (start + end);
            for (low, high) in [(start, middle), (middle, end)] {
                for (t, weight) in nodes(low, high) {
                    total += weight * norm(self.derivatives(t)[1]);
                }
            }
        }
        total
    }

    /// The parameter `fraction` of the way along the curve by arc length,
    /// found by bisection on the length: the correspondence a loft cuts at
    /// is by length, and a spline's parameter is not.
    pub(crate) fn parameter_at_fraction(self, fraction: f64) -> f64 {
        let (start, end) = self.domain();
        let total = self.length(start, end);
        let target = fraction.clamp(0.0, 1.0) * total;
        let (mut low, mut high) = (start, end);
        for _ in 0..60 {
            let middle = 0.5 * (low + high);
            if self.length(start, middle) < target {
                low = middle;
            } else {
                high = middle;
            }
        }
        0.5 * (low + high)
    }

    /// The smallest speed on a sampling of every span, which a curve with a
    /// cusp or a stalled end reaches zero at.
    pub(crate) fn least_speed(self) -> f64 {
        let (start, end) = self.domain();
        let mut least = f64::INFINITY;
        for (low, high) in self.spans(start, end) {
            for step in 0..=8 {
                let t = (high - low).mul_add(f64::from(step) / 8.0, low);
                least = least.min(norm(self.derivatives(t)[1]));
            }
        }
        least
    }

    /// A bound on the second derivative over the span `[low, high]`: the
    /// largest second difference of the control points it depends on, scaled
    /// as the derivative of a B-spline scales them. The second derivative is
    /// a B-spline whose control points these are, so by the convex hull it
    /// never exceeds the largest.
    pub(crate) fn curvature_bound(self, low: f64, high: f64) -> f64 {
        let degree = self.degree();
        if degree < 2 {
            return 0.0;
        }
        let knots = self.knots();
        let points = self.points();
        let span = find_span(degree, knots, self.count(), 0.5 * (low + high));
        let first = span - degree;
        let p = degree as f64;
        let rate = |index: usize| {
            let width = knots[index + degree + 1] - knots[index + 1];
            if width > 0.0 {
                scale(sub(points[index + 1], points[index]), p / width)
            } else {
                [0.0; D]
            }
        };
        let mut bound = 0.0_f64;
        for index in first..first + degree - 1 {
            let width = knots[index + degree + 1] - knots[index + 2];
            if width > 0.0 {
                let second = scale(sub(rate(index + 1), rate(index)), (p - 1.0) / width);
                bound = bound.max(norm(second));
            }
        }
        bound
    }

    /// The parameters to sample `[from, to]` at so that no chord between two
    /// neighbours strays more than `tolerance` from the curve: every knot in
    /// the interval, and between them a power of two of equal steps sized by
    /// the span's second-derivative bound, `h²·max|C''|/8 ≤ tolerance`. A
    /// power of two keeps two samplings of one span nested, whatever each
    /// asked for.
    pub(crate) fn samples(self, from: f64, to: f64, tolerance: f64, most: usize) -> Vec<f64> {
        let mut samples = vec![from.min(to)];
        for (low, high) in self.spans(from, to) {
            let bound = self.curvature_bound(low, high);
            let steps = span_steps(bound, high - low, tolerance, most);
            for step in 1..=steps {
                samples.push(if step == steps {
                    high
                } else {
                    (high - low).mul_add(step as f64 / steps as f64, low)
                });
            }
        }
        if from > to {
            samples.reverse();
        }
        samples
    }
}

/// How many equal steps a span of `width` needs for its chords to stay within
/// `tolerance` of a curve whose second derivative is at most `bound`: the
/// smallest power of two that does it, and at most `most`.
pub(crate) fn span_steps(bound: f64, width: f64, tolerance: f64, most: usize) -> usize {
    let most = most.max(1);
    if bound.is_nan()
        || bound <= 0.0
        || tolerance.is_nan()
        || tolerance <= 0.0
        || !width.is_finite()
    {
        return 1;
    }
    let mut steps = 1_usize;
    while steps < most {
        let step = width / steps as f64;
        if bound * step * step / 8.0 <= tolerance {
            break;
        }
        steps *= 2;
    }
    steps.min(most)
}

impl SplineCurve2 {
    /// The contour integrals Green's theorem turns a planar face's measures
    /// into, over `[from, to]` (walked backwards when `to < from`), with the
    /// coordinates measured from `anchor`: `½∫(x dy − y dx)`,
    /// `⅓∫x(x dy − y dx)`, `⅓∫y(x dy − y dx)` and `¼∫(x² + y²)(x dy − y dx)`.
    ///
    /// On one span each integrand is a polynomial of degree at most `4p − 1`,
    /// so the ten-point rule is exact for every one of them.
    pub(crate) fn contour(self, from: f64, to: f64, anchor: Point2) -> [f64; 4] {
        let mut totals = [0.0; 4];
        for (low, high) in self.spans(from, to) {
            for (t, weight) in nodes(low, high) {
                let [point, rate, _] = self.derivatives(t);
                let (x, y) = (point[0] - anchor.x, point[1] - anchor.y);
                let turn = x.mul_add(rate[1], -(y * rate[0]));
                totals[0] += weight * 0.5 * turn;
                totals[1] += weight * x * turn / 3.0;
                totals[2] += weight * y * turn / 3.0;
                totals[3] += weight * 0.25 * x.mul_add(x, y * y) * turn;
            }
        }
        if to < from {
            totals.map(|total| -total)
        } else {
            totals
        }
    }

    pub(crate) fn point(self, t: f64) -> Point2 {
        point2(self.evaluate(t))
    }

    pub(crate) fn tangent(self, t: f64) -> Vector2 {
        let rate = self.derivatives(t)[1];
        Vector2::new(rate[0], rate[1])
    }
}

impl SplineCurve3 {
    pub(crate) fn point(self, t: f64) -> Point3 {
        point3(self.evaluate(t))
    }

    pub(crate) fn tangent(self, t: f64) -> Vector3 {
        vector3(self.derivatives(t)[1])
    }

    /// A length the curve is the size of: the diagonal of its control
    /// polygon's box, which the curve lies inside.
    pub(crate) fn size(self) -> f64 {
        let mut low = [f64::INFINITY; 3];
        let mut high = [f64::NEG_INFINITY; 3];
        for point in self.points() {
            for axis in 0..3 {
                low[axis] = low[axis].min(point[axis]);
                high[axis] = high[axis].max(point[axis]);
            }
        }
        norm(sub(high, low))
    }
}

/// A space curve lying in `plane`, as the same B-spline in the plane's own
/// coordinates: its control points projected, which for a curve in the plane
/// is the curve itself.
pub(crate) fn plane_pcurve(
    curve: SplineCurve3,
    plane: crate::topology::Plane,
) -> Option<SplineCurve2> {
    curve.mapped(|point| array2(plane.project(point3(point))))
}

/// The Greville abscissae of a knot vector: where a B-spline whose control
/// points are the values of a linear function reproduces that function.
pub(crate) fn greville(degree: usize, knots: &[f64], count: usize) -> Vec<f64> {
    (0..count)
        .map(|index| knots[index + 1..=index + degree].iter().sum::<f64>() / degree as f64)
        .collect()
}

/// A straight segment as a B-spline of degree one over `[0, 1]`.
pub(crate) fn line_curve(start: Point3, end: Point3) -> Option<SplineCurve3> {
    SplineCurve3::new(
        1,
        vec![0.0, 0.0, 1.0, 1.0],
        vec![array3(start), array3(end)],
    )
    .ok()
}

/// A circular arc `center + radius·(cos t·u + sin t·v)` from `t = from` to
/// `t = to` as a cubic B-spline over `[0, 1]`, within `tolerance` of the arc.
///
/// The arc is cut into equal angles and each piece is the Bézier cubic with
/// handles `(4/3)·tan(θ/4)·r` along the tangents, whose radial error falls as
/// the sixth power of the angle. The ends of every piece lie on the arc, the
/// tangents there are the arc's own, and equal pieces with equal handles meet
/// with one derivative, so the fit is C¹. The count doubles until the error,
/// sampled finely on every piece, is within half the tolerance. The pieces
/// are laid end to end with every joint a knot of full multiplicity, so the
/// joints are points of the arc exactly.
///
/// The fit is made and measured about the centre, and only its control
/// points are carried out to it: measured where it lies, a fit a hundred
/// kilometres out would be judged against the rounding of its coordinates
/// rather than its own error. Once carried there, no curve is nearer the arc
/// than those coordinates can hold, so the tolerance is never taken below a
/// few units in their last place.
pub(crate) fn arc_curve(
    center: Point3,
    u: Vector3,
    v: Vector3,
    radius: f64,
    from: f64,
    to: f64,
    tolerance: f64,
) -> Option<SplineCurve3> {
    let sweep = to - from;
    if radius.is_nan()
        || radius <= 0.0
        || !sweep.is_finite()
        || sweep == 0.0
        || tolerance.is_nan()
        || tolerance <= 0.0
    {
        return None;
    }
    let tolerance = tolerance.max(settled_length(center));
    // Points about the centre, as vectors from it.
    let at = |angle: f64| {
        let (sin, cos) = angle.sin_cos();
        u * (radius * cos) + v * (radius * sin)
    };
    let rate = |angle: f64| {
        let (sin, cos) = angle.sin_cos();
        u * (-radius * sin) + v * (radius * cos)
    };
    // The error of one piece of a quarter turn is 2.7e-4 of the radius and
    // falls as the sixth power of the angle; start from that estimate.
    let quarter = std::f64::consts::FRAC_PI_2;
    let estimate = (sweep.abs() / quarter) * (2.8e-4 * radius / (0.5 * tolerance)).powf(1.0 / 6.0);
    let mut pieces = (estimate.ceil() as usize).clamp(1, 4096);
    loop {
        let step = sweep / pieces as f64;
        let handle = (4.0 / 3.0) * (0.25 * step).tan();
        let mut points = Vec::with_capacity(3 * pieces + 1);
        let mut worst = 0.0_f64;
        for piece in 0..pieces {
            let start_angle = step.mul_add(piece as f64, from);
            let end_angle = if piece + 1 == pieces {
                to
            } else {
                step.mul_add((piece + 1) as f64, from)
            };
            let start = at(start_angle);
            let end = at(end_angle);
            let control = [
                start,
                start + rate(start_angle) * handle,
                end + rate(end_angle) * (-handle),
                end,
            ];
            if piece == 0 {
                points.push(array3(center + control[0]));
            }
            points.extend(control[1..].iter().map(|point| array3(center + *point)));
            for sample in 1..16 {
                let t = f64::from(sample) / 16.0;
                let s = 1.0 - t;
                let radial = control[0] * (s * s * s)
                    + control[1] * (3.0 * s * s * t)
                    + control[2] * (3.0 * s * t * t)
                    + control[3] * (t * t * t);
                let in_plane = u * radial.dot(u) + v * radial.dot(v);
                worst = worst
                    .max((in_plane.length() - radius).abs())
                    .max((radial - in_plane).length());
            }
        }
        if worst <= 0.5 * tolerance {
            let mut knots = vec![0.0; 4];
            for piece in 1..pieces {
                knots.extend(std::iter::repeat_n(piece as f64 / pieces as f64, 3));
            }
            knots.extend([1.0; 4]);
            return SplineCurve3::new(3, knots, points).ok();
        }
        if pieces >= 4096 {
            return None;
        }
        pieces *= 2;
    }
}

/// The given curves, each already over `[0, 1]`, raised to one degree and
/// refined to one knot vector, so that their control points pair one for one:
/// what a surface through them needs. The knot vector is the union of
/// theirs, each knot as often as the curve that repeats it most.
pub(crate) fn common_basis(curves: &[SplineCurve3]) -> Option<Vec<SplineCurve3>> {
    let degree = curves.iter().map(|curve| curve.degree()).max()?;
    let raised = curves
        .iter()
        .map(|curve| curve.elevated(degree))
        .collect::<Option<Vec<_>>>()?;
    let mut union: Vec<(f64, usize)> = Vec::new();
    for curve in &raised {
        for (knot, multiplicity) in curve.interior_knots() {
            match union.iter_mut().find(|(value, _)| *value == knot) {
                Some((_, most)) => *most = (*most).max(multiplicity),
                None => union.push((knot, multiplicity)),
            }
        }
    }
    union.sort_by(|left, right| left.0.total_cmp(&right.0));
    let mut target = vec![0.0; degree + 1];
    for (knot, multiplicity) in &union {
        target.extend(std::iter::repeat_n(*knot, *multiplicity));
    }
    target.extend(std::iter::repeat_n(1.0, degree + 1));
    raised.iter().map(|curve| curve.refined(&target)).collect()
}

/// The clamped knot vector of degree `degree` that interpolation at the
/// rising `parameters` uses: each interior knot the mean of `degree`
/// consecutive parameters (Piegl and Tiller, eq. 9.8), which keeps the
/// system below well conditioned.
pub(crate) fn interpolation_knots(parameters: &[f64], degree: usize) -> Vec<f64> {
    let count = parameters.len();
    let mut knots = vec![parameters[0]; degree + 1];
    for index in 1..count.saturating_sub(degree) {
        let sum: f64 = parameters[index..index + degree].iter().sum();
        knots.push(sum / degree as f64);
    }
    knots.extend(std::iter::repeat_n(parameters[count - 1], degree + 1));
    knots
}

/// The control points of the curve of `degree` on `knots` through `data` at
/// `parameters`. The ends are the ends of the data exactly — a clamped curve
/// starts on its first control point — and the interior is one linear solve,
/// by Gaussian elimination with partial pivoting in a fixed order.
fn interpolate<const D: usize>(
    degree: usize,
    knots: &[f64],
    parameters: &[f64],
    data: &[[f64; D]],
) -> Option<Vec<[f64; D]>> {
    let count = data.len();
    let mut points = vec![[0.0; D]; count];
    points[0] = data[0];
    points[count - 1] = data[count - 1];
    let unknowns = count.saturating_sub(2);
    if unknowns == 0 {
        return Some(points);
    }
    let mut equations = Vec::with_capacity(unknowns);
    let mut rhs = vec![[0.0; D]; unknowns];
    for row in 0..unknowns {
        let parameter = parameters[row + 1];
        let span = find_span(degree, knots, count, parameter);
        let values = basis(degree, knots, span, parameter, 0)[0];
        rhs[row] = data[row + 1];
        let mut terms = Vec::with_capacity(degree + 1);
        for (offset, value) in values.iter().enumerate().take(degree + 1) {
            let index = span - degree + offset;
            if index == 0 || index == count - 1 {
                rhs[row] = sub(rhs[row], scale(points[index], *value));
            } else {
                terms.push((index - 1, *value));
            }
        }
        equations.push(Equation::from_terms(&terms));
    }
    let rhs = solve(equations, rhs)?;
    points[1..count - 1].copy_from_slice(&rhs);
    points
        .iter()
        .all(|point| point.iter().all(|component| component.is_finite()))
        .then_some(points)
}

/// A clamped, uniform knot vector for `count` control points of `degree`:
/// the one the sketch's control-vertex tool gives a spline.
pub(crate) fn clamped_uniform_knots(count: usize, degree: usize) -> Vec<f64> {
    let degree = degree.min(count.saturating_sub(1)).max(1);
    let interior = count.saturating_sub(degree + 1);
    let mut knots = vec![0.0; degree + 1];
    for index in 1..=interior {
        knots.push(index as f64 / (interior + 1) as f64);
    }
    knots.extend(std::iter::repeat_n(1.0, degree + 1));
    knots
}

/// The spline the sketch's fit-point tool draws through `points`, so that a
/// script and a sketch draw the same curve.
///
/// Open, it is the curve of degree `min(3, n − 1)` through the points at
/// their chord-length parameters, on knots averaged from them. Closed, it
/// is the clamped cubic through the points and back to the first that
/// leaves the first point along the tangent it arrives on: with chord-length
/// parameters `ū`, the tangent a periodic curve would have at the seam — the
/// chord from the last point to the second over the parameter span it
/// covers — fixes the second and the second-to-last control points, and one
/// square system gives the rest. The curve is C¹ at the seam and clamped, so
/// it starts and ends on the first point.
pub(crate) fn fit_points(points: &[[f64; 2]], closed: bool) -> Option<SplineCurve2> {
    let count = points.len();
    if count < 2 || (closed && count < 3) {
        return None;
    }
    let mut data = points.to_vec();
    if closed {
        data.push(points[0]);
    }
    let chords = data
        .windows(2)
        .map(|pair| norm(sub(pair[1], pair[0])))
        .collect::<Vec<_>>();
    let total = chords.iter().sum::<f64>();
    if !total.is_finite() || total < 1.0e-12 || chords.iter().any(|chord| *chord < 1.0e-12) {
        return None;
    }
    let mut parameters = Vec::with_capacity(data.len());
    let mut running = 0.0;
    parameters.push(0.0);
    for chord in &chords {
        running += chord;
        parameters.push((running / total).clamp(0.0, 1.0));
    }
    let last = parameters.len() - 1;
    parameters[last] = 1.0;
    if !closed {
        let degree = (count - 1).min(3);
        let knots = interpolation_knots(&parameters, degree);
        let control = interpolate(degree, &knots, &parameters, &data)?;
        return SplineCurve2::new(degree, knots, control).ok();
    }
    let n = count;
    let degree = 3;
    let total_points = n + 3;
    let mut knots = vec![0.0; degree + 1];
    knots.extend_from_slice(&parameters[1..n]);
    knots.extend(std::iter::repeat_n(1.0, degree + 1));
    let first_span = parameters[1];
    let last_span = 1.0 - parameters[n - 1];
    let seam = scale(
        sub(points[1], points[n - 1]),
        1.0 / (first_span + last_span),
    );
    let mut control = vec![[0.0; 2]; total_points];
    control[0] = points[0];
    control[1] = add(points[0], scale(seam, first_span / 3.0));
    control[total_points - 1] = points[0];
    control[total_points - 2] = sub(points[0], scale(seam, last_span / 3.0));
    let unknowns = total_points - 4;
    let mut equations = Vec::with_capacity(unknowns);
    let mut rhs = vec![[0.0; 2]; unknowns];
    for (row, known) in rhs.iter_mut().enumerate() {
        let k = row + 1;
        let span = find_span(degree, &knots, total_points, parameters[k]);
        let values = basis(degree, &knots, span, parameters[k], 0)[0];
        *known = data[k];
        let mut terms = Vec::with_capacity(degree + 1);
        for (offset, value) in values.iter().enumerate().take(degree + 1) {
            let index = span - degree + offset;
            if (2..total_points - 2).contains(&index) {
                terms.push((index - 2, *value));
            } else {
                *known = sub(*known, scale(control[index], *value));
            }
        }
        equations.push(Equation::from_terms(&terms));
    }
    let solved = solve(equations, rhs)?;
    control[2..total_points - 2].copy_from_slice(&solved);
    SplineCurve2::new(degree, knots, control).ok()
}

/// One equation of a banded system: its coefficients from column `first`
/// on, and zero in every column outside them.
struct Equation {
    first: usize,
    coefficients: Vec<f64>,
}

impl Equation {
    /// The equation with `terms`, `(column, coefficient)` in rising columns.
    fn from_terms(terms: &[(usize, f64)]) -> Self {
        let first = terms.first().map_or(0, |(column, _)| *column);
        let mut coefficients = Vec::with_capacity(terms.len());
        for (column, coefficient) in terms {
            coefficients.resize(column - first, 0.0);
            coefficients.push(*coefficient);
        }
        Self {
            first,
            coefficients,
        }
    }

    /// One past the last column the equation holds.
    fn end(&self) -> usize {
        self.first + self.coefficients.len()
    }

    fn at(&self, column: usize) -> f64 {
        column
            .checked_sub(self.first)
            .and_then(|offset| self.coefficients.get(offset))
            .copied()
            .unwrap_or(0.0)
    }
}

/// `A·x = b`, each unknown a point, by Gaussian elimination with partial
/// pivoting in a fixed order, over the band the equations fill.
///
/// Interpolation's matrix is banded: row `i` holds only the `p + 1` basis
/// functions that do not vanish at its parameter, which rise with `i`. A row
/// further down than the band reaches below the diagonal has nothing in the
/// column being eliminated, so the pivot search and the elimination stop at
/// the band's foot, and each row is only as wide as the pivot rows swapped
/// into it make it. That is the dense elimination exactly — every step it
/// would take outside the band subtracts a zero — at a cost that grows with
/// the number of rows rather than its cube: a sweep's walls are interpolated
/// through a copy of the profile for every row, and there are hundreds.
fn solve<const D: usize>(mut rows: Vec<Equation>, mut rhs: Vec<[f64; D]>) -> Option<Vec<[f64; D]>> {
    let size = rhs.len();
    if rows.len() != size {
        return None;
    }
    let below = rows
        .iter()
        .enumerate()
        .map(|(row, equation)| row.saturating_sub(equation.first))
        .max()
        .unwrap_or(0);
    for column in 0..size {
        let foot = (column + below).min(size - 1);
        let mut pivot = column;
        for row in column + 1..=foot {
            if rows[row].at(column).abs() > rows[pivot].at(column).abs() {
                pivot = row;
            }
        }
        let magnitude = rows[pivot].at(column).abs();
        if magnitude.is_nan() || magnitude <= 1.0e-14 {
            return None;
        }
        rows.swap(column, pivot);
        rhs.swap(column, pivot);
        let (upper, lower) = rows.split_at_mut(column + 1);
        let source = &upper[column];
        let end = source.end();
        for (offset, target) in lower[..foot - column].iter_mut().enumerate() {
            let factor = target.at(column) / source.at(column);
            if factor == 0.0 {
                continue;
            }
            // A row with something in this column starts at or before it;
            // it is widened to hold everything the pivot row reaches.
            if end > target.end() {
                target.coefficients.resize(end - target.first, 0.0);
            }
            for entry in column..end {
                target.coefficients[entry - target.first] -= factor * source.at(entry);
            }
            let row = column + 1 + offset;
            rhs[row] = sub(rhs[row], scale(rhs[column], factor));
        }
    }
    for (row, equation) in rows.iter().enumerate().rev() {
        let mut value = rhs[row];
        for (entry, solved) in rhs
            .iter()
            .enumerate()
            .take(equation.end().min(size))
            .skip(row + 1)
        {
            value = sub(value, scale(*solved, equation.at(entry)));
        }
        rhs[row] = scale(value, 1.0 / equation.at(row));
    }
    rhs.iter()
        .all(|point| point.iter().all(|component| component.is_finite()))
        .then_some(rhs)
}

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// The stored content of a B-spline surface. `points[i·counts[1] + j]` is
/// the control point `P_ij`, `i` along `u` and `j` along `v`.
#[derive(Debug, PartialEq)]
pub(crate) struct SurfaceData {
    degree: [usize; 2],
    knots: [Vec<f64>; 2],
    counts: [usize; 2],
    points: Vec<[f64; 3]>,
}

impl SurfaceData {
    fn hash_content(&self, hasher: &mut DefaultHasher) {
        self.degree.hash(hasher);
        self.counts.hash(hasher);
        for knots in &self.knots {
            for knot in knots {
                knot.to_bits().hash(hasher);
            }
        }
        for point in &self.points {
            for component in point {
                component.to_bits().hash(hasher);
            }
        }
    }
}

interned!(SurfaceData);

/// A B-spline surface: a `Copy` handle to interned content.
#[derive(Clone, Copy)]
pub(crate) struct SplineSurface(&'static SurfaceData);

impl PartialEq for SplineSurface {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0) || self.0 == other.0
    }
}

impl std::fmt::Debug for SplineSurface {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "SplineSurface{{degree: {:?}, points: {:?}}}",
            self.0.degree, self.0.counts
        )
    }
}

/// The face integrals of one B-spline face, as [`crate::ruled::RuledMeasures`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SplineMeasures {
    pub(crate) area: f64,
    pub(crate) flux: f64,
    pub(crate) moment: Vector3,
}

/// The value and both first and second partial derivatives at one point.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SurfaceJet {
    pub(crate) point: Point3,
    pub(crate) u: Vector3,
    pub(crate) v: Vector3,
    pub(crate) uu: Vector3,
    pub(crate) uv: Vector3,
    pub(crate) vv: Vector3,
}

impl SplineSurface {
    /// A clamped, non-rational tensor-product surface, validated and interned.
    pub(crate) fn new(
        degree: [usize; 2],
        knots: [Vec<f64>; 2],
        counts: [usize; 2],
        points: Vec<[f64; 3]>,
    ) -> Result<Self, SplineError> {
        if points.len() != counts[0] * counts[1] {
            return Err(SplineError::Knots);
        }
        if points
            .iter()
            .flatten()
            .any(|component| !component.is_finite())
        {
            return Err(SplineError::NonFinite);
        }
        validate_knots(degree[0], &knots[0], counts[0])?;
        validate_knots(degree[1], &knots[1], counts[1])?;
        let [u_knots, v_knots] = knots;
        Ok(Self(intern(SurfaceData {
            degree,
            knots: [
                u_knots.into_iter().map(canonical).collect(),
                v_knots.into_iter().map(canonical).collect(),
            ],
            counts,
            points: points
                .into_iter()
                .map(|point| point.map(canonical))
                .collect(),
        })))
    }

    /// The surface whose rows along `u` are `rows`, which share one degree
    /// and one knot vector, with `v` of `degree` over `knots`: row `j` is the
    /// curve of the control points `P_·j`.
    pub(crate) fn from_rows(rows: &[SplineCurve3], degree: usize, knots: Vec<f64>) -> Option<Self> {
        let first = rows.first()?;
        if rows
            .iter()
            .any(|row| row.degree() != first.degree() || row.knots() != first.knots())
        {
            return None;
        }
        let (count_u, count_v) = (first.count(), rows.len());
        let mut points = vec![[0.0; 3]; count_u * count_v];
        for (j, row) in rows.iter().enumerate() {
            for (i, point) in row.points().iter().enumerate() {
                points[i * count_v + j] = *point;
            }
        }
        Self::new(
            [first.degree(), degree],
            [first.knots().to_vec(), knots],
            [count_u, count_v],
            points,
        )
        .ok()
    }

    /// The wall a straight sweep of `profile` by `offset` makes: degree one
    /// along the sweep, over `v ∈ [0, 1]`, its bottom row the profile and its
    /// top row the profile moved.
    pub(crate) fn ruled(bottom: SplineCurve3, top: SplineCurve3) -> Option<Self> {
        Self::from_rows(&[bottom, top], 1, vec![0.0, 0.0, 1.0, 1.0])
    }

    /// The surface through `rows` at the rising `parameters` along `v`: the
    /// skinned surface of a loft. The rows share one basis
    /// ([`common_basis`]), and each column of the net is the curve of degree
    /// `min(3, rows − 1)` through the rows' control points at the
    /// parameters, so the surface passes through every row exactly and is
    /// C² along `v` between them.
    pub(crate) fn skinned(rows: &[SplineCurve3], parameters: &[f64]) -> Option<Self> {
        let count_v = rows.len();
        if count_v < 2 || parameters.len() != count_v {
            return None;
        }
        let first = rows[0];
        let degree = (count_v - 1).min(3);
        let knots = interpolation_knots(parameters, degree);
        let count_u = first.count();
        let mut points = vec![[0.0; 3]; count_u * count_v];
        for i in 0..count_u {
            let data = rows.iter().map(|row| row.points()[i]).collect::<Vec<_>>();
            let column = interpolate(degree, &knots, parameters, &data)?;
            for (j, point) in column.into_iter().enumerate() {
                points[i * count_v + j] = point;
            }
        }
        Self::new(
            [first.degree(), degree],
            [first.knots().to_vec(), knots],
            [count_u, count_v],
            points,
        )
        .ok()
    }

    pub(crate) fn degree(self) -> [usize; 2] {
        self.0.degree
    }

    pub(crate) fn knots(self) -> [&'static [f64]; 2] {
        [&self.0.knots[0], &self.0.knots[1]]
    }

    pub(crate) fn counts(self) -> [usize; 2] {
        self.0.counts
    }

    pub(crate) fn points(self) -> &'static [[f64; 3]] {
        &self.0.points
    }

    fn control(self, i: usize, j: usize) -> [f64; 3] {
        self.0.points[i * self.0.counts[1] + j]
    }

    /// `(u_min, u_max, v_min, v_max)`.
    pub(crate) fn domain(self) -> (f64, f64, f64, f64) {
        let [pu, pv] = self.0.degree;
        let [nu, nv] = self.0.counts;
        (
            self.0.knots[0][pu],
            self.0.knots[0][nu],
            self.0.knots[1][pv],
            self.0.knots[1][nv],
        )
    }

    /// The nonempty spans along `u` (`direction` 0) or `v` meeting
    /// `[from, to]`.
    pub(crate) fn spans(self, direction: usize, from: f64, to: f64) -> Vec<(f64, f64)> {
        spans_within(
            self.0.degree[direction],
            &self.0.knots[direction],
            self.0.counts[direction],
            from,
            to,
        )
    }

    /// `S(u, v)`. On the boundary of the domain the point is the boundary
    /// curve's, evaluated from that row or column of the net alone exactly as
    /// the edge along it evaluates its own curve, so an edge and the surface
    /// agree on every point they share, to the bit.
    pub(crate) fn evaluate(self, point: Point2) -> Point3 {
        let (u, v) = (point.x, point.y);
        let [pu, pv] = self.0.degree;
        let [nu, nv] = self.0.counts;
        let [knots_u, knots_v] = [&self.0.knots[0], &self.0.knots[1]];
        let (u_min, u_max, v_min, v_max) = self.domain();
        if v.is_nan() || v <= v_min {
            return point3(curve_point(pu, knots_u, nu, u, &|i| self.control(i, 0)));
        }
        if v >= v_max {
            return point3(curve_point(pu, knots_u, nu, u, &|i| {
                self.control(i, nv - 1)
            }));
        }
        if u.is_nan() || u <= u_min {
            return point3(curve_point(pv, knots_v, nv, v, &|j| self.control(0, j)));
        }
        if u >= u_max {
            return point3(curve_point(pv, knots_v, nv, v, &|j| {
                self.control(nu - 1, j)
            }));
        }
        let span_u = find_span(pu, knots_u, nu, u);
        let span_v = find_span(pv, knots_v, nv, v);
        let basis_u = basis(pu, knots_u, span_u, u, 0)[0];
        let basis_v = basis(pv, knots_v, span_v, v, 0)[0];
        let rows = std::array::from_fn::<[f64; 3], ORDER, _>(|s| {
            if s > pv {
                return [0.0; 3];
            }
            combine(&basis_u, pu, span_u - pu, &|i| {
                self.control(i, span_v - pv + s)
            })
        });
        point3(combine(&basis_v, pv, 0, &|s| rows[s]))
    }

    /// The point and every partial derivative up to the second.
    ///
    /// As for a curve, the derivatives are taken of the control points less
    /// the cell's first one, which the vanishing sums of the basis
    /// derivatives leave them unchanged by, so that far from the origin they
    /// do not cancel down to the rounding of the coordinates. The point
    /// itself is evaluated whole.
    pub(crate) fn jet(self, point: Point2) -> SurfaceJet {
        let [pu, pv] = self.0.degree;
        let [nu, nv] = self.0.counts;
        let [knots_u, knots_v] = [&self.0.knots[0], &self.0.knots[1]];
        let (u_min, u_max, v_min, v_max) = self.domain();
        let u = point.x.clamp(u_min, u_max);
        let v = point.y.clamp(v_min, v_max);
        let span_u = find_span(pu, knots_u, nu, u);
        let span_v = find_span(pv, knots_v, nv, v);
        let ders_u = basis(pu, knots_u, span_u, u, 2);
        let ders_v = basis(pv, knots_v, span_v, v, 2);
        let anchor = self.control(span_u - pu, span_v - pv);
        let mut terms = [[0.0; 3]; 6];
        for (s, j) in (span_v - pv..=span_v).enumerate() {
            let along = [0, 1, 2].map(|k| {
                combine(&ders_u[k], pu, span_u - pu, &|i| {
                    sub(self.control(i, j), anchor)
                })
            });
            // (k, l): ∂^{k+l}/∂u^k ∂v^l, in the order point, u, v, uu, uv, vv.
            for (slot, (k, l)) in [(0, 0), (1, 0), (0, 1), (2, 0), (1, 1), (0, 2)]
                .into_iter()
                .enumerate()
            {
                let weight = ders_v[l][s];
                for axis in 0..3 {
                    terms[slot][axis] = weight.mul_add(along[k][axis], terms[slot][axis]);
                }
            }
        }
        SurfaceJet {
            point: self.evaluate(point),
            u: vector3(terms[1]),
            v: vector3(terms[2]),
            uu: vector3(terms[3]),
            uv: vector3(terms[4]),
            vv: vector3(terms[5]),
        }
    }

    /// `S`, `∂S/∂u` and `∂S/∂v`.
    pub(crate) fn frame(self, point: Point2) -> (Point3, Vector3, Vector3) {
        let jet = self.jet(point);
        (jet.point, jet.u, jet.v)
    }

    /// The unnormalised normal `∂S/∂u × ∂S/∂v`, which points out of the
    /// material by the builder's choice of parameter directions.
    pub(crate) fn normal(self, point: Point2) -> Vector3 {
        let (_, along_u, along_v) = self.frame(point);
        along_u.cross(along_v)
    }

    pub(crate) fn unit_normal(self, point: Point2) -> Option<Vector3> {
        let normal = self.normal(point);
        let length = normal.length();
        (length.is_finite() && length > f64::EPSILON * self.scale().powi(2).max(1.0))
            .then(|| normal / length)
    }

    pub(crate) fn map_tangent(self, point: Point2, tangent: Vector2) -> Vector3 {
        let (_, along_u, along_v) = self.frame(point);
        along_u * tangent.x + along_v * tangent.y
    }

    pub(crate) fn is_finite(self) -> bool {
        // Stored surfaces are finite by construction.
        true
    }

    /// A length the surface is the size of: the diagonal of its control
    /// net's box, which the surface lies inside.
    pub(crate) fn scale(self) -> f64 {
        let mut low = [f64::INFINITY; 3];
        let mut high = [f64::NEG_INFINITY; 3];
        for point in self.points() {
            for axis in 0..3 {
                low[axis] = low[axis].min(point[axis]);
                high[axis] = high[axis].max(point[axis]);
            }
        }
        norm(sub(high, low))
    }

    /// The row of control points `P_·j` as a curve along `u`: the boundary
    /// curve at `v_min` for `j = 0` and at `v_max` for the last.
    pub(crate) fn row(self, j: usize) -> Option<SplineCurve3> {
        let [nu, nv] = self.0.counts;
        if j >= nv {
            return None;
        }
        SplineCurve3::new(
            self.0.degree[0],
            self.0.knots[0].clone(),
            (0..nu).map(|i| self.control(i, j)).collect(),
        )
        .ok()
    }

    /// The column `P_i·` as a curve along `v`.
    pub(crate) fn column(self, i: usize) -> Option<SplineCurve3> {
        let [nu, nv] = self.0.counts;
        if i >= nu {
            return None;
        }
        SplineCurve3::new(
            self.0.degree[1],
            self.0.knots[1].clone(),
            (0..nv).map(|j| self.control(i, j)).collect(),
        )
        .ok()
    }

    /// The curve `u ↦ S(u, v)` at a fixed `v`, exactly: its control points
    /// are the columns blended by the basis along `v` there.
    pub(crate) fn isocurve_at_v(self, v: f64) -> Option<SplineCurve3> {
        let [pu, pv] = self.0.degree;
        let [nu, nv] = self.0.counts;
        let (_, _, v_min, v_max) = self.domain();
        if v == v_min {
            return self.row(0);
        }
        if v == v_max {
            return self.row(nv - 1);
        }
        let knots_v = &self.0.knots[1];
        let span = find_span(pv, knots_v, nv, v);
        let values = basis(pv, knots_v, span, v, 0)[0];
        SplineCurve3::new(
            pu,
            self.0.knots[0].clone(),
            (0..nu)
                .map(|i| combine(&values, pv, span - pv, &|j| self.control(i, j)))
                .collect(),
        )
        .ok()
    }

    /// The curve `v ↦ S(u, v)` at a fixed `u`.
    pub(crate) fn isocurve_at_u(self, u: f64) -> Option<SplineCurve3> {
        let [pu, pv] = self.0.degree;
        let [nu, nv] = self.0.counts;
        let (u_min, u_max, _, _) = self.domain();
        if u == u_min {
            return self.column(0);
        }
        if u == u_max {
            return self.column(nu - 1);
        }
        let knots_u = &self.0.knots[0];
        let span = find_span(pu, knots_u, nu, u);
        let values = basis(pu, knots_u, span, u, 0)[0];
        SplineCurve3::new(
            pv,
            self.0.knots[1].clone(),
            (0..nv)
                .map(|j| combine(&values, pu, span - pu, &|i| self.control(i, j)))
                .collect(),
        )
        .ok()
    }

    /// The same surface with every control point mapped by `map`, which a
    /// similarity or a reflection carries a tensor-product surface by.
    pub(crate) fn mapped(self, map: impl Fn(Point3) -> Point3) -> Option<Self> {
        Self::new(
            self.0.degree,
            self.0.knots.clone(),
            self.0.counts,
            self.0
                .points
                .iter()
                .map(|point| array3(map(point3(*point))))
                .collect(),
        )
        .ok()
    }

    /// The same surface with `u` walked the other way over the negated
    /// domain, `S'(u, v) = S(−u, v)`, whose normal is the opposite one.
    pub(crate) fn reversed_u(self) -> Self {
        let [nu, nv] = self.0.counts;
        let mut points = Vec::with_capacity(self.0.points.len());
        for i in (0..nu).rev() {
            for j in 0..nv {
                points.push(self.control(i, j));
            }
        }
        Self::new(
            self.0.degree,
            [
                self.0.knots[0].iter().rev().map(|knot| -knot).collect(),
                self.0.knots[1].clone(),
            ],
            self.0.counts,
            points,
        )
        .unwrap_or(self)
    }

    /// Second-derivative bounds along `u` and `v` and of the twist over the
    /// span cell `[u_low, u_high] × [v_low, v_high]`, from the control net's
    /// differences: each derivative is a B-spline whose control points are
    /// those differences, so the largest of them bounds it.
    pub(crate) fn curvature_bounds(
        self,
        (u_low, u_high): (f64, f64),
        (v_low, v_high): (f64, f64),
    ) -> [f64; 3] {
        let [pu, pv] = self.0.degree;
        let [nu, nv] = self.0.counts;
        let [knots_u, knots_v] = [&self.0.knots[0], &self.0.knots[1]];
        let span_u = find_span(pu, knots_u, nu, 0.5 * (u_low + u_high));
        let span_v = find_span(pv, knots_v, nv, 0.5 * (v_low + v_high));
        let rate =
            |degree: usize, knots: &[f64], index: usize, first: [f64; 3], second: [f64; 3]| {
                let width = knots[index + degree + 1] - knots[index + 1];
                if width > 0.0 {
                    scale(sub(second, first), degree as f64 / width)
                } else {
                    [0.0; 3]
                }
            };
        let mut bounds = [0.0_f64; 3];
        let rows = span_v - pv..=span_v;
        let columns = span_u - pu..=span_u;
        if pu >= 2 {
            for j in rows.clone() {
                for i in span_u - pu..span_u - 1 {
                    let width = knots_u[i + pu + 1] - knots_u[i + 2];
                    if width > 0.0 {
                        let first =
                            rate(pu, knots_u, i, self.control(i, j), self.control(i + 1, j));
                        let second = rate(
                            pu,
                            knots_u,
                            i + 1,
                            self.control(i + 1, j),
                            self.control(i + 2, j),
                        );
                        bounds[0] = bounds[0]
                            .max(norm(scale(sub(second, first), (pu as f64 - 1.0) / width)));
                    }
                }
            }
        }
        if pv >= 2 {
            for i in columns.clone() {
                for j in span_v - pv..span_v - 1 {
                    let width = knots_v[j + pv + 1] - knots_v[j + 2];
                    if width > 0.0 {
                        let first =
                            rate(pv, knots_v, j, self.control(i, j), self.control(i, j + 1));
                        let second = rate(
                            pv,
                            knots_v,
                            j + 1,
                            self.control(i, j + 1),
                            self.control(i, j + 2),
                        );
                        bounds[1] = bounds[1]
                            .max(norm(scale(sub(second, first), (pv as f64 - 1.0) / width)));
                    }
                }
            }
        }
        for i in span_u - pu..span_u {
            for j in span_v - pv..span_v {
                let width_u = knots_u[i + pu + 1] - knots_u[i + 1];
                let width_v = knots_v[j + pv + 1] - knots_v[j + 1];
                if width_u > 0.0 && width_v > 0.0 {
                    let twist = sub(
                        sub(self.control(i + 1, j + 1), self.control(i, j + 1)),
                        sub(self.control(i + 1, j), self.control(i, j)),
                    );
                    bounds[2] =
                        bounds[2].max(norm(twist) * (pu as f64 / width_u) * (pv as f64 / width_v));
                }
            }
        }
        bounds
    }

    /// The parameters of the point of the surface nearest `target`.
    ///
    /// Newton's method on the squared distance with its exact Hessian, from
    /// `seed` or, without one, from the nearest of a sampling of every span
    /// cell. A step that would move away is halved, parameters stay inside
    /// the domain, and a walk that has not settled within the iteration
    /// limit is refused rather than returned.
    ///
    /// The walk has settled when a step no longer brings the point nearer,
    /// or when it moves the point by no more than the rounding of the
    /// coordinates the point is computed in. Far from the origin that
    /// rounding, not the parameters' own, bounds how still the walk can get:
    /// at a hundred kilometres a point is only known to about `10⁻¹¹`, and a
    /// test on the parameter step alone would chase that noise until the
    /// iterations ran out.
    pub(crate) fn invert(self, target: Point3, seed: Option<Point2>) -> Option<Point2> {
        if !target.is_finite() {
            return None;
        }
        let (u_min, u_max, v_min, v_max) = self.domain();
        let distance_squared = |point: Point2| {
            let offset = self.evaluate(point) - target;
            offset.dot(offset)
        };
        let mut current = match seed {
            Some(seed) if seed.is_finite() => {
                Point2::new(seed.x.clamp(u_min, u_max), seed.y.clamp(v_min, v_max))
            }
            _ => self.coarse_seed(target)?,
        };
        let mut value = distance_squared(current);
        let width = (u_max - u_min).max(v_max - v_min);
        const ITERATIONS: usize = 64;
        for _ in 0..ITERATIONS {
            let jet = self.jet(current);
            let offset = jet.point - target;
            let gradient = [offset.dot(jet.u), offset.dot(jet.v)];
            let uu = jet.u.dot(jet.u) + offset.dot(jet.uu);
            let uv = jet.u.dot(jet.v) + offset.dot(jet.uv);
            let vv = jet.v.dot(jet.v) + offset.dot(jet.vv);
            let determinant = uu.mul_add(vv, -(uv * uv));
            let step = if determinant.is_finite() && determinant > 0.0 && uu > 0.0 {
                Point2::new(
                    (vv * gradient[0] - uv * gradient[1]) / determinant,
                    (uu * gradient[1] - uv * gradient[0]) / determinant,
                )
            } else {
                let scale = jet
                    .u
                    .dot(jet.u)
                    .max(jet.v.dot(jet.v))
                    .max(f64::MIN_POSITIVE);
                Point2::new(gradient[0] / scale, gradient[1] / scale)
            };
            if !step.is_finite() {
                return None;
            }
            let mut length = 1.0;
            let mut accepted = None;
            for _ in 0..32 {
                let candidate = Point2::new(
                    (current.x - step.x * length).clamp(u_min, u_max),
                    (current.y - step.y * length).clamp(v_min, v_max),
                );
                let candidate_value = distance_squared(candidate);
                if candidate_value <= value {
                    accepted = Some((candidate, candidate_value));
                    break;
                }
                length *= 0.5;
            }
            let Some((next, next_value)) = accepted else {
                return Some(current);
            };
            let moved = (next.x - current.x).abs().max((next.y - current.y).abs());
            let carried = (jet.u * (next.x - current.x) + jet.v * (next.y - current.y)).length();
            let stalled = next_value >= value;
            current = next;
            value = next_value;
            if moved <= 4.0 * f64::EPSILON * width.max(1.0)
                || stalled
                || carried <= settled_length(jet.point)
            {
                return Some(current);
            }
        }
        None
    }

    /// The nearest of a sampling of every span cell.
    fn coarse_seed(self, target: Point3) -> Option<Point2> {
        let (u_min, u_max, v_min, v_max) = self.domain();
        let along = |direction: usize, from: f64, to: f64| {
            let mut samples = Vec::new();
            for (low, high) in self.spans(direction, from, to) {
                for step in 0..4 {
                    samples.push((high - low).mul_add(f64::from(step) / 4.0, low));
                }
            }
            samples.push(to);
            samples
        };
        let us = along(0, u_min, u_max);
        let vs = along(1, v_min, v_max);
        let mut best = None::<(f64, Point2)>;
        for u in &us {
            for v in &vs {
                let point = Point2::new(*u, *v);
                let offset = self.evaluate(point) - target;
                let distance = offset.dot(offset);
                if distance.is_finite() && best.is_none_or(|(least, _)| distance < least) {
                    best = Some((distance, point));
                }
            }
        }
        best.map(|(_, point)| point)
    }

    /// How far `target` sits from the surface, found by inversion.
    pub(crate) fn distance_to(self, target: Point3, seed: Option<Point2>) -> Option<f64> {
        let found = self.invert(target, seed)?;
        let distance = self.evaluate(found).distance(target);
        distance.is_finite().then_some(distance)
    }

    /// The smallest length the normal reaches on a sampling of every span
    /// cell of `domain`, times the domain's own area — a figure in square
    /// lengths whatever the parameters are scaled to. Zero where the surface
    /// pinches or folds.
    pub(crate) fn least_normal(self, domain: (f64, f64, f64, f64)) -> f64 {
        let (u_min, u_max, v_min, v_max) = domain;
        let mut least = f64::INFINITY;
        for (u_low, u_high) in self.spans(0, u_min, u_max) {
            for (v_low, v_high) in self.spans(1, v_min, v_max) {
                for a in 0..=4 {
                    let u = (u_high - u_low).mul_add(f64::from(a) / 4.0, u_low);
                    for b in 0..=4 {
                        let v = (v_high - v_low).mul_add(f64::from(b) / 4.0, v_low);
                        least = least.min(self.normal(Point2::new(u, v)).length());
                    }
                }
            }
        }
        least * (u_max - u_min).abs() * (v_max - v_min).abs()
    }

    /// Area, flux and first moment of the face covering the parameter
    /// rectangle `domain`, measured from `anchor`, as
    /// [`crate::ruled::RuledSurface::measures`] defines them.
    ///
    /// Each span cell is halved in both directions and integrated by the
    /// ten-point rule in each: exact for the flux, a polynomial of degree
    /// `3p − 1` along `u` and `3q − 1` along `v`, and for the moment, of
    /// degree `4p − 1` and `4q − 1`; and exponentially convergent for the
    /// area, whose integrand is the length of a polynomial normal.
    pub(crate) fn measures(self, domain: (f64, f64, f64, f64), anchor: Point3) -> SplineMeasures {
        let (u_min, u_max, v_min, v_max) = domain;
        let halves = |direction: usize, from: f64, to: f64| {
            let mut pieces = Vec::new();
            for (low, high) in self.spans(direction, from, to) {
                let middle = 0.5 * (low + high);
                pieces.push((low, middle));
                pieces.push((middle, high));
            }
            pieces
        };
        let u_nodes: Vec<(f64, f64)> = halves(0, u_min, u_max)
            .into_iter()
            .flat_map(|(low, high)| nodes(low, high).collect::<Vec<_>>())
            .collect();
        let v_nodes: Vec<(f64, f64)> = halves(1, v_min, v_max)
            .into_iter()
            .flat_map(|(low, high)| nodes(low, high).collect::<Vec<_>>())
            .collect();
        let mut area = 0.0;
        let mut flux = 0.0;
        let mut moment = Vector3::new(0.0, 0.0, 0.0);
        for (u, u_weight) in &u_nodes {
            for (v, v_weight) in &v_nodes {
                let (point, along_u, along_v) = self.frame(Point2::new(*u, *v));
                let normal = along_u.cross(along_v);
                let offset = point - anchor;
                let weight = u_weight * v_weight;
                area += weight * normal.length();
                flux += weight * offset.dot(normal);
                moment = moment + normal * (weight * 0.5 * offset.dot(offset));
            }
        }
        // A rectangle walked backwards along one parameter has the other
        // sign; the area is a length and never does.
        let sign = ((u_max - u_min) * (v_max - v_min)).signum();
        SplineMeasures {
            area: area.abs(),
            flux: flux * sign,
            moment: moment * sign,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cubic() -> SplineCurve3 {
        SplineCurve3::new(
            3,
            vec![0.0, 0.0, 0.0, 0.0, 0.4, 0.7, 1.0, 1.0, 1.0, 1.0],
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 2.0, 0.0],
                [3.0, 3.0, 1.0],
                [5.0, 1.0, 0.0],
                [6.0, -1.0, 2.0],
                [8.0, 0.0, 0.0],
            ],
        )
        .expect("a valid cubic")
    }

    fn close(first: [f64; 3], second: [f64; 3], tolerance: f64) -> bool {
        norm(sub(first, second)) <= tolerance
    }

    #[test]
    fn the_basis_is_a_partition_of_unity_with_matching_derivatives() {
        let curve = cubic();
        for step in 0..=40 {
            let t = f64::from(step) / 40.0;
            let span = find_span(3, curve.knots(), curve.count(), t);
            let ders = basis(3, curve.knots(), span, t, 2);
            let sum: f64 = ders[0].iter().take(4).sum();
            assert!((sum - 1.0).abs() < 1.0e-14, "{sum}");
            let sum_first: f64 = ders[1].iter().take(4).sum();
            assert!(sum_first.abs() < 1.0e-12, "{sum_first}");
        }
        // The derivative against a central difference away from the knots.
        for t in [0.1, 0.3, 0.55, 0.85] {
            let h = 1.0e-6;
            let [_, first, second] = curve.derivatives(t);
            let ahead = curve.evaluate(t + h);
            let behind = curve.evaluate(t - h);
            let numeric = scale(sub(ahead, behind), 0.5 / h);
            assert!(close(first, numeric, 1.0e-6), "{first:?} {numeric:?}");
            let [_, ahead_rate, _] = curve.derivatives(t + h);
            let [_, behind_rate, _] = curve.derivatives(t - h);
            let numeric_second = scale(sub(ahead_rate, behind_rate), 0.5 / h);
            assert!(close(second, numeric_second, 1.0e-4));
        }
    }

    #[test]
    fn the_ends_are_the_end_control_points_to_the_bit() {
        let curve = cubic();
        assert_eq!(curve.evaluate(0.0), curve.first());
        assert_eq!(curve.evaluate(1.0), curve.last());
    }

    #[test]
    fn refinement_elevation_and_splitting_leave_the_curve_where_it_was() {
        let curve = cubic();
        let inserted = curve.inserted(0.55, 2).inserted(0.4, 1);
        let elevated = curve.elevated(5).expect("elevates");
        let (left, right) = curve.split(0.62).expect("splits");
        assert_eq!(left.last(), right.first());
        for step in 0..=50 {
            let t = f64::from(step) / 50.0;
            let expected = curve.evaluate(t);
            assert!(close(inserted.evaluate(t), expected, 1.0e-13));
            assert!(close(elevated.evaluate(t), expected, 1.0e-13));
            let halves = if t <= 0.62 {
                left.evaluate(t)
            } else {
                right.evaluate(t)
            };
            assert!(close(halves, expected, 1.0e-13));
        }
        let reversed = curve.reversed();
        assert_eq!(reversed.reversed(), curve);
        assert!(close(reversed.evaluate(-0.3), curve.evaluate(0.3), 1.0e-14));
    }

    #[test]
    fn equal_content_is_one_stored_spline() {
        let first = cubic();
        let second = cubic();
        assert!(std::ptr::eq(first.0, second.0));
        let negative_zero = SplineCurve3::new(
            1,
            vec![0.0, 0.0, 1.0, 1.0],
            vec![[-0.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
        )
        .expect("valid");
        let zero =
            line_curve(Point3::new(0.0, 0.0, 0.0), Point3::new(1.0, 0.0, 0.0)).expect("valid");
        assert!(std::ptr::eq(negative_zero.0, zero.0));
    }

    #[test]
    fn malformed_splines_are_refused_by_reason() {
        let points = vec![[0.0, 0.0], [1.0, 1.0], [2.0, 0.0]];
        assert_eq!(
            SplineCurve2::new(2, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0], points.clone()).err(),
            Some(SplineError::Unclamped)
        );
        assert_eq!(
            SplineCurve2::new(0, vec![0.0, 1.0, 2.0, 3.0], points.clone()).err(),
            Some(SplineError::Degree)
        );
        assert_eq!(
            SplineCurve2::new(2, vec![0.0, 0.0, 0.0, 1.0, 1.0], points).err(),
            Some(SplineError::Knots)
        );
    }

    #[test]
    fn an_arc_fits_within_its_tolerance() {
        let center = Point3::new(1.0, 2.0, 3.0);
        let (u, v) = (Vector3::new(1.0, 0.0, 0.0), Vector3::new(0.0, 1.0, 0.0));
        let radius = 50.0;
        let tolerance = 1.0e-9;
        let curve = arc_curve(center, u, v, radius, 0.3, 2.0, tolerance).expect("fits");
        for step in 0..=997 {
            let t = f64::from(step) / 997.0;
            let point = curve.point(t);
            assert!(
                ((point - center).length() - radius).abs() <= tolerance,
                "{t}"
            );
        }
        assert!(close(
            curve.first(),
            array3(center + u * (radius * 0.3f64.cos()) + v * (radius * 0.3f64.sin())),
            1.0e-12
        ));
    }

    /// The dense elimination the banded one replaced, kept here as the
    /// reference it must agree with.
    fn dense_solve(mut matrix: Vec<Vec<f64>>, mut rhs: Vec<[f64; 3]>) -> Option<Vec<[f64; 3]>> {
        let size = rhs.len();
        for column in 0..size {
            let mut pivot = column;
            for row in column + 1..size {
                if matrix[row][column].abs() > matrix[pivot][column].abs() {
                    pivot = row;
                }
            }
            if matrix[pivot][column].abs() <= 1.0e-14 {
                return None;
            }
            matrix.swap(column, pivot);
            rhs.swap(column, pivot);
            for row in column + 1..size {
                let factor = matrix[row][column] / matrix[column][column];
                if factor == 0.0 {
                    continue;
                }
                let (upper, lower) = matrix.split_at_mut(row);
                for (target, source) in lower[0][column..].iter_mut().zip(&upper[column][column..])
                {
                    *target -= factor * source;
                }
                rhs[row] = sub(rhs[row], scale(rhs[column], factor));
            }
        }
        for row in (0..size).rev() {
            let mut value = rhs[row];
            for entry in row + 1..size {
                value = sub(value, scale(rhs[entry], matrix[row][entry]));
            }
            rhs[row] = scale(value, 1.0 / matrix[row][row]);
        }
        Some(rhs)
    }

    /// Interpolation's systems, solved over their band, come out as the
    /// dense elimination gives them, to the bit.
    #[test]
    fn the_banded_solve_is_the_dense_one() {
        for (count, degree) in [(4, 3), (9, 2), (40, 3), (257, 3), (30, 5)] {
            // Uneven chord-length parameters, so that pivoting has work.
            let mut parameters = vec![0.0];
            for index in 1..count {
                let step = 1.0 + 0.9 * (index as f64 * 1.7).sin();
                parameters.push(parameters[index - 1] + step);
            }
            let total = parameters[count - 1];
            let parameters = parameters
                .iter()
                .map(|value| value / total)
                .collect::<Vec<_>>();
            let knots = interpolation_knots(&parameters, degree);
            let unknowns = count - 2;
            let mut matrix = vec![vec![0.0; unknowns]; unknowns];
            let mut equations = Vec::new();
            let mut rhs = Vec::new();
            for row in 0..unknowns {
                let parameter = parameters[row + 1];
                let span = find_span(degree, &knots, count, parameter);
                let values = basis(degree, &knots, span, parameter, 0)[0];
                let mut terms = Vec::new();
                for (offset, value) in values.iter().enumerate().take(degree + 1) {
                    let index = span - degree + offset;
                    if index != 0 && index != count - 1 {
                        matrix[row][index - 1] = *value;
                        terms.push((index - 1, *value));
                    }
                }
                equations.push(Equation::from_terms(&terms));
                let angle = row as f64 * 0.37;
                rhs.push([angle.cos() * 1.0e5, angle.sin(), row as f64]);
            }
            let dense = dense_solve(matrix, rhs.clone()).expect("solves");
            let banded = solve(equations, rhs).expect("solves");
            assert_eq!(dense, banded, "{count} points of degree {degree}");
        }
    }

    #[test]
    fn a_skinned_surface_passes_through_its_rows() {
        let rows = [0.0, 3.0, 5.0, 9.0, 12.0].map(|height| {
            let lift = |point: [f64; 3]| [point[0], point[1] + 0.1 * height, point[2] + height];
            cubic().mapped(lift).expect("maps")
        });
        let parameters = [0.0, 0.25, 0.4, 0.75, 1.0];
        let surface = SplineSurface::skinned(&rows, &parameters).expect("skins");
        assert_eq!(surface.degree(), [3, 3]);
        for (row, v) in rows.iter().zip(parameters) {
            for step in 0..=20 {
                let u = f64::from(step) / 20.0;
                let point = surface.evaluate(Point2::new(u, v));
                assert!(close(array3(point), row.evaluate(u), 1.0e-12), "{u} {v}");
            }
        }
        // The first and last rows are the boundary, to the bit.
        assert_eq!(surface.row(0), Some(rows[0]));
        assert_eq!(surface.row(4), Some(rows[4]));
    }

    #[test]
    fn inversion_recovers_the_parameters_of_its_own_points() {
        let top = cubic()
            .mapped(|point| [point[0], point[1] + 1.0, point[2] + 10.0])
            .expect("maps");
        let surface = SplineSurface::ruled(cubic(), top).expect("builds");
        for (u, v) in [(0.0, 0.0), (0.3, 0.7), (1.0, 1.0), (0.5, 0.5), (0.9, 0.1)] {
            let point = surface.evaluate(Point2::new(u, v));
            let found = surface.invert(point, None).expect("converges");
            assert!(
                (found.x - u).abs() < 1.0e-9 && (found.y - v).abs() < 1.0e-9,
                "{found:?}"
            );
        }
        let middle = Point2::new(0.45, 0.5);
        let above = surface.evaluate(middle) + surface.unit_normal(middle).expect("regular") * 0.01;
        let gap = surface.distance_to(above, None).expect("converges");
        assert!((gap - 0.01).abs() < 1.0e-9, "{gap}");
    }

    #[test]
    fn a_straight_sweep_measures_its_prism() {
        // A unit square's side swept up by two: the flux of the one wall
        // through x = 1, measured from the origin, is its area times one.
        let bottom =
            line_curve(Point3::new(1.0, 0.0, 0.0), Point3::new(1.0, 1.0, 0.0)).expect("line");
        let top = line_curve(Point3::new(1.0, 0.0, 2.0), Point3::new(1.0, 1.0, 2.0)).expect("line");
        let surface = SplineSurface::ruled(bottom, top).expect("builds");
        let measures = surface.measures(surface.domain(), Point3::new(0.0, 0.0, 0.0));
        assert!((measures.area - 2.0).abs() < 1.0e-14);
        assert!((measures.flux - 2.0).abs() < 1.0e-14);
    }
}
