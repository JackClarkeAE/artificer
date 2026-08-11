//! Dependency-free geometric primitives and certified numerical filters.
//!
//! [`orient2d`] classifies the orientation of three finite binary64 points in
//! exact represented-value arithmetic. Its fast path encloses every
//! subtraction, multiplication, and final subtraction with outward-rounded
//! intervals. Ambiguous, overflowing, or underflow-sensitive filters fall back
//! to a dependency-free exact dyadic integer calculation, so every finite
//! input receives a certified clockwise, counter-clockwise, or collinear
//! result. [`Orientation2::Indeterminate`] is reserved for non-finite input.

mod foundation;
mod parametric;
mod planar;

pub use foundation::{
    Angle, Bounds2, Bounds3, Direction2, Direction3, Length, LengthUnit, Plane3, Point3,
    PrecisionPolicy, Similarity2, Vector3,
};
pub use parametric::{
    AnalyticSurface, BSplineCurve3, BSplineSurface3, BezierCurve3, BezierSurface3, GeometryError,
    NurbsCurve3, NurbsSurface3, ParameterDomain, ParametricCurve3, ParametricSurface3,
    SurfaceDerivatives,
};
pub use planar::{Arc2, Circle2, Line2, SegmentRelation, classify_segment_relation};

/// A point in two-dimensional Cartesian space.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point2 {
    pub x: f64,
    pub y: f64,
}

impl Point2 {
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    #[must_use]
    pub const fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

impl std::ops::Sub for Point2 {
    type Output = Vector2;

    fn sub(self, rhs: Self) -> Self::Output {
        Vector2::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl std::ops::Add<Vector2> for Point2 {
    type Output = Self;

    fn add(self, rhs: Vector2) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y)
    }
}

/// A displacement in two-dimensional Cartesian space.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vector2 {
    pub x: f64,
    pub y: f64,
}

impl Vector2 {
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    #[must_use]
    pub const fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

impl std::ops::Add for Vector2 {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl std::ops::Sub for Vector2 {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self::new(self.x - rhs.x, self.y - rhs.y)
    }
}

/// A finite or non-finite line segment with explicit endpoints.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment2 {
    pub start: Point2,
    pub end: Point2,
}

impl Segment2 {
    #[must_use]
    pub const fn new(start: Point2, end: Point2) -> Self {
        Self { start, end }
    }

    #[must_use]
    pub const fn is_finite(self) -> bool {
        self.start.is_finite() && self.end.is_finite()
    }

    #[must_use]
    pub fn vector(self) -> Vector2 {
        self.end - self.start
    }

    #[must_use]
    pub fn is_degenerate(self) -> bool {
        self.start == self.end
    }
}

/// Direction of a certified simple closed profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileWinding {
    CounterClockwise,
    Clockwise,
}

/// Structural reasons why a point sequence cannot describe a profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidProfile {
    /// Fewer than two points for an open path, or fewer than three distinct
    /// vertices for a closed profile.
    TooFewVertices,
    /// At least one coordinate is NaN or infinite.
    NonFiniteCoordinate,
    /// A vertex is repeated anywhere other than the required closing point.
    RepeatedVertex,
}

/// Conservative classification of a polyline intended to become a profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileClassification {
    /// The final point does not exactly equal the first point.
    Open,
    /// A simple closed profile whose winding has been certified.
    Closed { winding: ProfileWinding },
    /// Two non-adjacent edges have a certified intersection.
    SelfIntersecting,
    /// The input is structurally malformed.
    Invalid(InvalidProfile),
    /// The input is finite and structurally valid, but its topology or winding
    /// cannot be certified by the current numerical filters.
    Indeterminate,
}

/// Classifies a polyline as an open path or a certified closed profile.
///
/// A closed input repeats its first point as its final point. Exact equality is
/// intentional: interactive callers should snap coordinates before invoking
/// this function. No geometric epsilon is introduced. Adjacent polygon edges
/// may meet at their shared endpoint; all other repeated vertices are invalid.
/// A numerically unresolved edge relation or signed area returns
/// [`ProfileClassification::Indeterminate`] rather than guessing.
#[must_use]
pub fn classify_profile(points: &[Point2]) -> ProfileClassification {
    if points.len() < 2 {
        return ProfileClassification::Invalid(InvalidProfile::TooFewVertices);
    }
    if points.iter().any(|point| !point.is_finite()) {
        return ProfileClassification::Invalid(InvalidProfile::NonFiniteCoordinate);
    }

    let is_closed = points.first() == points.last();
    let vertices = if is_closed {
        &points[..points.len() - 1]
    } else {
        points
    };

    if (is_closed && vertices.len() < 3) || (!is_closed && vertices.len() < 2) {
        return ProfileClassification::Invalid(InvalidProfile::TooFewVertices);
    }

    for (index, vertex) in vertices.iter().enumerate() {
        if vertices[index + 1..].contains(vertex) {
            return ProfileClassification::Invalid(InvalidProfile::RepeatedVertex);
        }
    }

    if !is_closed {
        return ProfileClassification::Open;
    }

    for first_index in 0..vertices.len() {
        let first = Segment2::new(
            vertices[first_index],
            vertices[(first_index + 1) % vertices.len()],
        );
        for second_index in first_index + 1..vertices.len() {
            if edges_are_adjacent(first_index, second_index, vertices.len()) {
                continue;
            }
            let second = Segment2::new(
                vertices[second_index],
                vertices[(second_index + 1) % vertices.len()],
            );
            match segment_intersection(first, second) {
                SegmentIntersection::Disjoint => {}
                SegmentIntersection::Intersecting => {
                    return ProfileClassification::SelfIntersecting;
                }
                SegmentIntersection::Indeterminate => {
                    return ProfileClassification::Indeterminate;
                }
            }
        }
    }

    match signed_area_orientation(vertices) {
        Some(ProfileWinding::CounterClockwise) => ProfileClassification::Closed {
            winding: ProfileWinding::CounterClockwise,
        },
        Some(ProfileWinding::Clockwise) => ProfileClassification::Closed {
            winding: ProfileWinding::Clockwise,
        },
        None => ProfileClassification::Indeterminate,
    }
}

/// Certified result of the filtered/exact two-dimensional orientation test.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Orientation2 {
    CounterClockwise,
    Clockwise,
    Collinear,
    Indeterminate,
}

/// Certifies the sign of `(b - a) x (c - a)`.
///
/// The classification is with respect to the exact real values represented by
/// the input `f64`s. There is no geometric tolerance. The interval filter is
/// used first; its unresolved cases use exact dyadic arithmetic over the
/// binary64 significands and exponents.
#[must_use]
pub fn orient2d(a: Point2, b: Point2, c: Point2) -> Orientation2 {
    if !a.is_finite() || !b.is_finite() || !c.is_finite() {
        return Orientation2::Indeterminate;
    }

    if let Some(determinant) = determinant_interval(a, b, c) {
        if determinant.lower > 0.0 {
            return Orientation2::CounterClockwise;
        }
        if determinant.upper < 0.0 {
            return Orientation2::Clockwise;
        }
    }

    match exact_determinant_sign(a, b, c) {
        std::cmp::Ordering::Greater => Orientation2::CounterClockwise,
        std::cmp::Ordering::Less => Orientation2::Clockwise,
        std::cmp::Ordering::Equal => Orientation2::Collinear,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SegmentIntersection {
    Disjoint,
    Intersecting,
    Indeterminate,
}

fn edges_are_adjacent(first: usize, second: usize, edge_count: usize) -> bool {
    first == second || (first + 1) % edge_count == second || (second + 1) % edge_count == first
}

fn segment_intersection(first: Segment2, second: Segment2) -> SegmentIntersection {
    if bounding_boxes_are_disjoint(first, second) {
        return SegmentIntersection::Disjoint;
    }

    let first_start = orient2d(first.start, first.end, second.start);
    let first_end = orient2d(first.start, first.end, second.end);
    let second_start = orient2d(second.start, second.end, first.start);
    let second_end = orient2d(second.start, second.end, first.end);

    if orientations_are_opposite(first_start, first_end)
        && orientations_are_opposite(second_start, second_end)
    {
        return SegmentIntersection::Intersecting;
    }

    for (orientation, point, segment) in [
        (first_start, second.start, first),
        (first_end, second.end, first),
        (second_start, first.start, second),
        (second_end, first.end, second),
    ] {
        if orientation == Orientation2::Collinear && point_is_in_segment_bounds(point, segment) {
            return SegmentIntersection::Intersecting;
        }
    }

    if [first_start, first_end, second_start, second_end].contains(&Orientation2::Indeterminate) {
        SegmentIntersection::Indeterminate
    } else {
        SegmentIntersection::Disjoint
    }
}

fn bounding_boxes_are_disjoint(first: Segment2, second: Segment2) -> bool {
    first.start.x.min(first.end.x) > second.start.x.max(second.end.x)
        || second.start.x.min(second.end.x) > first.start.x.max(first.end.x)
        || first.start.y.min(first.end.y) > second.start.y.max(second.end.y)
        || second.start.y.min(second.end.y) > first.start.y.max(first.end.y)
}

fn point_is_in_segment_bounds(point: Point2, segment: Segment2) -> bool {
    (segment.start.x.min(segment.end.x)..=segment.start.x.max(segment.end.x)).contains(&point.x)
        && (segment.start.y.min(segment.end.y)..=segment.start.y.max(segment.end.y))
            .contains(&point.y)
}

fn orientations_are_opposite(first: Orientation2, second: Orientation2) -> bool {
    matches!(
        (first, second),
        (Orientation2::CounterClockwise, Orientation2::Clockwise)
            | (Orientation2::Clockwise, Orientation2::CounterClockwise)
    )
}

fn signed_area_orientation(vertices: &[Point2]) -> Option<ProfileWinding> {
    let anchor_index = vertices
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| point_total_cmp(**left, **right))
        .map(|(index, _)| index)?;
    let anchor = vertices[anchor_index];
    let mut total: Option<Interval> = None;

    for offset in 1..vertices.len() - 1 {
        let current = vertices[(anchor_index + offset) % vertices.len()];
        let next = vertices[(anchor_index + offset + 1) % vertices.len()];
        let triangle = determinant_interval(anchor, current, next)?;
        total = Some(match total {
            Some(area) => area.add(triangle)?,
            None => triangle,
        });
    }

    if let Some(total) = total {
        if total.lower > 0.0 {
            return Some(ProfileWinding::CounterClockwise);
        }
        if total.upper < 0.0 {
            return Some(ProfileWinding::Clockwise);
        }
    }

    match exact_polygon_area_sign(vertices) {
        std::cmp::Ordering::Greater => Some(ProfileWinding::CounterClockwise),
        std::cmp::Ordering::Less => Some(ProfileWinding::Clockwise),
        std::cmp::Ordering::Equal => None,
    }
}

fn determinant_interval(a: Point2, b: Point2, c: Point2) -> Option<Interval> {
    // A determinant has the same sign under a cyclic permutation, but a
    // floating-point filter can otherwise obtain enclosures of different
    // widths from different anchors. Rotate to one deterministic anchor so a
    // loop's classification cannot depend on which vertex happens to be
    // listed first. Reversal still swaps the remaining operands and sign.
    let (a, b, c) = cyclically_canonicalized(a, b, c);

    let ab_x = Interval::enclose_subtraction(b.x, a.x)?;
    let ab_y = Interval::enclose_subtraction(b.y, a.y)?;
    let ac_x = Interval::enclose_subtraction(c.x, a.x)?;
    let ac_y = Interval::enclose_subtraction(c.y, a.y)?;
    let left = ab_x.multiply(ac_y)?;
    let right = ab_y.multiply(ac_x)?;
    left.subtract(right)
}

fn cyclically_canonicalized(a: Point2, b: Point2, c: Point2) -> (Point2, Point2, Point2) {
    if point_total_cmp(b, a).is_lt() && point_total_cmp(b, c).is_lt() {
        (b, c, a)
    } else if point_total_cmp(c, a).is_lt() && point_total_cmp(c, b).is_lt() {
        (c, a, b)
    } else {
        (a, b, c)
    }
}

fn point_total_cmp(left: Point2, right: Point2) -> std::cmp::Ordering {
    left.x
        .total_cmp(&right.x)
        .then_with(|| left.y.total_cmp(&right.y))
}

#[derive(Clone, Copy, Debug)]
struct Interval {
    lower: f64,
    upper: f64,
}

impl Interval {
    fn enclose_subtraction(left: f64, right: f64) -> Option<Self> {
        let rounded = left - right;
        Self::around(rounded)
    }

    fn multiply(self, other: Self) -> Option<Self> {
        let products = [
            self.lower * other.lower,
            self.lower * other.upper,
            self.upper * other.lower,
            self.upper * other.upper,
        ];
        if products.iter().any(|value| !value.is_finite()) {
            return None;
        }

        let mut lower = products[0];
        let mut upper = products[0];
        for product in products.into_iter().skip(1) {
            lower = lower.min(product);
            upper = upper.max(product);
        }

        Self::outward(lower, upper)
    }

    fn subtract(self, other: Self) -> Option<Self> {
        let lower = self.lower - other.upper;
        let upper = self.upper - other.lower;
        Self::outward(lower, upper)
    }

    fn add(self, other: Self) -> Option<Self> {
        let lower = self.lower + other.lower;
        let upper = self.upper + other.upper;
        Self::outward(lower, upper)
    }

    fn around(value: f64) -> Option<Self> {
        if !value.is_finite() {
            return None;
        }
        Self::outward(value, value)
    }

    fn outward(lower: f64, upper: f64) -> Option<Self> {
        if !lower.is_finite() || !upper.is_finite() {
            return None;
        }

        let lower = lower.next_down();
        let upper = upper.next_up();
        if !lower.is_finite() || !upper.is_finite() {
            return None;
        }

        Some(Self { lower, upper })
    }
}

/// Unsigned arbitrary-precision integer used only by exact predicate
/// fallbacks. Binary64 exponents bound every value here to fewer than 4,200
/// bits, so this deliberately small implementation remains resource-bounded.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct BigNat(Vec<u64>);

impl BigNat {
    fn from_u64(value: u64) -> Self {
        if value == 0 {
            Self::default()
        } else {
            Self(vec![value])
        }
    }

    fn is_zero(&self) -> bool {
        self.0.is_empty()
    }

    fn normalize(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }

    fn shifted(&self, bits: u32) -> Self {
        if self.is_zero() {
            return Self::default();
        }
        let words = (bits / 64) as usize;
        let remainder = bits % 64;
        let mut result = vec![0; self.0.len() + words + usize::from(remainder != 0)];
        for (index, value) in self.0.iter().copied().enumerate() {
            result[index + words] |= value << remainder;
            if remainder != 0 {
                result[index + words + 1] |= value >> (64 - remainder);
            }
        }
        let mut result = Self(result);
        result.normalize();
        result
    }

    fn cmp_magnitude(&self, other: &Self) -> std::cmp::Ordering {
        self.0
            .len()
            .cmp(&other.0.len())
            .then_with(|| self.0.iter().rev().cmp(other.0.iter().rev()))
    }

    fn add(&self, other: &Self) -> Self {
        let length = self.0.len().max(other.0.len());
        let mut result = Vec::with_capacity(length + 1);
        let mut carry = 0_u128;
        for index in 0..length {
            let left = u128::from(self.0.get(index).copied().unwrap_or(0));
            let right = u128::from(other.0.get(index).copied().unwrap_or(0));
            let sum = left + right + carry;
            result.push(sum as u64);
            carry = sum >> 64;
        }
        if carry != 0 {
            result.push(carry as u64);
        }
        Self(result)
    }

    /// Subtracts `other` from `self`; the caller proves `self >= other`.
    fn subtract(&self, other: &Self) -> Self {
        debug_assert!(self.cmp_magnitude(other).is_ge());
        let mut result = Vec::with_capacity(self.0.len());
        let mut borrow = 0_u128;
        for index in 0..self.0.len() {
            let left = u128::from(self.0[index]);
            let right = u128::from(other.0.get(index).copied().unwrap_or(0)) + borrow;
            if left >= right {
                result.push((left - right) as u64);
                borrow = 0;
            } else {
                result.push(((1_u128 << 64) + left - right) as u64);
                borrow = 1;
            }
        }
        debug_assert_eq!(borrow, 0);
        let mut result = Self(result);
        result.normalize();
        result
    }

    fn multiply(&self, other: &Self) -> Self {
        if self.is_zero() || other.is_zero() {
            return Self::default();
        }
        let mut result = vec![0_u64; self.0.len() + other.0.len()];
        for (left_index, left) in self.0.iter().copied().enumerate() {
            let mut carry = 0_u128;
            for (right_index, right) in other.0.iter().copied().enumerate() {
                let index = left_index + right_index;
                let product =
                    u128::from(left) * u128::from(right) + u128::from(result[index]) + carry;
                result[index] = product as u64;
                carry = product >> 64;
            }
            let mut index = left_index + other.0.len();
            while carry != 0 {
                if index == result.len() {
                    result.push(0);
                }
                let sum = u128::from(result[index]) + carry;
                result[index] = sum as u64;
                carry = sum >> 64;
                index += 1;
            }
        }
        let mut result = Self(result);
        result.normalize();
        result
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SignedBig {
    sign: i8,
    magnitude: BigNat,
}

impl SignedBig {
    fn new(sign: i8, magnitude: BigNat) -> Self {
        if magnitude.is_zero() {
            Self::default()
        } else {
            Self {
                sign: sign.signum(),
                magnitude,
            }
        }
    }

    fn shifted(&self, bits: u32) -> Self {
        Self::new(self.sign, self.magnitude.shifted(bits))
    }

    fn negate(mut self) -> Self {
        self.sign = -self.sign;
        self
    }

    fn add(&self, other: &Self) -> Self {
        match (self.sign, other.sign) {
            (0, _) => other.clone(),
            (_, 0) => self.clone(),
            (left, right) if left == right => Self::new(left, self.magnitude.add(&other.magnitude)),
            _ => match self.magnitude.cmp_magnitude(&other.magnitude) {
                std::cmp::Ordering::Greater => {
                    Self::new(self.sign, self.magnitude.subtract(&other.magnitude))
                }
                std::cmp::Ordering::Less => {
                    Self::new(other.sign, other.magnitude.subtract(&self.magnitude))
                }
                std::cmp::Ordering::Equal => Self::default(),
            },
        }
    }

    fn subtract(&self, other: &Self) -> Self {
        self.add(&other.clone().negate())
    }

    fn multiply(&self, other: &Self) -> Self {
        Self::new(
            self.sign * other.sign,
            self.magnitude.multiply(&other.magnitude),
        )
    }

    fn ordering(&self) -> std::cmp::Ordering {
        self.sign.cmp(&0)
    }
}

#[derive(Clone, Debug)]
struct ExactDyadic {
    integer: SignedBig,
    exponent: i32,
}

impl ExactDyadic {
    fn from_f64(value: f64) -> Self {
        debug_assert!(value.is_finite());
        let bits = value.to_bits();
        let negative = bits >> 63 != 0;
        let exponent_bits = ((bits >> 52) & 0x7ff) as i32;
        let fraction = bits & ((1_u64 << 52) - 1);
        let (mantissa, exponent) = if exponent_bits == 0 {
            (fraction, -1074)
        } else {
            ((1_u64 << 52) | fraction, exponent_bits - 1023 - 52)
        };
        Self {
            integer: SignedBig::new(if negative { -1 } else { 1 }, BigNat::from_u64(mantissa)),
            exponent,
        }
    }

    fn align(&self, other: &Self) -> (SignedBig, SignedBig, i32) {
        let exponent = self.exponent.min(other.exponent);
        (
            self.integer.shifted((self.exponent - exponent) as u32),
            other.integer.shifted((other.exponent - exponent) as u32),
            exponent,
        )
    }

    fn add(&self, other: &Self) -> Self {
        let (left, right, exponent) = self.align(other);
        Self {
            integer: left.add(&right),
            exponent,
        }
    }

    fn subtract(&self, other: &Self) -> Self {
        let (left, right, exponent) = self.align(other);
        Self {
            integer: left.subtract(&right),
            exponent,
        }
    }

    fn multiply(&self, other: &Self) -> Self {
        Self {
            integer: self.integer.multiply(&other.integer),
            exponent: self.exponent + other.exponent,
        }
    }

    fn ordering(&self) -> std::cmp::Ordering {
        self.integer.ordering()
    }
}

fn exact_determinant_sign(a: Point2, b: Point2, c: Point2) -> std::cmp::Ordering {
    let ax = ExactDyadic::from_f64(a.x);
    let ay = ExactDyadic::from_f64(a.y);
    let ab_x = ExactDyadic::from_f64(b.x).subtract(&ax);
    let ab_y = ExactDyadic::from_f64(b.y).subtract(&ay);
    let ac_x = ExactDyadic::from_f64(c.x).subtract(&ax);
    let ac_y = ExactDyadic::from_f64(c.y).subtract(&ay);
    ab_x.multiply(&ac_y)
        .subtract(&ab_y.multiply(&ac_x))
        .ordering()
}

fn exact_polygon_area_sign(vertices: &[Point2]) -> std::cmp::Ordering {
    let mut total = ExactDyadic {
        integer: SignedBig::default(),
        exponent: 0,
    };
    for (first, second) in vertices
        .iter()
        .copied()
        .zip(vertices.iter().copied().cycle().skip(1))
        .take(vertices.len())
    {
        let cross = ExactDyadic::from_f64(first.x)
            .multiply(&ExactDyadic::from_f64(second.y))
            .subtract(&ExactDyadic::from_f64(first.y).multiply(&ExactDyadic::from_f64(second.x)));
        total = total.add(&cross);
    }
    total.ordering()
}

#[cfg(test)]
mod tests {
    use super::{
        InvalidProfile, Orientation2, Point2, ProfileClassification, ProfileWinding, Segment2,
        Vector2, classify_profile, orient2d,
    };

    #[test]
    fn owned_vector_and_segment_primitives_preserve_endpoints() {
        let start = Point2::new(-2.0, 3.0);
        let displacement = Vector2::new(5.0, -7.0);
        let end = start + displacement;
        let segment = Segment2::new(start, end);

        assert_eq!(end, Point2::new(3.0, -4.0));
        assert_eq!(segment.vector(), displacement);
        assert!(segment.is_finite());
        assert!(!segment.is_degenerate());
        assert!(Segment2::new(start, start).is_degenerate());
    }

    #[test]
    fn classifies_open_paths_and_both_closed_windings() {
        let open = [
            Point2::new(0.0, 0.0),
            Point2::new(4.0, 0.0),
            Point2::new(4.0, 3.0),
        ];
        let counter_clockwise = [
            Point2::new(0.0, 0.0),
            Point2::new(4.0, 0.0),
            Point2::new(4.0, 3.0),
            Point2::new(0.0, 3.0),
            Point2::new(0.0, 0.0),
        ];
        let clockwise = [
            Point2::new(0.0, 0.0),
            Point2::new(0.0, 3.0),
            Point2::new(4.0, 3.0),
            Point2::new(4.0, 0.0),
            Point2::new(0.0, 0.0),
        ];

        assert_eq!(classify_profile(&open), ProfileClassification::Open);
        assert_eq!(
            classify_profile(&counter_clockwise),
            ProfileClassification::Closed {
                winding: ProfileWinding::CounterClockwise
            }
        );
        assert_eq!(
            classify_profile(&clockwise),
            ProfileClassification::Closed {
                winding: ProfileWinding::Clockwise
            }
        );
    }

    #[test]
    fn reversal_flips_only_the_winding() {
        let forward = [
            Point2::new(-3.0, -1.0),
            Point2::new(5.0, -1.0),
            Point2::new(4.0, 2.0),
            Point2::new(-1.0, 4.0),
            Point2::new(-3.0, -1.0),
        ];
        let reverse = [
            Point2::new(-3.0, -1.0),
            Point2::new(-1.0, 4.0),
            Point2::new(4.0, 2.0),
            Point2::new(5.0, -1.0),
            Point2::new(-3.0, -1.0),
        ];

        assert_eq!(
            classify_profile(&forward),
            ProfileClassification::Closed {
                winding: ProfileWinding::CounterClockwise
            }
        );
        assert_eq!(
            classify_profile(&reverse),
            ProfileClassification::Closed {
                winding: ProfileWinding::Clockwise
            }
        );
    }

    #[test]
    fn cyclic_start_and_reversal_preserve_profile_topology() {
        let fixtures = [
            vec![(0, 0), (7, 0), (9, 4), (4, 7), (-2, 3)],
            vec![(0, 0), (6, 0), (6, 5), (3, 2), (0, 5)],
            vec![(0, 0), (6, 6), (0, 6), (6, 0)],
        ];

        for vertices in fixtures {
            let profile = closed_integer_profile(&vertices);
            let expected = classify_profile(&profile);
            assert!(matches!(
                expected,
                ProfileClassification::Closed { .. } | ProfileClassification::SelfIntersecting
            ));

            for start in 0..vertices.len() {
                let rotated = rotate_closed_profile(&profile, start);
                assert_eq!(
                    classify_profile(&rotated),
                    expected,
                    "cyclic start {start} changed {vertices:?}"
                );

                let reversed = reverse_closed_profile(&rotated);
                let reversed_expected = match expected {
                    ProfileClassification::Closed {
                        winding: ProfileWinding::CounterClockwise,
                    } => ProfileClassification::Closed {
                        winding: ProfileWinding::Clockwise,
                    },
                    ProfileClassification::Closed {
                        winding: ProfileWinding::Clockwise,
                    } => ProfileClassification::Closed {
                        winding: ProfileWinding::CounterClockwise,
                    },
                    ProfileClassification::SelfIntersecting => {
                        ProfileClassification::SelfIntersecting
                    }
                    _ => unreachable!("fixture was checked above"),
                };
                assert_eq!(
                    classify_profile(&reversed),
                    reversed_expected,
                    "reversal changed the topology of {vertices:?} from start {start}"
                );
            }
        }
    }

    #[test]
    fn certifies_a_bow_tie_as_self_intersecting() {
        let bow_tie = [
            Point2::new(0.0, 0.0),
            Point2::new(4.0, 4.0),
            Point2::new(0.0, 4.0),
            Point2::new(4.0, 0.0),
            Point2::new(0.0, 0.0),
        ];

        assert_eq!(
            classify_profile(&bow_tie),
            ProfileClassification::SelfIntersecting
        );
    }

    #[test]
    fn nonadjacent_edge_contacts_are_conservatively_self_intersecting() {
        let endpoint_on_edge = [
            Point2::new(0.0, 0.0),
            Point2::new(6.0, 0.0),
            Point2::new(6.0, 4.0),
            Point2::new(3.0, 0.0),
            Point2::new(0.0, 4.0),
            Point2::new(0.0, 0.0),
        ];
        let collinear_overlap = [
            Point2::new(0.0, 0.0),
            Point2::new(6.0, 0.0),
            Point2::new(4.0, 0.0),
            Point2::new(2.0, 0.0),
            Point2::new(0.0, 4.0),
            Point2::new(0.0, 0.0),
        ];

        for profile in [&endpoint_on_edge[..], &collinear_overlap[..]] {
            assert_eq!(
                classify_profile(profile),
                ProfileClassification::SelfIntersecting
            );
        }
    }

    #[test]
    fn certifiable_grid_profiles_match_an_exact_integer_oracle() {
        let mut state = 0xD1B5_4A32_D192_ED03_u64;
        let mut simple_cases = 0;
        let mut intersecting_cases = 0;

        for case_index in 0..20_000 {
            let vertex_count = 4 + (next_random(&mut state) % 4) as usize;
            let vertices = (0..vertex_count)
                .map(|_| {
                    (
                        (next_random(&mut state) % 65) as i64 - 32,
                        (next_random(&mut state) % 65) as i64 - 32,
                    )
                })
                .collect::<Vec<_>>();
            let Some(expected) = exact_certifiable_profile_oracle(&vertices) else {
                continue;
            };
            let profile = closed_integer_profile(&vertices);

            assert_eq!(
                classify_profile(&profile),
                expected,
                "exact-grid oracle mismatch in case {case_index}: {vertices:?}"
            );
            match expected {
                ProfileClassification::Closed { .. } => simple_cases += 1,
                ProfileClassification::SelfIntersecting => intersecting_cases += 1,
                _ => unreachable!("the exact oracle only returns certifiable loops"),
            }

            if simple_cases >= 1_000 && intersecting_cases >= 1_000 {
                break;
            }
        }

        assert!(
            simple_cases >= 1_000,
            "oracle corpus produced only {simple_cases} simple loops"
        );
        assert!(
            intersecting_cases >= 1_000,
            "oracle corpus produced only {intersecting_cases} intersecting loops"
        );
    }

    #[test]
    fn adjacent_edges_may_share_their_declared_endpoint() {
        let pentagon = [
            Point2::new(0.0, 0.0),
            Point2::new(4.0, 0.0),
            Point2::new(6.0, 3.0),
            Point2::new(2.0, 6.0),
            Point2::new(-2.0, 3.0),
            Point2::new(0.0, 0.0),
        ];

        assert_eq!(
            classify_profile(&pentagon),
            ProfileClassification::Closed {
                winding: ProfileWinding::CounterClockwise
            }
        );
    }

    #[test]
    fn rejects_repeated_vertices_other_than_the_closing_point() {
        let consecutive = [
            Point2::new(0.0, 0.0),
            Point2::new(4.0, 0.0),
            Point2::new(4.0, 0.0),
            Point2::new(0.0, 3.0),
            Point2::new(0.0, 0.0),
        ];
        let separated = [
            Point2::new(0.0, 0.0),
            Point2::new(4.0, 0.0),
            Point2::new(4.0, 3.0),
            Point2::new(4.0, 0.0),
            Point2::new(0.0, 3.0),
            Point2::new(0.0, 0.0),
        ];

        for points in [&consecutive[..], &separated[..]] {
            assert_eq!(
                classify_profile(points),
                ProfileClassification::Invalid(InvalidProfile::RepeatedVertex)
            );
        }
    }

    #[test]
    fn snapped_profiles_survive_binary_scaling_and_representable_translation() {
        let base = [
            Point2::new(-2.0, -1.0),
            Point2::new(3.0, -1.0),
            Point2::new(3.0, 2.0),
            Point2::new(-2.0, 2.0),
            Point2::new(-2.0, -1.0),
        ];
        let expected = ProfileClassification::Closed {
            winding: ProfileWinding::CounterClockwise,
        };

        for exponent in [-200, -40, 0, 40, 200] {
            let scale = 2.0_f64.powi(exponent);
            let scaled = base.map(|point| Point2::new(point.x * scale, point.y * scale));
            assert_eq!(
                classify_profile(&scaled),
                expected,
                "failed at binary scale 2^{exponent}"
            );
        }

        let shift = 2.0_f64.powi(42);
        let translated = base.map(|point| Point2::new(point.x + shift, point.y - shift));
        assert_eq!(classify_profile(&translated), expected);
    }

    #[test]
    fn non_finite_profiles_are_invalid_and_unresolved_area_is_indeterminate() {
        let non_finite = [
            Point2::new(0.0, 0.0),
            Point2::new(f64::NAN, 1.0),
            Point2::new(2.0, 0.0),
            Point2::new(0.0, 0.0),
        ];
        let collinear = [
            Point2::new(0.0, 0.0),
            Point2::new(1.0, 1.0),
            Point2::new(2.0, 2.0),
            Point2::new(0.0, 0.0),
        ];

        assert_eq!(
            classify_profile(&non_finite),
            ProfileClassification::Invalid(InvalidProfile::NonFiniteCoordinate)
        );
        assert_eq!(
            classify_profile(&collinear),
            ProfileClassification::Indeterminate
        );
    }

    #[test]
    fn profile_classification_is_deterministic() {
        let profile = [
            Point2::new(0.25, -8.0),
            Point2::new(12.25, -8.0),
            Point2::new(10.25, 7.0),
            Point2::new(-1.75, 3.0),
            Point2::new(0.25, -8.0),
        ];
        let expected = classify_profile(&profile);

        for _ in 0..10_000 {
            assert_eq!(classify_profile(&profile), expected);
        }
    }

    fn closed_integer_profile(vertices: &[(i64, i64)]) -> Vec<Point2> {
        let mut profile = vertices
            .iter()
            .map(|&(x, y)| Point2::new(x as f64, y as f64))
            .collect::<Vec<_>>();
        profile.push(profile[0]);
        profile
    }

    fn rotate_closed_profile(profile: &[Point2], start: usize) -> Vec<Point2> {
        let vertices = &profile[..profile.len() - 1];
        let mut rotated = vertices
            .iter()
            .cycle()
            .skip(start)
            .take(vertices.len())
            .copied()
            .collect::<Vec<_>>();
        rotated.push(rotated[0]);
        rotated
    }

    fn reverse_closed_profile(profile: &[Point2]) -> Vec<Point2> {
        let mut reversed = profile[..profile.len() - 1]
            .iter()
            .rev()
            .copied()
            .collect::<Vec<_>>();
        reversed.push(reversed[0]);
        reversed
    }

    fn next_random(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *state
    }

    fn exact_certifiable_profile_oracle(vertices: &[(i64, i64)]) -> Option<ProfileClassification> {
        if vertices.len() < 3 {
            return None;
        }
        for (index, vertex) in vertices.iter().enumerate() {
            if vertices[index + 1..].contains(vertex) {
                return None;
            }
        }

        for first_index in 0..vertices.len() {
            let first = (
                vertices[first_index],
                vertices[(first_index + 1) % vertices.len()],
            );
            for second_index in first_index + 1..vertices.len() {
                if super::edges_are_adjacent(first_index, second_index, vertices.len()) {
                    continue;
                }
                let second = (
                    vertices[second_index],
                    vertices[(second_index + 1) % vertices.len()],
                );
                if exact_bounding_boxes_are_disjoint(first, second) {
                    continue;
                }

                let orientations = [
                    exact_orientation(first.0, first.1, second.0),
                    exact_orientation(first.0, first.1, second.1),
                    exact_orientation(second.0, second.1, first.0),
                    exact_orientation(second.0, second.1, first.1),
                ];
                // Boundary contacts have their own explicit regressions above.
                // This corpus compares only relations the current interval
                // filter can certify without a collinearity fallback.
                if orientations.contains(&0) {
                    return None;
                }
                if orientations[0].signum() != orientations[1].signum()
                    && orientations[2].signum() != orientations[3].signum()
                {
                    return Some(ProfileClassification::SelfIntersecting);
                }
            }
        }

        let twice_area = vertices
            .iter()
            .zip(vertices.iter().cycle().skip(1))
            .take(vertices.len())
            .map(|(&(x1, y1), &(x2, y2))| {
                i128::from(x1) * i128::from(y2) - i128::from(y1) * i128::from(x2)
            })
            .sum::<i128>();
        match twice_area.cmp(&0) {
            std::cmp::Ordering::Greater => Some(ProfileClassification::Closed {
                winding: ProfileWinding::CounterClockwise,
            }),
            std::cmp::Ordering::Less => Some(ProfileClassification::Closed {
                winding: ProfileWinding::Clockwise,
            }),
            std::cmp::Ordering::Equal => None,
        }
    }

    fn exact_orientation(a: (i64, i64), b: (i64, i64), c: (i64, i64)) -> i128 {
        i128::from(b.0 - a.0) * i128::from(c.1 - a.1)
            - i128::from(b.1 - a.1) * i128::from(c.0 - a.0)
    }

    fn exact_bounding_boxes_are_disjoint(
        first: ((i64, i64), (i64, i64)),
        second: ((i64, i64), (i64, i64)),
    ) -> bool {
        first.0.0.min(first.1.0) > second.0.0.max(second.1.0)
            || second.0.0.min(second.1.0) > first.0.0.max(first.1.0)
            || first.0.1.min(first.1.1) > second.0.1.max(second.1.1)
            || second.0.1.min(second.1.1) > first.0.1.max(first.1.1)
    }

    #[test]
    fn certifies_clear_orientations_and_reversal() {
        let a = Point2::new(0.0, 0.0);
        let b = Point2::new(3.0, 1.0);
        let c = Point2::new(1.0, 4.0);

        assert_eq!(orient2d(a, b, c), Orientation2::CounterClockwise);
        assert_eq!(orient2d(a, c, b), Orientation2::Clockwise);
        assert_eq!(orient2d(b, c, a), Orientation2::CounterClockwise);
    }

    #[test]
    fn exact_fallback_certifies_general_collinearity() {
        let origin = Point2::new(0.0, 0.0);

        assert_eq!(
            orient2d(origin, origin, Point2::new(1.0, 2.0)),
            Orientation2::Collinear
        );
        assert_eq!(
            orient2d(
                Point2::new(2.0, -4.0),
                Point2::new(2.0, 9.0),
                Point2::new(2.0, 1.0),
            ),
            Orientation2::Collinear
        );
        assert_eq!(
            orient2d(
                Point2::new(-4.0, 2.0),
                Point2::new(9.0, 2.0),
                Point2::new(1.0, 2.0),
            ),
            Orientation2::Collinear
        );
        assert_eq!(
            orient2d(origin, Point2::new(1.0, 1.0), Point2::new(2.0, 2.0)),
            Orientation2::Collinear
        );
    }

    #[test]
    fn survives_exact_power_of_two_scaling() {
        let base = [
            Point2::new(-2.0, 1.0),
            Point2::new(3.0, -1.0),
            Point2::new(1.0, 5.0),
        ];

        for exponent in [-200, -40, 0, 40, 200] {
            let scale = 2.0_f64.powi(exponent);
            let scaled = base.map(|point| Point2::new(point.x * scale, point.y * scale));
            assert_eq!(
                orient2d(scaled[0], scaled[1], scaled[2]),
                Orientation2::CounterClockwise,
                "failed at binary scale 2^{exponent}"
            );
        }
    }

    #[test]
    fn survives_representable_translation() {
        let translation = 2.0_f64.powi(42);
        let a = Point2::new(translation, -translation);
        let b = Point2::new(translation + 4.0, -translation + 1.0);
        let c = Point2::new(translation + 1.0, -translation + 8.0);

        assert_eq!(orient2d(a, b, c), Orientation2::CounterClockwise);
        assert_eq!(orient2d(a, c, b), Orientation2::Clockwise);
    }

    #[test]
    fn exact_fallback_resolves_close_cancellation() {
        let a = Point2::new(0.0, 0.0);
        let b = Point2::new(1.0, 1.0);
        let c = Point2::new(2.0, 2.0_f64.next_up());

        assert_eq!(orient2d(a, b, c), Orientation2::CounterClockwise);
        assert_eq!(orient2d(a, c, b), Orientation2::Clockwise);
    }

    #[test]
    fn cyclic_permutations_make_the_same_certification_decision() {
        // This triangle previously produced Indeterminate, Clockwise,
        // Clockwise solely from changing the cyclic anchor.
        let a = Point2::new(366_323.410_080_313_7, 708_064_694.201_921);
        let b = Point2::new(-73_971_847.405_503_9, 573_558_447.940_012_8);
        let c = Point2::new(-54_785_192.826_378_97, 608_274_460.688_888_9);

        let expected = orient2d(a, b, c);
        assert_eq!(expected, Orientation2::Clockwise);
        assert_eq!(orient2d(b, c, a), expected);
        assert_eq!(orient2d(c, a, b), expected);
        assert_eq!(orient2d(a, c, b), Orientation2::CounterClockwise);
        assert_eq!(orient2d(c, b, a), Orientation2::CounterClockwise);
        assert_eq!(orient2d(b, a, c), Orientation2::CounterClockwise);
    }

    #[test]
    fn exact_fallback_resolves_underflow_and_overflow() {
        let tiny = f64::MIN_POSITIVE;
        assert_eq!(
            orient2d(
                Point2::new(0.0, 0.0),
                Point2::new(tiny, 0.0),
                Point2::new(0.0, tiny),
            ),
            Orientation2::CounterClockwise
        );

        assert_eq!(
            orient2d(
                Point2::new(-f64::MAX, 0.0),
                Point2::new(f64::MAX, 1.0),
                Point2::new(0.0, f64::MAX),
            ),
            Orientation2::CounterClockwise
        );
    }

    #[test]
    fn non_finite_inputs_are_indeterminate() {
        let finite = Point2::new(0.0, 0.0);
        for non_finite in [
            Point2::new(f64::NAN, 0.0),
            Point2::new(0.0, f64::INFINITY),
            Point2::new(f64::NEG_INFINITY, 0.0),
        ] {
            assert_eq!(
                orient2d(non_finite, non_finite, finite),
                Orientation2::Indeterminate
            );
            assert_eq!(
                orient2d(finite, non_finite, finite),
                Orientation2::Indeterminate
            );
        }
    }

    #[test]
    fn exact_integer_grid_signs_are_certified() {
        for ax in -2_i64..=2 {
            for ay in -2_i64..=2 {
                for bx in -2_i64..=2 {
                    for by in -2_i64..=2 {
                        for cx in -2_i64..=2 {
                            for cy in -2_i64..=2 {
                                let determinant = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
                                if determinant == 0 {
                                    continue;
                                }
                                let expected = if determinant > 0 {
                                    Orientation2::CounterClockwise
                                } else {
                                    Orientation2::Clockwise
                                };
                                assert_eq!(
                                    orient2d(
                                        Point2::new(ax as f64, ay as f64),
                                        Point2::new(bx as f64, by as f64),
                                        Point2::new(cx as f64, cy as f64),
                                    ),
                                    expected
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn one_million_certified_signs_match_a_large_exact_integer_oracle() {
        // Large exactly represented integer coordinates force rounded f64
        // products and substantial determinant cancellation. i128 remains an
        // independent exact oracle for this deliberately bounded corpus.
        let mut state = 0xA076_1D64_78BD_642F_u64;
        for case_index in 0..1_000_000_u64 {
            let mut next_integer = || {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                i128::from((state >> 13) as i64) - (1_i128 << 50)
            };

            let ax = next_integer();
            let ay = next_integer();
            let bx = next_integer();
            let by = next_integer();
            let (cx, cy) = if case_index % 2 == 0 {
                (next_integer(), next_integer())
            } else {
                // Near-collinear construction: the two large products mostly
                // cancel, with a one-unit perpendicular perturbation.
                let multiplier = i128::from((case_index % 7) + 2);
                let dx = (bx - ax) / 16;
                let dy = (by - ay) / 16;
                (
                    ax + multiplier * dx - dy.signum(),
                    ay + multiplier * dy + dx.signum(),
                )
            };

            for coordinate in [ax, ay, bx, by, cx, cy] {
                assert_eq!(coordinate, coordinate as f64 as i128);
            }
            let left = (bx - ax)
                .checked_mul(cy - ay)
                .expect("oracle left product must fit i128");
            let right = (by - ay)
                .checked_mul(cx - ax)
                .expect("oracle right product must fit i128");
            let exact = left
                .checked_sub(right)
                .expect("oracle determinant must fit i128");
            let result = orient2d(
                Point2::new(ax as f64, ay as f64),
                Point2::new(bx as f64, by as f64),
                Point2::new(cx as f64, cy as f64),
            );
            match result {
                Orientation2::CounterClockwise => assert!(exact > 0, "case {case_index}"),
                Orientation2::Clockwise => assert!(exact < 0, "case {case_index}"),
                Orientation2::Collinear => assert_eq!(exact, 0, "case {case_index}"),
                Orientation2::Indeterminate => panic!("finite case {case_index} was unresolved"),
            }
        }
    }

    #[test]
    fn repeated_evaluation_is_bitwise_deterministic() {
        let points = [
            Point2::new(0.25, -8.0),
            Point2::new(1.0 / 3.0, 11.0),
            Point2::new(7.25, 0.125),
        ];
        let expected = orient2d(points[0], points[1], points[2]);

        for _ in 0..10_000 {
            assert_eq!(orient2d(points[0], points[1], points[2]), expected);
        }
    }
}
