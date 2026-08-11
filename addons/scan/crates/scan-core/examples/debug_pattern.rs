use artificer_geometry::{Point3, Vector3};
use artificer_scan_core::report::{ReverseOptions, reverse_engineer, report_summary};
use artificer_scan_core::synth;

fn main() {
    let mut soup = Vec::new();
    for lug in 0..12 {
        let start = std::f64::consts::TAU * lug as f64 / 12.0;
        for i in 0..8usize {
            for j in 0..8usize {
                let corner = |di: usize, dj: usize| {
                    let angle = start + (i + di) as f64 * (14.0f64.to_radians() / 8.0);
                    let z = 2.0 + (j + dj) as f64 * 0.75;
                    let wave = ((i + di) as f64 * 2.5).sin() * ((j + dj) as f64 * 2.5).sin();
                    let radial = 20.0 + 0.6 * wave;
                    Point3::new(radial * angle.cos(), radial * angle.sin(), z)
                };
                let (a, b, c, d) = (corner(0, 0), corner(1, 0), corner(1, 1), corner(0, 1));
                soup.push([a, b, c]);
                soup.push([a, c, d]);
            }
        }
    }
    soup.extend(synth::disk_soup(Point3::new(0.0, 0.0, 0.0), Vector3::new(0.0, 0.0, -1.0), 22.0, 96));
    let mesh = artificer_scan_core::mesh::TriangleMesh::from_triangle_soup(&soup, 1e-9).unwrap();
    let report = reverse_engineer(&mesh, &ReverseOptions::default());
    print!("{}", report_summary(&report));
    println!("plan? {} pattern? {:?}", report.plan.is_some(), report.plan.as_ref().and_then(|p| p.pattern.map(|q| (q.count, q.strength))));
}
// appended: manual correlation probe
