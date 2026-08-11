use artificer_geometry::Vector3;
use artificer_kernel::brep::{Primitive, make_primitive};

/// Fixed-seed constructive topology gate. This is intentionally deterministic
/// and cheap enough for PR CI; long-running randomized mutation belongs to the
/// nightly evidence tier.
#[test]
fn one_hundred_thousand_constructive_breps_validate() {
    let mut state = 0xA076_1D64_78BD_642F_u64;
    for index in 0..100_000 {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let random = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        let first = 0.01 + (random & 0xffff) as f64 / 4096.0;
        let second = 0.01 + ((random >> 16) & 0xffff) as f64 / 4096.0;
        let primitive = match index % 5 {
            0 => Primitive::Box {
                size: Vector3::new(first, second, first + second),
            },
            1 => Primitive::Cylinder {
                radius: first,
                height: second,
            },
            2 => Primitive::Cone {
                radius: first,
                height: second,
            },
            3 => Primitive::Sphere { radius: first },
            _ => Primitive::Torus {
                major_radius: first + second + 0.02,
                minor_radius: first.min(second),
            },
        };
        let body =
            make_primitive(primitive).unwrap_or_else(|code| panic!("case {index}: {code:?}"));
        assert_eq!(body.validate(), Ok(()), "case {index}");
        assert_eq!(body.counts().solids, 1, "case {index}");
    }
}
