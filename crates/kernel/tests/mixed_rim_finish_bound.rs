//! A fillet asked for on a drilled rim and a straight block edge together.
//!
//! The exact rungs send the two kinds of edge to separate features, so the
//! request falls to the faceted tier, which sweeps the rim one display chord
//! at a time and unions the sweeps. Neighbouring sweeps overlap, and every
//! union split the volume so far along the next one's near-coplanar walls:
//! the removal volume grew by some two hundred polygons per sweep, past forty
//! thousand in three minutes with no end in sight, and before the tier's walks
//! were made iterative it overflowed the stack and took the process with it.
//! The workbench previews an edge finish before it is confirmed, so picking
//! the two edges was enough to freeze or crash it.
//!
//! The tier now declines once the removal volume outgrows the body it would
//! be cut from, and the request is refused by name, quickly.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use artificer_kernel::CancellationToken;
use artificer_kernel::api::scripting::compile_script;
use artificer_kernel::api::session::Session;

#[test]
fn a_drilled_rim_and_a_block_edge_filleted_together_are_refused_promptly() {
    let script = r#"
let b = box(size: [2, 3, 4], label: "b");
let h = drill(face: faces(">Z"), center: [0, 0], diameter: 1, depth: 1000, label: "h");
let f = fillet(edges: [nearest(point: [1.5, 1.5, 4], kind: "edge"), nearest(point: [0, 1.5, 4], kind: "edge")], radius: 0.2, label: "f");
"#;
    let commands = compile_script(script, &BTreeMap::new()).expect("the script compiles");
    let mut session = Session::new();
    let token = CancellationToken::default();
    let mut outcomes = Vec::new();
    for command in commands {
        let started = Instant::now();
        let outcome = session.execute(command, &token);
        outcomes.push((
            outcome.map(|_| ()).map_err(|error| error.message),
            started.elapsed(),
        ));
    }
    let [(box_step, _), (drill_step, _), (fillet_step, elapsed)] = outcomes.as_slice() else {
        panic!("three steps ran: {outcomes:?}");
    };
    assert!(box_step.is_ok() && drill_step.is_ok(), "{outcomes:?}");
    let refusal = fillet_step
        .as_ref()
        .expect_err("a rim and a straight edge are two features");
    assert!(
        refusal.contains("separate features"),
        "the refusal names what to do instead: {refusal}"
    );
    // Generous for a loaded debug build; the unbounded union took minutes.
    assert!(
        *elapsed < Duration::from_secs(30),
        "the refusal came after {elapsed:?}"
    );
}
