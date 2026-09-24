//! The primitives a Part 21 file is written with: numbered entities, the
//! spelling of reals and strings, and the fixed framing of header and
//! sections.

use std::fmt::Write as _;

/// A Part 21 file under construction: entities are numbered as they are
/// added, and [`Writer::finish`] wraps them in the header and sections.
#[derive(Clone, Debug, Default)]
pub struct Writer {
    data: String,
    next: u64,
}

impl Writer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: String::new(),
            next: 1,
        }
    }

    /// Appends one entity, `KIND(args)` or a complex `(A()B())`, and returns
    /// its number.
    pub fn entity(&mut self, body: &str) -> u64 {
        let id = self.next;
        self.next += 1;
        let _ = writeln!(self.data, "#{id}={body};");
        id
    }

    /// The number the next entity will get.
    #[must_use]
    pub const fn next_id(&self) -> u64 {
        self.next
    }

    /// The DATA section's text so far.
    #[must_use]
    pub fn data(&self) -> &str {
        &self.data
    }

    /// The whole file: `ISO-10303-21`, a header with the description lines,
    /// the file name and the schema, and the entities.
    #[must_use]
    pub fn finish(self, description: &[&str], name: &str, schema: &str) -> String {
        let mut file = String::with_capacity(self.data.len() + 512);
        file.push_str("ISO-10303-21;\nHEADER;\n");
        let description: Vec<String> = description.iter().map(|line| quoted(line)).collect();
        let _ = writeln!(
            file,
            "FILE_DESCRIPTION(({}),'2;1');",
            if description.is_empty() {
                "''".to_owned()
            } else {
                description.join(",")
            }
        );
        let _ = writeln!(
            file,
            "FILE_NAME({},'',('Artificer'),(''),'Artificer','Artificer','');",
            quoted(name)
        );
        let _ = writeln!(file, "FILE_SCHEMA(({}));", quoted(schema));
        file.push_str("ENDSEC;\nDATA;\n");
        file.push_str(&self.data);
        file.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
        file
    }
}

/// `#1,#2,#3`: a list of references without its parentheses.
#[must_use]
pub fn ids(ids: &[u64]) -> String {
    ids.iter()
        .map(|id| format!("#{id}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// A Part 21 string literal: apostrophes doubled, characters outside the
/// printable ASCII set replaced by an underscore.
#[must_use]
pub fn quoted(text: &str) -> String {
    let body: String = text
        .chars()
        .map(|character| {
            if character == '\'' {
                "''".to_owned()
            } else if character.is_ascii_graphic() || character == ' ' {
                character.to_string()
            } else {
                "_".to_owned()
            }
        })
        .collect();
    format!("'{body}'")
}

/// A Part 21 real: the shortest digits that read back to the same float,
/// always with a decimal point, and an uppercase exponent when there is one.
/// A non-finite value is written as zero rather than as text no reader
/// accepts.
#[must_use]
pub fn real(value: f64) -> String {
    if !value.is_finite() {
        return "0.".to_owned();
    }
    let text = format!("{value:?}");
    let (mantissa, exponent) = match text.split_once('e') {
        Some((mantissa, exponent)) => (mantissa.to_owned(), Some(exponent.to_owned())),
        None => (text, None),
    };
    let mantissa = if mantissa.contains('.') {
        mantissa
    } else {
        format!("{mantissa}.")
    };
    match exponent {
        Some(exponent) => format!("{mantissa}E{exponent}"),
        None => mantissa,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reals_carry_a_point_and_an_uppercase_exponent() {
        assert_eq!(real(1.0), "1.0");
        assert_eq!(real(-0.5), "-0.5");
        assert_eq!(real(1.0e-7), "1.E-7");
        assert_eq!(real(0.0), "0.0");
    }

    #[test]
    fn strings_double_apostrophes_and_drop_control_characters() {
        assert_eq!(quoted("it's"), "'it''s'");
        assert_eq!(quoted("a\tb"), "'a_b'");
    }
}
