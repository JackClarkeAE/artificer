//! The units a file's geometry is written in.
//!
//! Part 21 files declare their units through a `GLOBAL_UNIT_ASSIGNED_CONTEXT`
//! naming one unit per dimension: an `SI_UNIT` with a prefix, or a
//! `CONVERSION_BASED_UNIT` such as `INCH` or `DEGREE` defined through a
//! `MEASURE_WITH_UNIT` in some other unit. The declared accuracy is an
//! `UNCERTAINTY_MEASURE_WITH_UNIT` in the same context. Files from every
//! system spell these the same way; files with no context at all are read as
//! millimetres and radians.

use crate::{Entity, Graph, Value};

/// What one unit of the file measures.
#[derive(Clone, Debug, PartialEq)]
pub struct Units {
    /// Multiplies every length in the file into millimetres.
    pub length_to_mm: f64,
    /// Multiplies every plane angle in the file into radians.
    pub angle_to_radians: f64,
    /// The declared `distance_accuracy_value`, in millimetres, when the
    /// file declares one.
    pub uncertainty_mm: Option<f64>,
    /// The length unit's name as the file spells it: `MILLIMETRE`, `INCH`,
    /// and so on, for reports.
    pub length_unit_name: String,
}

impl Default for Units {
    fn default() -> Self {
        Self {
            length_to_mm: 1.0,
            angle_to_radians: 1.0,
            uncertainty_mm: None,
            length_unit_name: "MILLIMETRE".to_owned(),
        }
    }
}

/// The factor an SI prefix multiplies by.
fn prefix_factor(prefix: Option<&str>) -> Option<f64> {
    Some(match prefix {
        None => 1.0,
        Some("EXA") => 1.0e18,
        Some("PETA") => 1.0e15,
        Some("TERA") => 1.0e12,
        Some("GIGA") => 1.0e9,
        Some("MEGA") => 1.0e6,
        Some("KILO") => 1.0e3,
        Some("HECTO") => 1.0e2,
        Some("DECA") => 1.0e1,
        Some("DECI") => 1.0e-1,
        Some("CENTI") => 1.0e-2,
        Some("MILLI") => 1.0e-3,
        Some("MICRO") => 1.0e-6,
        Some("NANO") => 1.0e-9,
        Some("PICO") => 1.0e-12,
        Some("FEMTO") => 1.0e-15,
        Some("ATTO") => 1.0e-18,
        Some(_) => return None,
    })
}

/// The factor an SI base unit multiplies by, into millimetres for lengths
/// and radians for plane angles.
fn base_factor(name: &str) -> Option<f64> {
    Some(match name {
        "METRE" => 1000.0,
        "RADIAN" => 1.0,
        _ => return None,
    })
}

/// The name a unit entity carries.
fn unit_name(entity: &Entity) -> String {
    if let Some(conversion) = entity.instance("CONVERSION_BASED_UNIT") {
        return conversion
            .arg(0)
            .as_str()
            .unwrap_or("")
            .to_ascii_uppercase();
    }
    if let Some(si) = entity.instance("SI_UNIT") {
        let prefix = si.arg(0).as_enum().unwrap_or("");
        let name = si.arg(1).as_enum().unwrap_or("");
        return format!("{prefix}{name}");
    }
    String::new()
}

/// What one of `entity` measures in the base unit (millimetres or radians),
/// following conversion-based units through their definitions.
fn unit_factor(graph: &Graph, entity: &Entity, depth: usize) -> Option<f64> {
    if depth > 8 {
        return None;
    }
    if let Some(si) = entity.instance("SI_UNIT") {
        let prefix = prefix_factor(si.arg(0).as_enum())?;
        let base = base_factor(si.arg(1).as_enum()?)?;
        return Some(prefix * base);
    }
    if let Some(conversion) = entity.instance("CONVERSION_BASED_UNIT") {
        let measure = graph.get(conversion.arg(1).as_ref()?)?;
        let instance = measure
            .instance("MEASURE_WITH_UNIT")
            .or_else(|| measure.instances.first())?;
        let value = instance.arg(0).as_f64()?;
        let unit = graph.get(instance.arg(1).as_ref()?)?;
        return Some(value * unit_factor(graph, unit, depth + 1)?);
    }
    None
}

/// The unit entities the first geometric context names, or, without a
/// context, every unit entity of the kind, in id order.
fn units_of_kind<'a>(graph: &'a Graph, kind: &'a str) -> Vec<&'a Entity> {
    let context = graph
        .entities()
        .find_map(|entity| entity.instance("GLOBAL_UNIT_ASSIGNED_CONTEXT"))
        .and_then(|context| context.arg(0).as_refs());
    if let Some(ids) = context {
        let listed: Vec<&Entity> = ids
            .iter()
            .filter_map(|id| graph.get(*id))
            .filter(|entity| entity.is(kind))
            .collect();
        if !listed.is_empty() {
            return listed;
        }
    }
    graph.of_kind(kind).collect()
}

pub(crate) fn read(graph: &Graph) -> Units {
    let mut units = Units::default();
    if let Some(length) = units_of_kind(graph, "LENGTH_UNIT")
        .into_iter()
        .find_map(|entity| unit_factor(graph, entity, 0).map(|factor| (entity, factor)))
    {
        units.length_to_mm = length.1;
        units.length_unit_name = unit_name(length.0);
    }
    if let Some(angle) = units_of_kind(graph, "PLANE_ANGLE_UNIT")
        .into_iter()
        .find_map(|entity| unit_factor(graph, entity, 0))
    {
        units.angle_to_radians = angle;
    }
    units.uncertainty_mm = graph
        .of_kind("UNCERTAINTY_MEASURE_WITH_UNIT")
        .find_map(|entity| {
            let instance = entity.instance("UNCERTAINTY_MEASURE_WITH_UNIT")?;
            let value = match instance.arg(0) {
                Value::Typed(_, inner) => inner.as_f64()?,
                other => other.as_f64()?,
            };
            let unit = graph.get(instance.arg(1).as_ref()?)?;
            let factor = unit_factor(graph, unit, 0)?;
            (value > 0.0 && value.is_finite()).then_some(value * factor)
        });
    units
}
