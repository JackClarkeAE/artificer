//! ISO 10303-21 (STEP Part 21) exchange structures: a tokenizer, an entity
//! graph with typed argument access, unit handling, and the writing
//! primitives a Part 21 file is spelled with.
//!
//! This crate is deliberately dependency-free and knows nothing about
//! geometry. It reads a Part 21 file into a graph of entity instances keyed
//! by their `#n` numbers — simple instances and complex (multi-type)
//! instances alike — and answers questions such as "which entities are
//! `ADVANCED_FACE`s", "what is argument three of `#412`", and "what does one
//! length unit of this file measure in millimetres". What the entities mean
//! is the reader's business: the kernel's STEP import builds a B-rep from
//! this graph, the scan add-on's reader builds a mesh from it.
//!
//! The Part 21 features a reader meets in files from real systems are all
//! handled here: `/* */` comments, statements continued over any number of
//! lines, `$` (absent) and `*` (derived) arguments, typed parameters such as
//! `LENGTH_MEASURE(1.E-6)`, enumerations, doubled apostrophes in strings,
//! user-defined entities (`!NAME(...)`), several `DATA` sections, and the
//! edition-3 `ANCHOR`, `REFERENCE` and `SIGNATURE` sections, which are
//! skipped.

use std::collections::BTreeMap;
use std::fmt;

mod parser;
mod scanner;
mod units;
mod writer;

pub use units::Units;
pub use writer::{Writer, ids, quoted, real};

/// One parsed argument of an entity instance.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `#n`: a reference to another entity.
    Ref(u64),
    /// A real number, written with a decimal point or an exponent.
    Real(f64),
    /// An integer, written without either.
    Integer(i64),
    /// `'text'`, with doubled apostrophes already collapsed.
    Str(String),
    /// `.NAME.`, without its dots.
    Enum(String),
    /// A parenthesised aggregate.
    List(Vec<Value>),
    /// A typed parameter, `TYPE(value)`, such as `LENGTH_MEASURE(1.E-6)`.
    Typed(String, Box<Value>),
    /// `$`: the attribute is absent.
    Null,
    /// `*`: the attribute is derived by a subtype and not written.
    Derived,
}

impl Value {
    /// The referenced entity number.
    #[must_use]
    pub fn as_ref(&self) -> Option<u64> {
        match self {
            Self::Ref(id) => Some(*id),
            Self::Typed(_, inner) => Value::as_ref(inner),
            _ => None,
        }
    }

    /// The number, whether written as a real or an integer, looking through
    /// a typed parameter.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Real(value) => Some(*value),
            Self::Integer(value) => Some(*value as f64),
            Self::Typed(_, inner) => inner.as_f64(),
            _ => None,
        }
    }

    /// The integer, or a real that is a whole number.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            Self::Real(value) if value.fract() == 0.0 && value.abs() < 9.0e15 => {
                Some(*value as i64)
            }
            Self::Typed(_, inner) => inner.as_i64(),
            _ => None,
        }
    }

    /// A non-negative integer as a count.
    #[must_use]
    pub fn as_usize(&self) -> Option<usize> {
        self.as_i64().and_then(|value| usize::try_from(value).ok())
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(text) => Some(text),
            Self::Typed(_, inner) => inner.as_str(),
            _ => None,
        }
    }

    /// The enumeration item, without its dots.
    #[must_use]
    pub fn as_enum(&self) -> Option<&str> {
        match self {
            Self::Enum(name) => Some(name),
            Self::Typed(_, inner) => inner.as_enum(),
            _ => None,
        }
    }

    /// `.T.` and `.F.` as a boolean; `.U.` (unknown) and anything else as
    /// `None`.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self.as_enum()? {
            "T" => Some(true),
            "F" => Some(false),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Self::List(items) => Some(items),
            Self::Typed(_, inner) => inner.as_list(),
            _ => None,
        }
    }

    /// The list's entity references, in order, or `None` when the value is
    /// not a list or any item is not a reference.
    #[must_use]
    pub fn as_refs(&self) -> Option<Vec<u64>> {
        self.as_list()?.iter().map(Value::as_ref).collect()
    }

    /// The list's numbers, in order.
    #[must_use]
    pub fn as_f64s(&self) -> Option<Vec<f64>> {
        self.as_list()?.iter().map(Value::as_f64).collect()
    }

    /// Whether the attribute was written as `$` or `*`.
    #[must_use]
    pub fn is_absent(&self) -> bool {
        matches!(self, Self::Null | Self::Derived)
    }

    /// Every entity number this value refers to, at any depth.
    pub fn references(&self, out: &mut Vec<u64>) {
        match self {
            Self::Ref(id) => out.push(*id),
            Self::List(items) => items.iter().for_each(|item| item.references(out)),
            Self::Typed(_, inner) => inner.references(out),
            _ => {}
        }
    }
}

/// Stands in for an argument the file never wrote.
static MISSING_ARGUMENT: Value = Value::Null;

/// One typed part of an entity: `KIND(args)`. A simple entity has exactly
/// one; a complex entity such as `(BOUNDED_SURFACE()B_SPLINE_SURFACE(...)...)`
/// has one per supertype, in the file's order.
#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    pub kind: String,
    pub args: Vec<Value>,
}

impl Instance {
    /// Argument `index`, or `Null` when the instance carries fewer
    /// arguments than the caller expects, so a truncated entity reads as
    /// absent rather than taking the process down.
    #[must_use]
    pub fn arg(&self, index: usize) -> &Value {
        self.args.get(index).unwrap_or(&MISSING_ARGUMENT)
    }
}

/// One entity of the DATA section: `#id = ...`.
#[derive(Clone, Debug, PartialEq)]
pub struct Entity {
    pub id: u64,
    /// Whether the entity was written as a user-defined one (`!KIND`).
    pub user_defined: bool,
    pub instances: Vec<Instance>,
}

impl Entity {
    /// The kind of a simple entity, or the first listed kind of a complex
    /// one. [`Self::is`] answers "is this, among other things, a KIND".
    #[must_use]
    pub fn kind(&self) -> &str {
        self.instances.first().map_or("", |instance| &instance.kind)
    }

    /// Every kind the entity instantiates, in the file's order.
    pub fn kinds(&self) -> impl Iterator<Item = &str> + '_ {
        self.instances.iter().map(|instance| instance.kind.as_str())
    }

    #[must_use]
    pub fn is(&self, kind: &str) -> bool {
        self.instances.iter().any(|instance| instance.kind == kind)
    }

    #[must_use]
    pub fn is_complex(&self) -> bool {
        self.instances.len() > 1
    }

    /// The part of the entity of a given kind, for a complex entity.
    #[must_use]
    pub fn instance(&self, kind: &str) -> Option<&Instance> {
        self.instances.iter().find(|instance| instance.kind == kind)
    }

    /// Argument `index` of a simple entity (of its first instance for a
    /// complex one), or `Null` when it was not written.
    #[must_use]
    pub fn arg(&self, index: usize) -> &Value {
        self.instances
            .first()
            .map_or(&MISSING_ARGUMENT, |instance| instance.arg(index))
    }

    /// The arguments of a simple entity, empty for a complex one.
    #[must_use]
    pub fn args(&self) -> &[Value] {
        if self.instances.len() == 1 {
            &self.instances[0].args
        } else {
            &[]
        }
    }

    /// Every entity number this entity refers to, at any depth.
    #[must_use]
    pub fn references(&self) -> Vec<u64> {
        let mut out = Vec::new();
        for instance in &self.instances {
            for arg in &instance.args {
                arg.references(&mut out);
            }
        }
        out
    }
}

/// The HEADER section's three fixed entities, with their fields named.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Header {
    /// `FILE_DESCRIPTION`: the description lines and the implementation
    /// level (`2;1`).
    pub description: Vec<String>,
    pub implementation_level: String,
    /// `FILE_NAME`.
    pub name: String,
    pub time_stamp: String,
    pub author: Vec<String>,
    pub organization: Vec<String>,
    pub preprocessor_version: String,
    pub originating_system: String,
    pub authorization: String,
    /// `FILE_SCHEMA`: the schema identifiers, such as
    /// `AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }`.
    pub schema: Vec<String>,
    /// Every header entity as written, for the ones above and any others.
    pub entities: Vec<Instance>,
}

impl Header {
    /// The application protocol the schema names: `203`, `214`, `242`, or
    /// `None` when the schema does not say.
    #[must_use]
    pub fn application_protocol(&self) -> Option<u32> {
        let schema = self.schema.first()?;
        let upper = schema.to_ascii_uppercase();
        if upper.contains("10303 242") || upper.starts_with("AP242") {
            Some(242)
        } else if upper.contains("10303 214") || upper.starts_with("AUTOMOTIVE_DESIGN") {
            Some(214)
        } else if upper.contains("10303 203") || upper.starts_with("CONFIG_CONTROL_DESIGN") {
            Some(203)
        } else {
            None
        }
    }
}

/// The DATA section as an id-keyed entity graph.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph {
    entities: BTreeMap<u64, Entity>,
}

impl Graph {
    #[must_use]
    pub fn get(&self, id: u64) -> Option<&Entity> {
        self.entities.get(&id)
    }

    /// Whether `#id` exists.
    #[must_use]
    pub fn contains(&self, id: u64) -> bool {
        self.entities.contains_key(&id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Every entity, in ascending id order.
    pub fn entities(&self) -> impl Iterator<Item = &Entity> + '_ {
        self.entities.values()
    }

    /// Every entity that is, among other things, a `kind`, in ascending id
    /// order.
    pub fn of_kind<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Entity> + 'a {
        self.entities.values().filter(move |entity| entity.is(kind))
    }

    /// How many entities are, among other things, a `kind`.
    #[must_use]
    pub fn count(&self, kind: &str) -> usize {
        self.of_kind(kind).count()
    }

    /// The entity numbers referenced anywhere but defined nowhere.
    #[must_use]
    pub fn dangling_references(&self) -> Vec<u64> {
        let mut missing: Vec<u64> = self
            .entities
            .values()
            .flat_map(Entity::references)
            .filter(|id| !self.entities.contains_key(id))
            .collect();
        missing.sort_unstable();
        missing.dedup();
        missing
    }
}

/// A parsed Part 21 file.
#[derive(Clone, Debug, PartialEq)]
pub struct File {
    pub header: Header,
    pub graph: Graph,
    /// The length and angle units the file's geometry is written in, and
    /// its declared accuracy.
    pub units: Units,
}

/// Why a file could not be read as Part 21.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// The one-based line the error was found on.
    pub line: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Reads a Part 21 file. Every entity of every DATA section lands in the
/// graph; the units are read from the geometric context when there is one
/// and from the unit entities themselves otherwise.
pub fn parse(text: &str) -> Result<File, ParseError> {
    let (header, graph) = parser::parse(text)?;
    let units = units::read(&graph);
    Ok(File {
        header,
        graph,
        units,
    })
}

/// Reads bytes as a Part 21 file, tolerating bytes outside UTF-8 in strings.
pub fn parse_bytes(bytes: &[u8]) -> Result<File, ParseError> {
    parse(&String::from_utf8_lossy(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "ISO-10303-21;
HEADER;
/* a comment before the header entities */
FILE_DESCRIPTION(('Artificer exact B-rep export'),'2;1');
FILE_NAME('Artificer.step','2026-09-24T00:00:00',('Artificer'),(''),'Artificer','Artificer','');
FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));
ENDSEC;
DATA;
#1=APPLICATION_CONTEXT('automotive design');
#2=CARTESIAN_POINT('',(0.,0.,0.));
#3=DIRECTION('',(0.,0.,1.));
#4=AXIS2_PLACEMENT_3D('',#2,#3,$);
#5=(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.));
#6=(NAMED_UNIT(*)PLANE_ANGLE_UNIT()SI_UNIT($,.RADIAN.));
#7=UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-6),#5,
  'distance_accuracy_value','confusion accuracy');
#8=(GEOMETRIC_REPRESENTATION_CONTEXT(3)
GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#7))
GLOBAL_UNIT_ASSIGNED_CONTEXT((#5,#6))REPRESENTATION_CONTEXT('',''));
#9=B_SPLINE_CURVE_WITH_KNOTS('it''s',3,(#2,#2,#2,#2),.UNSPECIFIED.,.F.,.F.,(4,4),(0.,1.),.UNSPECIFIED.);
#10=!MY_ENTITY(42,'x');
ENDSEC;
END-ISO-10303-21;
";

    #[test]
    fn a_file_parses_into_header_graph_and_units() {
        let file = parse(SAMPLE).expect("the sample parses");
        assert_eq!(
            file.header.description,
            vec!["Artificer exact B-rep export"]
        );
        assert_eq!(file.header.implementation_level, "2;1");
        assert_eq!(file.header.name, "Artificer.step");
        assert_eq!(file.header.author, vec!["Artificer"]);
        assert_eq!(file.header.application_protocol(), Some(214));
        assert_eq!(file.graph.len(), 10);
        assert_eq!(file.graph.count("CARTESIAN_POINT"), 1);
        assert_eq!(file.units.length_to_mm, 1.0);
        assert_eq!(file.units.angle_to_radians, 1.0);
        assert_eq!(file.units.uncertainty_mm, Some(1.0e-6));
        assert!(file.graph.dangling_references().is_empty());
    }

    #[test]
    fn arguments_carry_their_types() {
        let file = parse(SAMPLE).unwrap();
        let placement = file.graph.get(4).unwrap();
        assert_eq!(placement.kind(), "AXIS2_PLACEMENT_3D");
        assert_eq!(placement.arg(0).as_str(), Some(""));
        assert_eq!(placement.arg(1).as_ref(), Some(2));
        assert!(placement.arg(3).is_absent());
        assert!(
            placement.arg(9).is_absent(),
            "a missing argument reads as absent"
        );
        let point = file.graph.get(2).unwrap();
        assert_eq!(point.arg(1).as_f64s(), Some(vec![0.0, 0.0, 0.0]));
        let spline = file.graph.get(9).unwrap();
        assert_eq!(spline.arg(0).as_str(), Some("it's"));
        assert_eq!(spline.arg(1).as_i64(), Some(3));
        assert_eq!(spline.arg(1).as_usize(), Some(3));
        assert_eq!(spline.arg(3).as_enum(), Some("UNSPECIFIED"));
        assert_eq!(spline.arg(4).as_bool(), Some(false));
        assert_eq!(spline.arg(2).as_refs(), Some(vec![2, 2, 2, 2]));
        assert_eq!(spline.arg(6).as_f64s(), Some(vec![4.0, 4.0]));
        let user = file.graph.get(10).unwrap();
        assert!(user.user_defined);
        assert_eq!(user.kind(), "MY_ENTITY");
        assert_eq!(user.arg(0).as_i64(), Some(42));
    }

    #[test]
    fn complex_instances_carry_every_supertype() {
        let file = parse(SAMPLE).unwrap();
        let unit = file.graph.get(5).unwrap();
        assert!(unit.is_complex());
        assert!(unit.is("LENGTH_UNIT"));
        assert!(unit.is("SI_UNIT"));
        assert_eq!(
            unit.kinds().collect::<Vec<_>>(),
            ["LENGTH_UNIT", "NAMED_UNIT", "SI_UNIT"]
        );
        let si = unit.instance("SI_UNIT").unwrap();
        assert_eq!(si.arg(0).as_enum(), Some("MILLI"));
        assert_eq!(si.arg(1).as_enum(), Some("METRE"));
        assert_eq!(unit.instance("NAMED_UNIT").unwrap().arg(0), &Value::Derived);
        // A complex entity's `args()` is empty: its arguments belong to its
        // parts, and reading them positionally would be a guess.
        assert!(unit.args().is_empty());
        let context = file.graph.get(8).unwrap();
        assert_eq!(
            context
                .instance("GLOBAL_UNIT_ASSIGNED_CONTEXT")
                .unwrap()
                .arg(0)
                .as_refs(),
            Some(vec![5, 6])
        );
        let uncertainty = file.graph.get(7).unwrap();
        assert_eq!(
            uncertainty.arg(0),
            &Value::Typed("LENGTH_MEASURE".to_owned(), Box::new(Value::Real(1.0e-6)))
        );
        assert_eq!(uncertainty.arg(0).as_f64(), Some(1.0e-6));
    }

    #[test]
    fn inch_and_degree_units_convert_to_millimetres_and_radians() {
        let text = "ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''),'2;1');
FILE_NAME('','',(''),(''),'','','');
FILE_SCHEMA(('AP242_MANAGED_MODEL_BASED_3D_ENGINEERING_MIM_LF { 1 0 10303 442 1 1 4 }'));
ENDSEC;
DATA;
#1=(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.));
#2=LENGTH_MEASURE_WITH_UNIT(LENGTH_MEASURE(25.4),#1);
#3=(CONVERSION_BASED_UNIT('INCH',#2)LENGTH_UNIT()NAMED_UNIT(#9));
#4=(NAMED_UNIT(*)PLANE_ANGLE_UNIT()SI_UNIT($,.RADIAN.));
#5=PLANE_ANGLE_MEASURE_WITH_UNIT(PLANE_ANGLE_MEASURE(0.0174532925199433),#4);
#6=(CONVERSION_BASED_UNIT('DEGREE',#5)NAMED_UNIT(#9)PLANE_ANGLE_UNIT());
#7=UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-5),#3,'distance_accuracy_value','');
#8=(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#7))GLOBAL_UNIT_ASSIGNED_CONTEXT((#3,#6))REPRESENTATION_CONTEXT('',''));
#9=DIMENSIONAL_EXPONENTS(1.,0.,0.,0.,0.,0.,0.);
ENDSEC;
END-ISO-10303-21;
";
        let file = parse(text).unwrap();
        assert_eq!(file.header.application_protocol(), Some(242));
        assert!((file.units.length_to_mm - 25.4).abs() < 1.0e-12);
        assert!((file.units.angle_to_radians - 0.0174532925199433).abs() < 1.0e-15);
        assert_eq!(file.units.length_unit_name, "INCH");
        assert!((file.units.uncertainty_mm.unwrap() - 25.4e-5).abs() < 1.0e-15);
    }

    #[test]
    fn metres_scale_by_a_thousand_and_missing_units_read_as_millimetres() {
        let metres = "ISO-10303-21;HEADER;ENDSEC;DATA;#1=(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT($,.METRE.));ENDSEC;END-ISO-10303-21;";
        assert_eq!(parse(metres).unwrap().units.length_to_mm, 1000.0);
        let none = "ISO-10303-21;HEADER;ENDSEC;DATA;#1=CARTESIAN_POINT('',(1.,2.,3.));ENDSEC;END-ISO-10303-21;";
        let file = parse(none).unwrap();
        assert_eq!(file.units.length_to_mm, 1.0);
        assert_eq!(file.units.uncertainty_mm, None);
    }

    #[test]
    fn several_data_sections_and_edition_three_sections_are_read() {
        let text = "ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''),'2;1');
ENDSEC;
ANCHOR;
<part>=#1;
ENDSEC;
REFERENCE;
#20=<http://example.com/other.step#part>;
ENDSEC;
DATA('first',('AUTOMOTIVE_DESIGN'));
#1=CARTESIAN_POINT('a',(1.,2.,3.));
ENDSEC;
DATA;
#2=CARTESIAN_POINT('b',(4.,5.,6.));
ENDSEC;
SIGNATURE;
abc
ENDSEC;
END-ISO-10303-21;
";
        let file = parse(text).unwrap();
        assert_eq!(file.graph.len(), 2);
        assert_eq!(file.graph.get(2).unwrap().arg(0).as_str(), Some("b"));
    }

    #[test]
    fn syntax_errors_name_their_line() {
        let text = "ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=CARTESIAN_POINT('',(1.,2.,3.);\nENDSEC;\nEND-ISO-10303-21;\n";
        let error = parse(text).unwrap_err();
        assert_eq!(error.line, 5, "{error}");
        let duplicate = "ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=A();\n#1=B();\nENDSEC;\nEND-ISO-10303-21;\n";
        let error = parse(duplicate).unwrap_err();
        assert!(error.message.contains("#1"), "{error}");
        assert_eq!(error.line, 6);
        let unterminated = "ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=A('open);\nENDSEC;\n";
        assert!(parse(unterminated).is_err());
    }

    #[test]
    fn nesting_past_the_ceiling_is_refused_rather_than_overflowing() {
        let deep = format!(
            "ISO-10303-21;HEADER;ENDSEC;DATA;#1=A({});ENDSEC;END-ISO-10303-21;",
            "(".repeat(10_000)
        );
        assert!(parse(&deep).is_err());
    }

    #[test]
    fn the_writer_spells_what_the_reader_reads() {
        let mut writer = Writer::new();
        let point = writer.entity(&format!(
            "CARTESIAN_POINT('',({},{},{}))",
            real(1.0),
            real(-0.5),
            real(1.0e-7)
        ));
        let direction = writer.entity("DIRECTION('',(0.,0.,1.))");
        let placement = writer.entity(&format!(
            "AXIS2_PLACEMENT_3D({},#{point},#{direction},$)",
            quoted("it's")
        ));
        let text = writer.finish(
            &["a test"],
            "test.step",
            "AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }",
        );
        let file = parse(&text).unwrap();
        assert_eq!(file.graph.len(), 3);
        assert_eq!(
            file.graph.get(placement).unwrap().arg(0).as_str(),
            Some("it's")
        );
        assert_eq!(
            file.graph.get(point).unwrap().arg(1).as_f64s(),
            Some(vec![1.0, -0.5, 1.0e-7])
        );
        assert_eq!(ids(&[1, 2, 3]), "#1,#2,#3");
        assert_eq!(real(12345678901234567890.0), "1.2345678901234567E19");
        assert_eq!(real(f64::NAN), "0.");
    }
}
