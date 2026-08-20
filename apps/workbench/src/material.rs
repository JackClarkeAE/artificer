//! Materials: density for mass properties, and a colour for display.
//!
//! This is deliberately the smallest useful material model. A material carries
//! the one physical quantity mass properties need — density — plus the colour
//! the workbench shades a body with. Nothing here feeds simulation, and no
//! material is ever inferred: an unassigned body reports its volume and says
//! so rather than quoting a mass it cannot justify.
//!
//! Densities are the usual room-temperature handbook values in kilograms per
//! cubic metre. Kernel geometry is in millimetres throughout, so mass is
//! `density * volume * 1e-9`.

use egui::Color32;

/// One selectable material.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    /// Stable identifier persisted in the workspace file. Display names may
    /// be reworded; this may not.
    pub key: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    /// Kilograms per cubic metre.
    pub density: f64,
    pub colour: Color32,
}

impl Material {
    /// Mass in grams for a volume measured in cubic millimetres.
    #[must_use]
    pub fn mass_grams(&self, volume_cubic_millimetres: f64) -> f64 {
        // kg/m^3 * mm^3 = 1e-6 g, so grams = density * volume * 1e-6.
        self.density * volume_cubic_millimetres * 1.0e-6
    }
}

/// The built-in library, ordered by category then name so the picker reads
/// predictably.
pub const LIBRARY: &[Material] = &[
    Material {
        key: "aluminium-6061",
        name: "Aluminium 6061",
        category: "Metal",
        density: 2700.0,
        colour: Color32::from_rgb(196, 200, 205),
    },
    Material {
        key: "brass",
        name: "Brass",
        category: "Metal",
        density: 8500.0,
        colour: Color32::from_rgb(206, 173, 96),
    },
    Material {
        key: "copper",
        name: "Copper",
        category: "Metal",
        density: 8940.0,
        colour: Color32::from_rgb(184, 115, 74),
    },
    Material {
        key: "steel-1018",
        name: "Steel 1018",
        category: "Metal",
        density: 7870.0,
        colour: Color32::from_rgb(140, 146, 154),
    },
    Material {
        key: "stainless-304",
        name: "Stainless 304",
        category: "Metal",
        density: 8000.0,
        colour: Color32::from_rgb(163, 170, 178),
    },
    Material {
        key: "titanium-6al4v",
        name: "Titanium Ti-6Al-4V",
        category: "Metal",
        density: 4430.0,
        colour: Color32::from_rgb(150, 148, 148),
    },
    Material {
        key: "abs",
        name: "ABS",
        category: "Plastic",
        density: 1040.0,
        colour: Color32::from_rgb(222, 222, 216),
    },
    Material {
        key: "acrylic",
        name: "Acrylic",
        category: "Plastic",
        density: 1180.0,
        colour: Color32::from_rgb(206, 222, 230),
    },
    Material {
        key: "nylon-66",
        name: "Nylon 6/6",
        category: "Plastic",
        density: 1150.0,
        colour: Color32::from_rgb(236, 232, 220),
    },
    Material {
        key: "pla",
        name: "PLA",
        category: "Plastic",
        density: 1240.0,
        colour: Color32::from_rgb(214, 226, 206),
    },
    Material {
        key: "polycarbonate",
        name: "Polycarbonate",
        category: "Plastic",
        density: 1200.0,
        colour: Color32::from_rgb(216, 224, 228),
    },
    Material {
        key: "oak",
        name: "Oak",
        category: "Other",
        density: 750.0,
        colour: Color32::from_rgb(198, 162, 110),
    },
    Material {
        key: "glass",
        name: "Soda-lime glass",
        category: "Other",
        density: 2500.0,
        colour: Color32::from_rgb(198, 218, 220),
    },
];

/// Resolves a persisted key. An unknown key means the workspace named a
/// material this build does not carry, which leaves the body unassigned
/// rather than silently substituting one.
#[must_use]
pub fn by_key(key: &str) -> Option<&'static Material> {
    LIBRARY.iter().find(|material| material.key == key)
}

/// Mass properties of one body, or of a set of them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MassProperties {
    pub volume: f64,
    /// `None` until every contributing body has a material.
    pub mass_grams: Option<f64>,
    /// The mass-weighted centre. Falls back to the volume-weighted centroid
    /// when masses are unknown, and is `None` when the kernel could not
    /// certify a centroid for some body.
    pub centre: Option<[f64; 3]>,
    /// True when [`Self::centre`] was accumulated over a mix of bodies that
    /// do have a density and bodies that do not.
    ///
    /// Either weighting is meaningful on its own: with a density everywhere
    /// the result is the centre of mass, and with a density nowhere the
    /// density cancels and the result is the centroid. Mixing them is not —
    /// a body weighted by `density x volume` outweighs one weighted by
    /// `volume` alone by roughly its density, so the unassigned bodies drop
    /// out of the average. The number stays available because it is still
    /// the best estimate on hand, but it is not certified, and the panel
    /// says so.
    pub centre_mixes_unknown_density: bool,
}

/// Accumulates mass properties across bodies.
#[derive(Default)]
pub struct MassAccumulator {
    volume: f64,
    mass: f64,
    every_body_has_mass: bool,
    weighted: [f64; 3],
    weight: f64,
    every_body_has_centroid: bool,
    any_body_has_density: bool,
    started: bool,
}

impl MassAccumulator {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            volume: 0.0,
            mass: 0.0,
            every_body_has_mass: true,
            weighted: [0.0; 3],
            weight: 0.0,
            every_body_has_centroid: true,
            any_body_has_density: false,
            started: false,
        }
    }

    /// Adds one body. `centroid` is the kernel's certified centroid, which is
    /// unavailable for a few analytic bodies; `density` is `None` when the
    /// body has no material.
    pub fn add(&mut self, volume: f64, centroid: Option<[f64; 3]>, density: Option<f64>) {
        self.started = true;
        self.volume += volume;
        match density {
            Some(density) => {
                self.mass += density * volume * 1.0e-6;
                self.any_body_has_density = true;
            }
            None => self.every_body_has_mass = false,
        }
        match centroid {
            Some(centroid) => {
                // Weight by mass when known, otherwise by volume. Either is
                // right on its own — with one density everywhere it cancels —
                // but a mix of the two is not, which is what
                // `any_body_has_density` alongside `every_body_has_mass`
                // records for the caller.
                let weight = density.map_or(volume, |density| density * volume);
                self.weight += weight;
                for (accumulated, coordinate) in self.weighted.iter_mut().zip(centroid) {
                    *accumulated += coordinate * weight;
                }
            }
            None => self.every_body_has_centroid = false,
        }
    }

    #[must_use]
    pub fn finish(self) -> MassProperties {
        let centre = (self.started && self.every_body_has_centroid && self.weight > 0.0)
            .then(|| self.weighted.map(|axis| axis / self.weight));
        MassProperties {
            volume: self.volume,
            mass_grams: (self.started && self.every_body_has_mass).then_some(self.mass),
            centre,
            centre_mixes_unknown_density: centre.is_some()
                && self.any_body_has_density
                && !self.every_body_has_mass,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_library_entry_is_distinct_and_plausible() {
        for material in LIBRARY {
            assert!(
                material.density > 0.0 && material.density < 25_000.0,
                "{} has an implausible density",
                material.name
            );
            assert_eq!(
                LIBRARY
                    .iter()
                    .filter(|other| other.key == material.key)
                    .count(),
                1,
                "{} has a duplicate key",
                material.key
            );
            assert!(by_key(material.key).is_some());
        }
        assert!(by_key("no-such-material").is_none());
    }

    #[test]
    fn a_cubic_centimetre_of_aluminium_weighs_its_density_in_grams() {
        let aluminium = by_key("aluminium-6061").expect("the library carries aluminium");
        // 1 cm^3 is 1000 mm^3, and aluminium is 2.7 g/cm^3.
        assert!((aluminium.mass_grams(1000.0) - 2.7).abs() < 1.0e-12);
    }

    #[test]
    fn mass_properties_report_what_they_cannot_justify() {
        // A body without a material contributes volume but blocks the mass.
        let mut accumulator = MassAccumulator::new();
        accumulator.add(1000.0, Some([0.0, 0.0, 0.0]), Some(2700.0));
        accumulator.add(1000.0, Some([10.0, 0.0, 0.0]), None);
        let properties = accumulator.finish();
        assert!((properties.volume - 2000.0).abs() < 1.0e-12);
        assert_eq!(properties.mass_grams, None);
        assert!(properties.centre.is_some());

        // A body without a certified centroid blocks the centre.
        let mut accumulator = MassAccumulator::new();
        accumulator.add(1000.0, None, Some(2700.0));
        let properties = accumulator.finish();
        assert_eq!(properties.centre, None);
        assert!(properties.mass_grams.is_some());
    }

    #[test]
    fn a_centre_mixing_weighted_and_unweighted_bodies_says_it_is_uncertified() {
        // One assigned body and one unassigned: the assigned body's weight is
        // density x volume and the other's is volume alone, so the assigned
        // one outweighs it ~2700:1 and the "centre" sits almost on top of it.
        // The number stays, but it must not read as certified.
        let mut mixed = MassAccumulator::new();
        mixed.add(1000.0, Some([0.0, 0.0, 0.0]), Some(2700.0));
        mixed.add(1000.0, Some([10.0, 0.0, 0.0]), None);
        let properties = mixed.finish();
        let centre = properties.centre.expect("still the best estimate on hand");
        assert!(
            centre[0] < 0.01,
            "the unassigned body is all but ignored: {centre:?}"
        );
        assert!(
            properties.centre_mixes_unknown_density,
            "a mixed weighting has to declare itself"
        );

        // Densities everywhere is a real centre of mass.
        let mut all_assigned = MassAccumulator::new();
        all_assigned.add(1000.0, Some([0.0, 0.0, 0.0]), Some(2700.0));
        all_assigned.add(1000.0, Some([10.0, 0.0, 0.0]), Some(2700.0));
        assert!(!all_assigned.finish().centre_mixes_unknown_density);

        // Densities nowhere cancels out, leaving the centroid.
        let mut none_assigned = MassAccumulator::new();
        none_assigned.add(1000.0, Some([0.0, 0.0, 0.0]), None);
        none_assigned.add(1000.0, Some([10.0, 0.0, 0.0]), None);
        let properties = none_assigned.finish();
        assert!(!properties.centre_mixes_unknown_density);
        assert!((properties.centre.expect("centroid")[0] - 5.0).abs() < 1.0e-12);
    }

    #[test]
    fn two_equal_masses_balance_midway() {
        let mut accumulator = MassAccumulator::new();
        accumulator.add(1000.0, Some([0.0, 0.0, 0.0]), Some(1000.0));
        accumulator.add(1000.0, Some([20.0, 0.0, 0.0]), Some(1000.0));
        let centre = accumulator.finish().centre.expect("both bodies balance");
        assert!((centre[0] - 10.0).abs() < 1.0e-12);
    }

    #[test]
    fn a_denser_body_pulls_the_centre_toward_itself() {
        let mut accumulator = MassAccumulator::new();
        accumulator.add(1000.0, Some([0.0, 0.0, 0.0]), Some(1000.0));
        accumulator.add(1000.0, Some([20.0, 0.0, 0.0]), Some(3000.0));
        let centre = accumulator.finish().centre.expect("both bodies balance");
        assert!((centre[0] - 15.0).abs() < 1.0e-12);
    }
}
