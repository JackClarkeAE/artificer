//! Tools, materials, and feeds and speeds (ADR 0057 §2.5).
//!
//! The built-in library is what `tools.json` in the user data folder is
//! seeded with; the workbench loads that file and hands the library here.
//! Feeds and speeds follow the handbook rule: spindle speed from the
//! material's surface speed and the tool's diameter, capped by the spindle;
//! feed from chip load × flutes × rpm on the mill and feed per revolution on
//! the lathe, under constant surface speed (`G96`).

use std::collections::BTreeMap;
use std::f64::consts::PI;

use serde::{Deserialize, Serialize};

/// The material the stock is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Material {
    Aluminium,
    MildSteel,
    Brass,
    Abs,
}

impl Material {
    pub const ALL: [Self; 4] = [Self::Aluminium, Self::MildSteel, Self::Brass, Self::Abs];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Aluminium => "Aluminium",
            Self::MildSteel => "Mild steel",
            Self::Brass => "Brass",
            Self::Abs => "ABS",
        }
    }

    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Aluminium => "aluminium",
            Self::MildSteel => "mild_steel",
            Self::Brass => "brass",
            Self::Abs => "abs",
        }
    }

    /// Density in g/cm³, for the card's mass readout.
    #[must_use]
    pub const fn density(self) -> f64 {
        match self {
            Self::Aluminium => 2.70,
            Self::MildSteel => 7.85,
            Self::Brass => 8.50,
            Self::Abs => 1.04,
        }
    }
}

/// What a tool is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    FlatEndMill,
    BallEndMill,
    Drill,
    CentreDrill,
    /// A roughing insert for outside turning (CNMG).
    TurningInsertRough,
    /// A finishing insert for outside turning (DNMG).
    TurningInsertFinish,
    PartingBlade,
    BoringBar,
}

impl ToolKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::FlatEndMill => "flat end mill",
            Self::BallEndMill => "ball end mill",
            Self::Drill => "drill",
            Self::CentreDrill => "centre drill",
            Self::TurningInsertRough => "roughing insert",
            Self::TurningInsertFinish => "finishing insert",
            Self::PartingBlade => "parting blade",
            Self::BoringBar => "boring bar",
        }
    }

    #[must_use]
    pub const fn is_lathe(self) -> bool {
        matches!(
            self,
            Self::TurningInsertRough
                | Self::TurningInsertFinish
                | Self::PartingBlade
                | Self::BoringBar
        )
    }
}

/// Cutting data for one material: surface speed and chip load.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Feed {
    /// Cutting speed in metres per minute.
    pub surface_speed: f64,
    /// Millimetres per tooth on the mill; millimetres per revolution on the
    /// lathe and for drills.
    pub chip_load: f64,
}

/// One tool in the library.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub number: u32,
    pub kind: ToolKind,
    pub name: String,
    /// Cutting diameter in millimetres. A turning insert's is its inscribed
    /// circle; a parting blade's is its width.
    pub diameter: f64,
    /// Corner radius of an end mill, nose radius of an insert.
    pub corner_radius: f64,
    pub flutes: u32,
    /// How long the cutting edge is: the deepest one pass can reach.
    pub flute_length: f64,
    /// The deepest one pass may cut, axially on the mill, radially on the lathe.
    pub max_depth_of_cut: f64,
    #[serde(default)]
    pub feeds: BTreeMap<Material, Feed>,
}

impl Tool {
    #[must_use]
    pub fn radius(&self) -> f64 {
        self.diameter / 2.0
    }

    #[must_use]
    pub fn feed_for(&self, material: Material) -> Feed {
        self.feeds
            .get(&material)
            .copied()
            .unwrap_or_else(|| default_feed(self.kind, self.diameter, material))
    }

    /// A short name for a tool change comment.
    #[must_use]
    pub fn summary(&self) -> String {
        format!("T{} {}", self.number, self.name)
    }
}

/// The handbook figures the built-in library is seeded with: surface speed
/// in m/min and chip load in mm per tooth or per revolution.
#[must_use]
pub fn default_feed(kind: ToolKind, diameter: f64, material: Material) -> Feed {
    let (speed, load) = match (kind, material) {
        (ToolKind::FlatEndMill | ToolKind::BallEndMill, Material::Aluminium) => (250.0, 0.05),
        (ToolKind::FlatEndMill | ToolKind::BallEndMill, Material::MildSteel) => (100.0, 0.03),
        (ToolKind::FlatEndMill | ToolKind::BallEndMill, Material::Brass) => (200.0, 0.05),
        (ToolKind::FlatEndMill | ToolKind::BallEndMill, Material::Abs) => (200.0, 0.08),
        (ToolKind::Drill | ToolKind::CentreDrill, Material::Aluminium) => (80.0, 0.15),
        (ToolKind::Drill | ToolKind::CentreDrill, Material::MildSteel) => (25.0, 0.10),
        (ToolKind::Drill | ToolKind::CentreDrill, Material::Brass) => (60.0, 0.15),
        (ToolKind::Drill | ToolKind::CentreDrill, Material::Abs) => (50.0, 0.20),
        (ToolKind::TurningInsertRough, Material::Aluminium) => (300.0, 0.25),
        (ToolKind::TurningInsertRough, Material::MildSteel) => (180.0, 0.25),
        (ToolKind::TurningInsertRough, Material::Brass) => (250.0, 0.25),
        (ToolKind::TurningInsertRough, Material::Abs) => (200.0, 0.30),
        (ToolKind::TurningInsertFinish | ToolKind::BoringBar, Material::Aluminium) => (350.0, 0.10),
        (ToolKind::TurningInsertFinish | ToolKind::BoringBar, Material::MildSteel) => (200.0, 0.10),
        (ToolKind::TurningInsertFinish | ToolKind::BoringBar, Material::Brass) => (280.0, 0.10),
        (ToolKind::TurningInsertFinish | ToolKind::BoringBar, Material::Abs) => (220.0, 0.12),
        (ToolKind::PartingBlade, Material::Aluminium) => (150.0, 0.06),
        (ToolKind::PartingBlade, Material::MildSteel) => (100.0, 0.05),
        (ToolKind::PartingBlade, Material::Brass) => (120.0, 0.06),
        (ToolKind::PartingBlade, Material::Abs) => (120.0, 0.08),
    };
    // Chip load scales with the cutter: a 2 mm end mill takes a fraction of
    // what a 12 mm one does.
    let load = match kind {
        ToolKind::FlatEndMill | ToolKind::BallEndMill => load * (diameter / 6.0).clamp(0.25, 2.0),
        ToolKind::Drill | ToolKind::CentreDrill => load * (diameter / 6.0).clamp(0.2, 2.0),
        _ => load,
    };
    Feed {
        surface_speed: speed,
        chip_load: load,
    }
}

/// The tools a setup may choose from, and the machine limits they run under.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolLibrary {
    pub tools: Vec<Tool>,
    /// The mill spindle's top speed, in rpm.
    pub mill_max_rpm: f64,
    /// The lathe spindle's top speed, in rpm.
    pub lathe_max_rpm: f64,
    /// Rapid traverse in mm/min, for the time estimate.
    pub rapid_rate: f64,
    /// Seconds a tool change takes, for the time estimate.
    pub tool_change_seconds: f64,
}

impl ToolLibrary {
    /// The built-in set of ADR 0057 §2.5.
    #[must_use]
    pub fn builtin() -> Self {
        let mut tools = Vec::new();
        let mut number = 1;
        let mut push = |kind: ToolKind,
                        name: String,
                        diameter: f64,
                        corner: f64,
                        flutes: u32,
                        flute_length: f64,
                        doc: f64| {
            let mut feeds = BTreeMap::new();
            for material in Material::ALL {
                feeds.insert(material, default_feed(kind, diameter, material));
            }
            tools.push(Tool {
                number,
                kind,
                name,
                diameter,
                corner_radius: corner,
                flutes,
                flute_length,
                max_depth_of_cut: doc,
                feeds,
            });
            number += 1;
        };
        for diameter in [2.0, 3.0, 4.0, 6.0, 8.0, 10.0, 12.0] {
            for flutes in [2, 4] {
                push(
                    ToolKind::FlatEndMill,
                    format!("{diameter} mm {flutes}-flute end mill"),
                    diameter,
                    0.0,
                    flutes,
                    diameter * 3.0,
                    diameter * 0.5,
                );
            }
        }
        for diameter in [3.0, 6.0, 10.0] {
            push(
                ToolKind::BallEndMill,
                format!("{diameter} mm ball end mill"),
                diameter,
                diameter / 2.0,
                2,
                diameter * 3.0,
                diameter * 0.25,
            );
        }
        push(
            ToolKind::CentreDrill,
            "centre drill".to_owned(),
            3.0,
            0.0,
            2,
            5.0,
            5.0,
        );
        let mut diameter = 1.0;
        while diameter <= 12.0 + 1.0e-9 {
            push(
                ToolKind::Drill,
                format!("{diameter} mm drill"),
                diameter,
                0.0,
                2,
                diameter * 8.0,
                diameter,
            );
            diameter += 0.5;
        }
        push(
            ToolKind::TurningInsertRough,
            "CNMG roughing insert".to_owned(),
            12.7,
            0.8,
            1,
            8.0,
            2.5,
        );
        push(
            ToolKind::TurningInsertFinish,
            "DNMG finishing insert".to_owned(),
            9.525,
            0.4,
            1,
            8.0,
            1.0,
        );
        push(
            ToolKind::PartingBlade,
            "3 mm parting blade".to_owned(),
            3.0,
            0.0,
            1,
            25.0,
            25.0,
        );
        push(
            ToolKind::BoringBar,
            "boring bar".to_owned(),
            8.0,
            0.4,
            1,
            40.0,
            1.0,
        );
        Self {
            tools,
            mill_max_rpm: 10_000.0,
            lathe_max_rpm: 3_000.0,
            rapid_rate: 5_000.0,
            tool_change_seconds: 5.0,
        }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    #[must_use]
    pub fn tool(&self, number: u32) -> Option<&Tool> {
        self.tools.iter().find(|tool| tool.number == number)
    }

    #[must_use]
    pub fn of_kind(&self, kind: ToolKind) -> impl Iterator<Item = &Tool> {
        self.tools.iter().filter(move |tool| tool.kind == kind)
    }

    /// The largest flat end mill whose diameter is at most `max_diameter`
    /// and whose radius is at most `max_radius`, preferring more flutes.
    #[must_use]
    pub fn largest_end_mill(&self, max_diameter: f64, max_radius: f64) -> Option<&Tool> {
        self.of_kind(ToolKind::FlatEndMill)
            .filter(|tool| {
                tool.diameter <= max_diameter + 1.0e-9 && tool.radius() <= max_radius + 1.0e-9
            })
            .max_by(|a, b| {
                a.diameter
                    .total_cmp(&b.diameter)
                    .then_with(|| a.flutes.cmp(&b.flutes))
            })
    }

    /// The drill of exactly this diameter, if the library has one.
    #[must_use]
    pub fn drill_of(&self, diameter: f64) -> Option<&Tool> {
        self.of_kind(ToolKind::Drill)
            .find(|tool| (tool.diameter - diameter).abs() <= 1.0e-6)
    }

    /// The largest drill no wider than `diameter`.
    #[must_use]
    pub fn largest_drill_within(&self, diameter: f64) -> Option<&Tool> {
        self.of_kind(ToolKind::Drill)
            .filter(|tool| tool.diameter <= diameter + 1.0e-9)
            .max_by(|a, b| a.diameter.total_cmp(&b.diameter))
    }

    #[must_use]
    pub fn first_of_kind(&self, kind: ToolKind) -> Option<&Tool> {
        self.of_kind(kind).next()
    }
}

/// Spindle speed for a cutting speed and diameter, capped by the spindle.
#[must_use]
pub fn rpm_for(surface_speed_m_min: f64, diameter_mm: f64, max_rpm: f64) -> f64 {
    if diameter_mm <= 1.0e-9 {
        return max_rpm;
    }
    (surface_speed_m_min * 1000.0 / (PI * diameter_mm))
        .min(max_rpm)
        .max(1.0)
}

/// The mill's feed in mm/min: chip load × flutes × rpm.
#[must_use]
pub fn mill_feed(tool: &Tool, material: Material, max_rpm: f64) -> (f64, f64) {
    let feed = tool.feed_for(material);
    let rpm = rpm_for(feed.surface_speed, tool.diameter, max_rpm).round();
    let flutes = f64::from(tool.flutes.max(1));
    let rate = (feed.chip_load * flutes * rpm).round().max(1.0);
    (rpm, rate)
}

/// The drill's feed in mm/min: feed per revolution × rpm.
#[must_use]
pub fn drill_feed(tool: &Tool, material: Material, max_rpm: f64) -> (f64, f64) {
    let feed = tool.feed_for(material);
    let rpm = rpm_for(feed.surface_speed, tool.diameter, max_rpm).round();
    let rate = (feed.chip_load * rpm).round().max(1.0);
    (rpm, rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_library_has_the_advertised_tools() {
        let library = ToolLibrary::builtin();
        assert_eq!(library.of_kind(ToolKind::FlatEndMill).count(), 14);
        assert_eq!(library.of_kind(ToolKind::BallEndMill).count(), 3);
        assert_eq!(library.of_kind(ToolKind::Drill).count(), 23);
        assert_eq!(library.of_kind(ToolKind::CentreDrill).count(), 1);
        assert!(library.drill_of(6.5).is_some());
        assert!(library.drill_of(6.25).is_none());
        let mut numbers = library
            .tools
            .iter()
            .map(|tool| tool.number)
            .collect::<Vec<_>>();
        numbers.dedup();
        assert_eq!(numbers.len(), library.tools.len());
    }

    #[test]
    fn the_library_round_trips_through_json() {
        let library = ToolLibrary::builtin();
        let text = library.to_json().unwrap();
        assert_eq!(ToolLibrary::from_json(&text).unwrap(), library);
    }

    #[test]
    fn feeds_follow_the_handbook_rule() {
        let library = ToolLibrary::builtin();
        let mill = library.largest_end_mill(12.0, f64::INFINITY).unwrap();
        assert_eq!(mill.diameter, 12.0);
        assert_eq!(mill.flutes, 4);
        let (rpm, feed) = mill_feed(mill, Material::Aluminium, 10_000.0);
        // 250 m/min on Ø12: 6631 rpm; 0.1 mm/tooth × 4 × 6631.
        assert!((rpm - 6631.0).abs() < 1.0);
        assert!((feed - 0.1 * 4.0 * 6631.0).abs() < 5.0, "{feed}");
        // A slow spindle caps the speed.
        assert_eq!(rpm_for(250.0, 2.0, 10_000.0), 10_000.0);
    }
}
