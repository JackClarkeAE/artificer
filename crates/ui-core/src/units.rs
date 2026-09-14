//! Length units for display and entry.
//!
//! The kernel works in millimetres and nothing here changes that. What
//! changes is what a person sees and types: a readout is formatted in the
//! unit they chose, and a typed value is read in that unit unless it carries
//! a suffix of its own (`10mm`, `0.5in`, `1e3um`, `2'`). Every conversion
//! goes through this one type so the workbench, the sketch canvas and the
//! feature editors cannot disagree about what a number means.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A unit of length a person reads and types in. Kernel geometry stays in
/// millimetres whatever this is set to.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum LengthUnit {
    Micrometre,
    #[default]
    Millimetre,
    Centimetre,
    Metre,
    Inch,
    Foot,
}

impl LengthUnit {
    pub const ALL: [Self; 6] = [
        Self::Micrometre,
        Self::Millimetre,
        Self::Centimetre,
        Self::Metre,
        Self::Inch,
        Self::Foot,
    ];

    /// The unit's name with its symbol, for a settings list.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Micrometre => "Micrometres (µm)",
            Self::Millimetre => "Millimetres (mm)",
            Self::Centimetre => "Centimetres (cm)",
            Self::Metre => "Metres (m)",
            Self::Inch => "Inches (in)",
            Self::Foot => "Feet (ft)",
        }
    }

    /// The symbol a readout carries after its number.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Micrometre => "µm",
            Self::Millimetre => "mm",
            Self::Centimetre => "cm",
            Self::Metre => "m",
            Self::Inch => "in",
            Self::Foot => "ft",
        }
    }

    /// How many millimetres one of this unit is. Inch and foot are exact by
    /// definition (25.4 mm to the inch since 1959).
    #[must_use]
    pub const fn millimetres_per_unit(self) -> f64 {
        match self {
            Self::Micrometre => 0.001,
            Self::Millimetre => 1.0,
            Self::Centimetre => 10.0,
            Self::Metre => 1000.0,
            Self::Inch => 25.4,
            Self::Foot => 304.8,
        }
    }

    /// How many decimals a readout in this unit shows: about a micrometre of
    /// resolution whatever the unit, which is finer than any tolerance the
    /// kernel certifies at display.
    #[must_use]
    pub const fn decimals(self) -> usize {
        match self {
            Self::Micrometre => 1,
            Self::Millimetre => 3,
            Self::Centimetre => 4,
            Self::Metre => 6,
            Self::Inch => 4,
            Self::Foot => 5,
        }
    }

    /// A value in this unit, as millimetres.
    #[must_use]
    pub fn to_millimetres(self, value: f64) -> f64 {
        value * self.millimetres_per_unit()
    }

    /// Millimetres, as a value in this unit.
    #[must_use]
    pub fn from_millimetres(self, millimetres: f64) -> f64 {
        millimetres / self.millimetres_per_unit()
    }

    /// The number alone, in this unit, at the unit's precision with trailing
    /// zeros trimmed: `12.5`, not `12.500`.
    #[must_use]
    pub fn format_value(self, millimetres: f64) -> String {
        if !millimetres.is_finite() {
            return "—".to_owned();
        }
        let value = self.from_millimetres(millimetres);
        let text = format!("{value:.*}", self.decimals());
        trim_trailing_zeros(&text)
    }

    /// The number with the unit's symbol: `12.5 mm`, `0.4921 in`.
    #[must_use]
    pub fn format(self, millimetres: f64) -> String {
        format!("{} {}", self.format_value(millimetres), self.suffix())
    }

    /// Reads a typed length and returns millimetres.
    ///
    /// The text is a number, in plain or exponent form (`12.5`, `1e-3`,
    /// `2.5E+2`), optionally followed by a unit symbol. Without a symbol the
    /// number is in this unit; with one it is in that unit, so `10mm` means
    /// the same in an inch document as in a millimetre one. Accepted symbols
    /// are `µm`, `um`, `mm`, `cm`, `m`, `in`, `"`, `ft` and `'`, with or
    /// without a space before them. Arithmetic is not read here; a field
    /// that takes expressions evaluates them first and hands the number on.
    pub fn parse(self, text: &str) -> Result<f64, LengthParseError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(LengthParseError::Empty);
        }
        let split = text
            .char_indices()
            .find(|(_, character)| {
                !(character.is_ascii_digit()
                    || matches!(character, '.' | '+' | '-' | 'e' | 'E'))
            })
            .map_or(text.len(), |(index, _)| index);
        // `e` starts a suffix only when no digit follows it, so `1e3` is a
        // number and `1e` is not; the number parser decides.
        let (number_text, suffix) = split_number(text, split);
        let value = number_text
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .ok_or_else(|| LengthParseError::NotANumber(number_text.to_owned()))?;
        let unit = match suffix.trim() {
            "" => self,
            symbol => Self::from_symbol(symbol)
                .ok_or_else(|| LengthParseError::UnknownUnit(symbol.to_owned()))?,
        };
        Ok(unit.to_millimetres(value))
    }

    /// The unit a typed symbol names, or `None` for a symbol nobody uses.
    #[must_use]
    pub fn from_symbol(symbol: &str) -> Option<Self> {
        Some(match symbol.trim() {
            "µm" | "um" | "micrometre" | "micrometres" | "micron" | "microns" => Self::Micrometre,
            "mm" | "millimetre" | "millimetres" | "millimeter" | "millimeters" => Self::Millimetre,
            "cm" | "centimetre" | "centimetres" | "centimeter" | "centimeters" => Self::Centimetre,
            "m" | "metre" | "metres" | "meter" | "meters" => Self::Metre,
            "in" | "\"" | "inch" | "inches" => Self::Inch,
            "ft" | "'" | "foot" | "feet" => Self::Foot,
            _ => return None,
        })
    }
}

impl fmt::Display for LengthUnit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.suffix())
    }
}

/// Splits typed text into its number and its unit symbol. `split` is the
/// first index that cannot belong to a number; when the number ends in a
/// bare `e` (as `1e` or the `e` of `1em` would), that letter goes back to
/// the suffix.
fn split_number(text: &str, split: usize) -> (&str, &str) {
    let (mut number, mut suffix) = text.split_at(split);
    while let Some(stripped) = number.strip_suffix(['e', 'E', '+', '-']) {
        if stripped.parse::<f64>().is_ok() || stripped.is_empty() {
            number = stripped;
            suffix = &text[number.len()..];
        } else {
            break;
        }
    }
    (number, suffix)
}

fn trim_trailing_zeros(text: &str) -> String {
    if !text.contains('.') {
        return text.to_owned();
    }
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if matches!(trimmed, "" | "-" | "-0") {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Why a typed length could not be read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LengthParseError {
    Empty,
    NotANumber(String),
    UnknownUnit(String),
}

impl fmt::Display for LengthParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("Type a length"),
            Self::NotANumber(text) => write!(formatter, "`{text}` is not a number"),
            Self::UnknownUnit(symbol) => write!(
                formatter,
                "`{symbol}` is not a length unit; use mm, cm, m, in, ft or µm"
            ),
        }
    }
}

impl std::error::Error for LengthParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_unit_round_trips_through_millimetres() {
        for unit in LengthUnit::ALL {
            let back = unit.from_millimetres(unit.to_millimetres(3.25));
            assert!((back - 3.25).abs() < 1e-12, "{unit:?}");
        }
        assert_eq!(LengthUnit::Inch.to_millimetres(1.0), 25.4);
        assert_eq!(LengthUnit::Foot.to_millimetres(1.0), 304.8);
    }

    #[test]
    fn readouts_carry_the_symbol_and_trim_zeros() {
        assert_eq!(LengthUnit::Millimetre.format(12.5), "12.5 mm");
        assert_eq!(LengthUnit::Millimetre.format(12.0), "12 mm");
        assert_eq!(LengthUnit::Inch.format(25.4), "1 in");
        assert_eq!(LengthUnit::Inch.format_value(12.7), "0.5");
        assert_eq!(LengthUnit::Centimetre.format(12.5), "1.25 cm");
        assert_eq!(LengthUnit::Metre.format(1234.5), "1.2345 m");
        assert_eq!(LengthUnit::Micrometre.format(0.25), "250 µm");
        assert_eq!(LengthUnit::Millimetre.format(-0.0), "0 mm");
        assert_eq!(LengthUnit::Millimetre.format(f64::NAN), "— mm");
    }

    #[test]
    fn a_bare_number_is_read_in_the_field_unit() {
        assert_eq!(LengthUnit::Millimetre.parse("12.5"), Ok(12.5));
        assert_eq!(LengthUnit::Inch.parse("1"), Ok(25.4));
        assert_eq!(LengthUnit::Inch.parse(" 0.5 "), Ok(12.7));
        assert_eq!(LengthUnit::Metre.parse("-0.25"), Ok(-250.0));
    }

    #[test]
    fn a_suffix_names_its_own_unit() {
        assert_eq!(LengthUnit::Inch.parse("10mm"), Ok(10.0));
        assert_eq!(LengthUnit::Inch.parse("10 mm"), Ok(10.0));
        assert_eq!(LengthUnit::Millimetre.parse("1in"), Ok(25.4));
        assert_eq!(LengthUnit::Millimetre.parse("2\""), Ok(50.8));
        assert_eq!(LengthUnit::Millimetre.parse("1'"), Ok(304.8));
        assert_eq!(LengthUnit::Millimetre.parse("2.5cm"), Ok(25.0));
        assert_eq!(LengthUnit::Millimetre.parse("1.5 m"), Ok(1500.0));
        assert_eq!(LengthUnit::Millimetre.parse("250um"), Ok(0.25));
        assert_eq!(LengthUnit::Millimetre.parse("250µm"), Ok(0.25));
    }

    #[test]
    fn exponent_notation_is_a_number_not_a_unit() {
        assert_eq!(LengthUnit::Millimetre.parse("1e3"), Ok(1000.0));
        assert_eq!(LengthUnit::Millimetre.parse("1E-3"), Ok(0.001));
        assert_eq!(LengthUnit::Millimetre.parse("2.5e+1mm"), Ok(25.0));
        assert_eq!(LengthUnit::Inch.parse("1e3um"), Ok(1.0));
        assert_eq!(LengthUnit::Millimetre.parse("1e3 in"), Ok(25_400.0));
    }

    #[test]
    fn bad_input_says_what_is_wrong() {
        assert_eq!(LengthUnit::Millimetre.parse(""), Err(LengthParseError::Empty));
        assert_eq!(
            LengthUnit::Millimetre.parse("abc"),
            Err(LengthParseError::NotANumber(String::new()))
        );
        assert_eq!(
            LengthUnit::Millimetre.parse("12 furlongs"),
            Err(LengthParseError::UnknownUnit("furlongs".to_owned()))
        );
        assert_eq!(
            LengthUnit::Millimetre.parse("1e"),
            Err(LengthParseError::UnknownUnit("e".to_owned()))
        );
        assert!(LengthUnit::Millimetre.parse("1e999").is_err());
    }
}
