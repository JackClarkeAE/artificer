//! The one textual grammar every numeric entry reads.
//!
//! A document variable's value, a sketch dimension, a recipe field and an
//! extrusion distance are all typed as the same kind of text: numbers, units,
//! variable names, `+ - * /`, parentheses and unary minus — `width / 2 + 5mm`,
//! `30deg`, `depth * 2`. They used to be read by two readers, one that knew
//! units and one that did not, so an entry the Variables panel took could be
//! refused by a dimension box beside it. This module is the reader both sides
//! now share: the document model lowers what it parses into its stored
//! expression tree, and the sketch evaluates it on the spot.
//!
//! Parsing leaves names as text and bare numbers bare. What a bare number
//! means depends on where it stands, and [`Expression::assign_roles`] decides
//! it the way people write dimensions: an additive term wears the field's own
//! unit (`width + 5` adds five of whatever the field is in), while a bare
//! factor beside something dimensioned is a pure number (`width * 2` doubles
//! it, it does not multiply two lengths).

use std::fmt;

/// A unit a number may carry, written directly after it (`5mm`) or after a
/// space (`5 mm`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpressionUnit {
    Micrometer,
    Millimeter,
    Centimeter,
    Meter,
    Inch,
    Foot,
    Degree,
    Radian,
}

/// The suffixes the grammar accepts after a number.
pub const EXPRESSION_UNIT_SUFFIXES: [(&str, ExpressionUnit); 9] = [
    ("um", ExpressionUnit::Micrometer),
    ("µm", ExpressionUnit::Micrometer),
    ("mm", ExpressionUnit::Millimeter),
    ("cm", ExpressionUnit::Centimeter),
    ("m", ExpressionUnit::Meter),
    ("in", ExpressionUnit::Inch),
    ("ft", ExpressionUnit::Foot),
    ("deg", ExpressionUnit::Degree),
    ("rad", ExpressionUnit::Radian),
];

impl ExpressionUnit {
    /// What the unit measures.
    #[must_use]
    pub const fn dimension(self) -> Dimension {
        match self {
            Self::Degree | Self::Radian => Dimension::ANGLE,
            _ => Dimension::LENGTH,
        }
    }

    /// How many canonical units — millimetres, or radians — one of this is.
    #[must_use]
    pub fn canonical_scale(self) -> f64 {
        match self {
            Self::Micrometer => 1.0e-3,
            Self::Millimeter => 1.0,
            Self::Centimeter => 10.0,
            Self::Meter => 1_000.0,
            Self::Inch => 25.4,
            Self::Foot => 304.8,
            Self::Degree => std::f64::consts::PI / 180.0,
            Self::Radian => 1.0,
        }
    }
}

/// The powers of length and angle a value carries: a length is `(1, 0)`, an
/// area `(2, 0)`, an angle `(0, 1)`, a pure number `(0, 0)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dimension {
    pub length: i8,
    pub angle: i8,
}

impl Dimension {
    pub const SCALAR: Self = Self {
        length: 0,
        angle: 0,
    };
    pub const LENGTH: Self = Self {
        length: 1,
        angle: 0,
    };
    pub const ANGLE: Self = Self {
        length: 0,
        angle: 1,
    };

    const fn times(self, other: Self) -> Self {
        Self {
            length: self.length.saturating_add(other.length),
            angle: self.angle.saturating_add(other.angle),
        }
    }

    const fn over(self, other: Self) -> Self {
        Self {
            length: self.length.saturating_sub(other.length),
            angle: self.angle.saturating_sub(other.angle),
        }
    }
}

/// Why an entry could not be read or evaluated. Every message names what it
/// found, so the field can say it beside the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpressionError {
    Empty,
    InvalidNumber(String),
    UnknownUnit(String),
    UnknownName(String),
    UnexpectedCharacter(char),
    UnexpectedEnd,
    TrailingInput(String),
    /// Terms that cannot be added, or a result that is not what the field
    /// holds: a length added to an angle, an area typed as a length.
    MismatchedUnits,
    /// A result that is not a finite number, such as a division by zero.
    NotFinite,
}

impl fmt::Display for ExpressionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(formatter, "enter a value or expression"),
            Self::InvalidNumber(text) => write!(formatter, "{text:?} is not a number"),
            Self::UnknownUnit(text) => write!(formatter, "{text:?} is not a known unit"),
            Self::UnknownName(text) => write!(formatter, "{text:?} is not a defined variable"),
            Self::UnexpectedCharacter(character) => write!(formatter, "unexpected {character:?}"),
            Self::UnexpectedEnd => write!(formatter, "the expression ends too early"),
            Self::TrailingInput(text) => write!(formatter, "unexpected trailing input {text:?}"),
            Self::MismatchedUnits => write!(formatter, "the units do not agree"),
            Self::NotFinite => write!(formatter, "the result is not a finite number"),
        }
    }
}

impl std::error::Error for ExpressionError {}

/// A parsed entry. Names are unresolved text and bare numbers are bare.
#[derive(Clone, Debug, PartialEq)]
pub enum Expression {
    Number {
        magnitude: f64,
        unit: Option<ExpressionUnit>,
    },
    Name(String),
    Negate(Box<Expression>),
    Add(Box<Expression>, Box<Expression>),
    Subtract(Box<Expression>, Box<Expression>),
    Multiply(Box<Expression>, Box<Expression>),
    Divide(Box<Expression>, Box<Expression>),
}

/// What a bare number stands for once its place in the entry is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberUnit {
    /// The field's own unit: five in a length field is five of its unit.
    Field,
    /// A pure number: the two in `width * 2`.
    Scalar,
    /// The unit it was written with.
    Explicit(ExpressionUnit),
}

/// An entry whose every number knows its unit.
#[derive(Clone, Debug, PartialEq)]
pub enum TypedExpression {
    Number { magnitude: f64, unit: NumberUnit },
    Name(String),
    Negate(Box<TypedExpression>),
    Add(Box<TypedExpression>, Box<TypedExpression>),
    Subtract(Box<TypedExpression>, Box<TypedExpression>),
    Multiply(Box<TypedExpression>, Box<TypedExpression>),
    Divide(Box<TypedExpression>, Box<TypedExpression>),
}

/// A named value an entry may use: a document variable, canonical — lengths
/// in millimetres, angles in radians — with what it measures.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NamedQuantity {
    pub canonical: f64,
    pub dimension: Dimension,
}

/// What the field an entry is typed into holds: its dimension, and how many
/// canonical units one of its own units is (a length field in inches is
/// 25.4; an angle field in degrees is π/180).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldUnit {
    pub dimension: Dimension,
    pub canonical_scale: f64,
}

impl FieldUnit {
    /// A pure-number field, such as a count or a factor.
    pub const SCALAR: Self = Self {
        dimension: Dimension::SCALAR,
        canonical_scale: 1.0,
    };

    /// A length field in a unit of `millimetres_per_unit` millimetres.
    #[must_use]
    pub const fn length(millimetres_per_unit: f64) -> Self {
        Self {
            dimension: Dimension::LENGTH,
            canonical_scale: millimetres_per_unit,
        }
    }

    /// An angle field in degrees.
    #[must_use]
    pub fn degrees() -> Self {
        Self {
            dimension: Dimension::ANGLE,
            canonical_scale: std::f64::consts::PI / 180.0,
        }
    }
}

impl Expression {
    /// Whether this subtree carries a dimension of its own — a name, or a
    /// number written with a unit — as opposed to bare numerals.
    #[must_use]
    pub fn is_dimensioned(&self) -> bool {
        match self {
            Self::Number { unit, .. } => unit.is_some(),
            Self::Name(_) => true,
            Self::Negate(operand) => operand.is_dimensioned(),
            Self::Add(left, right)
            | Self::Subtract(left, right)
            | Self::Multiply(left, right)
            | Self::Divide(left, right) => left.is_dimensioned() || right.is_dimensioned(),
        }
    }

    /// Gives every bare number its unit by where it stands.
    ///
    /// Additive terms wear the field's unit. In a product, a bare factor
    /// beside a dimensioned one is a pure number; a bare divisor is a pure
    /// number too (`width / 2` halves). Two bare factors keep the product in
    /// the field's unit by reading the left one in it.
    #[must_use]
    pub fn assign_roles(self) -> TypedExpression {
        self.assign(NumberUnit::Field)
    }

    fn assign(self, role: NumberUnit) -> TypedExpression {
        match self {
            Self::Number { magnitude, unit } => TypedExpression::Number {
                magnitude,
                unit: unit.map_or(role, NumberUnit::Explicit),
            },
            Self::Name(name) => TypedExpression::Name(name),
            Self::Negate(operand) => TypedExpression::Negate(Box::new(operand.assign(role))),
            Self::Add(left, right) => TypedExpression::Add(
                Box::new(left.assign(role)),
                Box::new(right.assign(role)),
            ),
            Self::Subtract(left, right) => TypedExpression::Subtract(
                Box::new(left.assign(role)),
                Box::new(right.assign(role)),
            ),
            Self::Multiply(left, right) => {
                let (left_role, right_role) = match (left.is_dimensioned(), right.is_dimensioned())
                {
                    (false, true) => (NumberUnit::Scalar, role),
                    (true, false) | (true, true) | (false, false) => (role, NumberUnit::Scalar),
                };
                TypedExpression::Multiply(
                    Box::new(left.assign(left_role)),
                    Box::new(right.assign(right_role)),
                )
            }
            Self::Divide(left, right) => {
                let denominator = if right.is_dimensioned() {
                    role
                } else {
                    NumberUnit::Scalar
                };
                TypedExpression::Divide(
                    Box::new(left.assign(role)),
                    Box::new(right.assign(denominator)),
                )
            }
        }
    }
}

impl TypedExpression {
    /// Whether the whole entry is one number, signed or not: a literal
    /// rather than an expression.
    #[must_use]
    pub fn as_literal(&self) -> Option<(f64, NumberUnit)> {
        match self {
            Self::Number { magnitude, unit } => Some((*magnitude, *unit)),
            Self::Negate(operand) => match operand.as_ref() {
                Self::Number { magnitude, unit } => Some((-*magnitude, *unit)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Evaluates the entry for a field, to a canonical magnitude — millimetres
    /// for a length, radians for an angle — checking that terms added
    /// together agree and that the result is what the field holds.
    pub fn evaluate(
        &self,
        field: FieldUnit,
        resolve: &dyn Fn(&str) -> Option<NamedQuantity>,
    ) -> Result<f64, ExpressionError> {
        let (value, dimension) = self.value(field, resolve)?;
        if dimension != field.dimension {
            return Err(ExpressionError::MismatchedUnits);
        }
        if value.is_finite() {
            Ok(value)
        } else {
            Err(ExpressionError::NotFinite)
        }
    }

    fn value(
        &self,
        field: FieldUnit,
        resolve: &dyn Fn(&str) -> Option<NamedQuantity>,
    ) -> Result<(f64, Dimension), ExpressionError> {
        Ok(match self {
            Self::Number { magnitude, unit } => match unit {
                NumberUnit::Field => (magnitude * field.canonical_scale, field.dimension),
                NumberUnit::Scalar => (*magnitude, Dimension::SCALAR),
                NumberUnit::Explicit(unit) => {
                    (magnitude * unit.canonical_scale(), unit.dimension())
                }
            },
            Self::Name(name) => {
                let named = resolve(name).ok_or_else(|| ExpressionError::UnknownName(name.clone()))?;
                (named.canonical, named.dimension)
            }
            Self::Negate(operand) => {
                let (value, dimension) = operand.value(field, resolve)?;
                (-value, dimension)
            }
            Self::Add(left, right) | Self::Subtract(left, right) => {
                let (left_value, left_dimension) = left.value(field, resolve)?;
                let (right_value, right_dimension) = right.value(field, resolve)?;
                if left_dimension != right_dimension {
                    return Err(ExpressionError::MismatchedUnits);
                }
                let value = if matches!(self, Self::Add(..)) {
                    left_value + right_value
                } else {
                    left_value - right_value
                };
                (value, left_dimension)
            }
            Self::Multiply(left, right) => {
                let (left_value, left_dimension) = left.value(field, resolve)?;
                let (right_value, right_dimension) = right.value(field, resolve)?;
                (
                    left_value * right_value,
                    left_dimension.times(right_dimension),
                )
            }
            Self::Divide(left, right) => {
                let (left_value, left_dimension) = left.value(field, resolve)?;
                let (right_value, right_dimension) = right.value(field, resolve)?;
                (
                    left_value / right_value,
                    left_dimension.over(right_dimension),
                )
            }
        })
    }
}

/// Parses one entry and evaluates it for a field, in one step.
pub fn evaluate_entry(
    text: &str,
    field: FieldUnit,
    resolve: &dyn Fn(&str) -> Option<NamedQuantity>,
) -> Result<f64, ExpressionError> {
    parse_expression(text)?.assign_roles().evaluate(field, resolve)
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64, Option<ExpressionUnit>),
    Name(String),
    Plus,
    Minus,
    Star,
    Slash,
    Open,
    Close,
}

fn unit_for_suffix(suffix: &str) -> Option<ExpressionUnit> {
    EXPRESSION_UNIT_SUFFIXES
        .iter()
        .find(|(name, _)| *name == suffix)
        .map(|(_, unit)| *unit)
}

fn tokenize(text: &str) -> Result<Vec<Token>, ExpressionError> {
    let mut tokens = Vec::new();
    let mut characters = text.chars().peekable();
    while let Some(&character) = characters.peek() {
        match character {
            ' ' | '\t' => {
                characters.next();
            }
            '+' => {
                characters.next();
                tokens.push(Token::Plus);
            }
            '-' | '−' => {
                characters.next();
                tokens.push(Token::Minus);
            }
            '*' | '×' => {
                characters.next();
                tokens.push(Token::Star);
            }
            '/' | '÷' => {
                characters.next();
                tokens.push(Token::Slash);
            }
            '(' => {
                characters.next();
                tokens.push(Token::Open);
            }
            ')' => {
                characters.next();
                tokens.push(Token::Close);
            }
            '0'..='9' | '.' => {
                let mut digits = String::new();
                while let Some(&digit) = characters.peek() {
                    if digit.is_ascii_digit() || digit == '.' {
                        digits.push(digit);
                        characters.next();
                    } else {
                        break;
                    }
                }
                // An exponent: `1e3`, `2.5E-2`. The `e` belongs to the
                // number only when a digit follows it, directly or after
                // one sign; otherwise it starts a name.
                if let Some(&marker) = characters.peek()
                    && matches!(marker, 'e' | 'E')
                {
                    let mut ahead = characters.clone();
                    ahead.next();
                    let sign = ahead.next_if(|piece| *piece == '+' || *piece == '-');
                    if ahead.peek().is_some_and(char::is_ascii_digit) {
                        digits.push(marker);
                        characters.next();
                        if let Some(sign) = sign {
                            digits.push(sign);
                            characters.next();
                        }
                        while let Some(&piece) = characters.peek() {
                            if piece.is_ascii_digit() {
                                digits.push(piece);
                                characters.next();
                            } else {
                                break;
                            }
                        }
                    }
                }
                let magnitude = digits
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| ExpressionError::InvalidNumber(digits.clone()))?;
                // An identifier directly after a number is its unit —
                // juxtaposition is never multiplication in this grammar.
                let mut suffix = String::new();
                while let Some(&letter) = characters.peek() {
                    if letter.is_alphabetic() || letter == 'µ' {
                        suffix.push(letter);
                        characters.next();
                    } else {
                        break;
                    }
                }
                let unit = if suffix.is_empty() {
                    None
                } else {
                    Some(unit_for_suffix(&suffix).ok_or(ExpressionError::UnknownUnit(suffix))?)
                };
                tokens.push(Token::Number(magnitude, unit));
            }
            letter if letter.is_alphabetic() || letter == '_' => {
                let mut name = String::new();
                while let Some(&piece) = characters.peek() {
                    if piece.is_alphanumeric() || piece == '_' {
                        name.push(piece);
                        characters.next();
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Name(name));
            }
            other => return Err(ExpressionError::UnexpectedCharacter(other)),
        }
    }
    // `5 mm` is the same entry as `5mm`: a unit name directly after a bare
    // number binds to it across the whitespace the display form prints.
    let mut merged: Vec<Token> = Vec::with_capacity(tokens.len());
    for token in tokens {
        if let (Token::Name(name), Some(Token::Number(magnitude, None))) = (&token, merged.last())
            && let Some(unit) = unit_for_suffix(name)
        {
            let magnitude = *magnitude;
            merged.pop();
            merged.push(Token::Number(magnitude, Some(unit)));
            continue;
        }
        merged.push(token);
    }
    Ok(merged)
}

struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.cursor)
    }

    fn advance(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.cursor).cloned();
        if token.is_some() {
            self.cursor += 1;
        }
        token
    }

    fn expression(&mut self) -> Result<Expression, ExpressionError> {
        let mut left = self.term()?;
        loop {
            let add = match self.peek() {
                Some(Token::Plus) => true,
                Some(Token::Minus) => false,
                _ => break,
            };
            self.cursor += 1;
            let right = self.term()?;
            left = if add {
                Expression::Add(Box::new(left), Box::new(right))
            } else {
                Expression::Subtract(Box::new(left), Box::new(right))
            };
        }
        Ok(left)
    }

    fn term(&mut self) -> Result<Expression, ExpressionError> {
        let mut left = self.factor()?;
        while let Some(token) = self.peek() {
            let multiply = match token {
                Token::Star => true,
                Token::Slash => false,
                _ => break,
            };
            self.cursor += 1;
            let right = self.factor()?;
            left = if multiply {
                Expression::Multiply(Box::new(left), Box::new(right))
            } else {
                Expression::Divide(Box::new(left), Box::new(right))
            };
        }
        Ok(left)
    }

    fn factor(&mut self) -> Result<Expression, ExpressionError> {
        match self.advance().ok_or(ExpressionError::UnexpectedEnd)? {
            Token::Minus => Ok(Expression::Negate(Box::new(self.factor()?))),
            Token::Open => {
                let inner = self.expression()?;
                match self.advance() {
                    Some(Token::Close) => Ok(inner),
                    _ => Err(ExpressionError::UnexpectedEnd),
                }
            }
            Token::Number(magnitude, unit) => Ok(Expression::Number { magnitude, unit }),
            Token::Name(name) => Ok(Expression::Name(name)),
            Token::Plus => self.factor(),
            other => Err(ExpressionError::TrailingInput(format!("{other:?}"))),
        }
    }
}

/// Parses one entry: numbers, units, names, `+ - * /`, parentheses and
/// unary minus.
pub fn parse_expression(text: &str) -> Result<Expression, ExpressionError> {
    let tokens = tokenize(text)?;
    if tokens.is_empty() {
        return Err(ExpressionError::Empty);
    }
    let mut parser = Parser { tokens, cursor: 0 };
    let expression = parser.expression()?;
    if parser.cursor != parser.tokens.len() {
        return Err(ExpressionError::TrailingInput(format!(
            "{:?}",
            parser.tokens[parser.cursor]
        )));
    }
    Ok(expression)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variables(name: &str) -> Option<NamedQuantity> {
        match name {
            "width" => Some(NamedQuantity {
                canonical: 40.0,
                dimension: Dimension::LENGTH,
            }),
            "tilt" => Some(NamedQuantity {
                canonical: 45.0_f64.to_radians(),
                dimension: Dimension::ANGLE,
            }),
            "count" => Some(NamedQuantity {
                canonical: 3.0,
                dimension: Dimension::SCALAR,
            }),
            _ => None,
        }
    }

    fn close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1.0e-12,
            "{actual} is not {expected}"
        );
    }

    #[test]
    fn additive_numbers_wear_the_field_unit_and_factors_are_pure() {
        let inches = FieldUnit::length(25.4);
        // `width + 1` adds one inch to 40 mm.
        close(
            evaluate_entry("width + 1", inches, &variables).unwrap(),
            40.0 + 25.4,
        );
        // `width * 2` doubles, `width / 2` halves.
        close(evaluate_entry("width * 2", inches, &variables).unwrap(), 80.0);
        close(evaluate_entry("width / 2", inches, &variables).unwrap(), 20.0);
        // A written unit always wins.
        close(
            evaluate_entry("width + 5mm", inches, &variables).unwrap(),
            45.0,
        );
        close(
            evaluate_entry("width + 5 mm", inches, &variables).unwrap(),
            45.0,
        );
        close(evaluate_entry("2 * 3", inches, &variables).unwrap(), 6.0 * 25.4);
    }

    #[test]
    fn angles_are_read_and_returned_as_angles() {
        let degrees = FieldUnit::degrees();
        close(
            evaluate_entry("tilt", degrees, &variables).unwrap(),
            45.0_f64.to_radians(),
        );
        close(
            evaluate_entry("tilt / 3 + 15", degrees, &variables).unwrap(),
            30.0_f64.to_radians(),
        );
        close(
            evaluate_entry("0.5rad", degrees, &variables).unwrap(),
            0.5,
        );
    }

    #[test]
    fn units_that_do_not_agree_are_refused() {
        assert_eq!(
            evaluate_entry("width + tilt", FieldUnit::length(1.0), &variables),
            Err(ExpressionError::MismatchedUnits)
        );
        assert_eq!(
            evaluate_entry("width * width", FieldUnit::length(1.0), &variables),
            Err(ExpressionError::MismatchedUnits),
            "an area is not a length"
        );
        assert_eq!(
            evaluate_entry("30deg", FieldUnit::length(1.0), &variables),
            Err(ExpressionError::MismatchedUnits)
        );
        close(
            evaluate_entry("width / 10mm", FieldUnit::SCALAR, &variables).unwrap(),
            4.0,
        );
        close(
            evaluate_entry("count * 2", FieldUnit::SCALAR, &variables).unwrap(),
            6.0,
        );
    }

    #[test]
    fn what_cannot_be_read_says_why() {
        let field = FieldUnit::length(1.0);
        assert_eq!(
            evaluate_entry("", field, &variables),
            Err(ExpressionError::Empty)
        );
        assert_eq!(
            evaluate_entry("height", field, &variables),
            Err(ExpressionError::UnknownName("height".into()))
        );
        assert_eq!(
            evaluate_entry("5furlong", field, &variables),
            Err(ExpressionError::UnknownUnit("furlong".into()))
        );
        assert_eq!(
            evaluate_entry("(width", field, &variables),
            Err(ExpressionError::UnexpectedEnd)
        );
        assert_eq!(
            evaluate_entry("width / 0", field, &variables),
            Err(ExpressionError::NotFinite)
        );
        close(evaluate_entry("1e3", field, &variables).unwrap(), 1_000.0);
        close(
            evaluate_entry("−width × 2 ÷ 4", field, &variables).unwrap(),
            -20.0,
        );
    }
}
