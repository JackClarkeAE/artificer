//! The materials a study may be run in, as a table.
//!
//! Handbook figures for common engineering grades, in the units the solver
//! works in: millimetres and newtons, so a modulus is in megapascals (which
//! is newtons per square millimetre) and a stress comes out in the same.
//! Density is kept in kilograms per cubic metre because that is how a
//! datasheet quotes it; the one place it is used converts.

use serde::{Deserialize, Serialize};

/// One isotropic, linear-elastic material.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Material {
    /// A stable key a document can store.
    pub key: &'static str,
    pub name: &'static str,
    /// Young's modulus, in megapascals.
    pub youngs_modulus_mpa: f64,
    pub poisson_ratio: f64,
    pub density_kg_m3: f64,
    /// The yield strength the safety factor is taken against, in
    /// megapascals.
    pub yield_strength_mpa: f64,
    /// Thermal conductivity, in watts per metre-kelvin.
    pub conductivity_w_mk: f64,
}

impl Material {
    /// The weight of one cubic millimetre, in newtons, under standard
    /// gravity.
    #[must_use]
    pub fn weight_per_mm3_n(&self) -> f64 {
        self.density_kg_m3 * 1.0e-9 * STANDARD_GRAVITY_M_S2
    }

    /// Thermal conductivity in the solver's units: watts per
    /// millimetre-kelvin.
    #[must_use]
    pub fn conductivity_w_mmk(&self) -> f64 {
        self.conductivity_w_mk * 1.0e-3
    }
}

/// Standard gravity, for a body load.
pub const STANDARD_GRAVITY_M_S2: f64 = 9.806_65;

/// The materials the workbench offers, softest metal to stiffest, then the
/// printing plastics.
pub const MATERIALS: [Material; 6] = [
    Material {
        key: "aluminium-6061",
        name: "Aluminium 6061-T6",
        youngs_modulus_mpa: 68_900.0,
        poisson_ratio: 0.33,
        density_kg_m3: 2_700.0,
        yield_strength_mpa: 276.0,
        conductivity_w_mk: 167.0,
    },
    Material {
        key: "mild-steel",
        name: "Mild steel S275",
        youngs_modulus_mpa: 200_000.0,
        poisson_ratio: 0.29,
        density_kg_m3: 7_850.0,
        yield_strength_mpa: 275.0,
        conductivity_w_mk: 50.0,
    },
    Material {
        key: "stainless-304",
        name: "Stainless steel 304",
        youngs_modulus_mpa: 193_000.0,
        poisson_ratio: 0.29,
        density_kg_m3: 8_000.0,
        yield_strength_mpa: 215.0,
        conductivity_w_mk: 16.2,
    },
    Material {
        key: "brass-c260",
        name: "Brass C260",
        youngs_modulus_mpa: 110_000.0,
        poisson_ratio: 0.34,
        density_kg_m3: 8_530.0,
        yield_strength_mpa: 145.0,
        conductivity_w_mk: 120.0,
    },
    Material {
        key: "abs",
        name: "ABS",
        youngs_modulus_mpa: 2_300.0,
        poisson_ratio: 0.35,
        density_kg_m3: 1_040.0,
        yield_strength_mpa: 40.0,
        conductivity_w_mk: 0.17,
    },
    Material {
        key: "pla",
        name: "PLA",
        youngs_modulus_mpa: 3_500.0,
        poisson_ratio: 0.36,
        density_kg_m3: 1_240.0,
        yield_strength_mpa: 60.0,
        conductivity_w_mk: 0.13,
    },
];

/// Looks a material up by its stable key.
#[must_use]
pub fn material_by_key(key: &str) -> Option<Material> {
    MATERIALS
        .iter()
        .find(|material| material.key == key)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_material_is_physically_plausible_and_findable() {
        for material in MATERIALS {
            assert!(material.youngs_modulus_mpa > 0.0, "{}", material.key);
            assert!(
                material.poisson_ratio > 0.0 && material.poisson_ratio < 0.5,
                "{}",
                material.key
            );
            assert!(material.yield_strength_mpa > 0.0, "{}", material.key);
            assert!(material.density_kg_m3 > 0.0, "{}", material.key);
            assert!(material.conductivity_w_mk > 0.0, "{}", material.key);
            assert_eq!(material_by_key(material.key), Some(material));
        }
        assert!(material_by_key("unobtainium").is_none());
    }

    #[test]
    fn a_cubic_millimetre_of_steel_weighs_what_it_should() {
        let steel = material_by_key("mild-steel").unwrap();
        // 7850 kg/m³ is 7.85e-6 kg/mm³, times g.
        assert!((steel.weight_per_mm3_n() - 7.698e-5).abs() < 1.0e-7);
    }
}
