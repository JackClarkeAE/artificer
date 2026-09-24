//! The post-processor: a plan as G-code in the LinuxCNC/Fanuc dialect
//! (ADR 0057 §2.6).
//!
//! One file per setup, with a header naming the setup and its tools. Every
//! coordinate is written with the shortest decimal that reads back to the
//! same double, so the simulation that consumes this file sees exactly the
//! positions the plan holds. The mill writes arc centres absolute
//! (`G90.1`); the lathe writes them by radius (`R`), programs diameters
//! (`G7`) and feeds per revolution (`G95`) under constant surface speed
//! (`G96`).

use std::fmt::Write as _;

use artificer_protocol::Point3;

use crate::plan::{FeedRate, Machine, Move, Plan, Spindle};
use crate::tools::Tool;

/// A number as G-code reads it: the shortest decimal that round-trips,
/// never in exponent form, never `-0`.
#[must_use]
pub fn number(value: f64) -> String {
    let value = if value == 0.0 { 0.0 } else { value };
    let text = format!("{value}");
    if text.contains('e') || text.contains('E') {
        // Display never uses exponents for finite f64, but be safe.
        return format!("{value:.12}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned();
    }
    text
}

/// Writes the plan as G-code.
#[must_use]
pub fn post(plan: &Plan) -> String {
    let mut out = String::new();
    let lathe = plan.machine == Machine::Lathe;
    let _ = writeln!(out, "%");
    let _ = writeln!(out, "(Artificer CAM: {})", sanitise(&plan.setup_name));
    let _ = writeln!(
        out,
        "(Machine: {}; material: {})",
        plan.machine.label(),
        plan.material.label()
    );
    let _ = writeln!(out, "(Tools:)");
    for tool in &plan.tools {
        let _ = writeln!(out, "({})", tool_line(tool));
    }
    for note in &plan.notes {
        let _ = writeln!(out, "({})", sanitise(note));
    }
    if lathe {
        let _ = writeln!(out, "G21 G90 G18 G40 G54 G7 G95");
    } else {
        let _ = writeln!(out, "G21 G90 G17 G40 G49 G54 G90.1 G94");
    }
    let mut current_tool: Option<u32> = None;
    let mut cycle_active = false;
    for (index, operation) in plan.operations.iter().enumerate() {
        let _ = writeln!(
            out,
            "(Operation {}: {})",
            index + 1,
            sanitise(&operation.name)
        );
        for note in &operation.notes {
            let _ = writeln!(out, "({})", sanitise(note));
        }
        // Every operation ends at its own safe position, so a tool change
        // needs no retract of its own.
        if current_tool != Some(operation.tool) {
            if current_tool.is_some() {
                if cycle_active {
                    let _ = writeln!(out, "G80");
                    cycle_active = false;
                }
                let _ = writeln!(out, "M5 M9");
            }
            let _ = writeln!(out, "T{} M6", operation.tool);
            if !lathe {
                let _ = writeln!(out, "G43 H{}", operation.tool);
            }
            current_tool = Some(operation.tool);
        }
        match operation.spindle {
            Spindle::Rpm(rpm) => {
                let _ = writeln!(out, "G97 S{} M3", number(rpm));
            }
            Spindle::SurfaceSpeed {
                metres_per_minute,
                max_rpm,
            } => {
                let _ = writeln!(
                    out,
                    "G96 D{} S{} M3",
                    number(max_rpm),
                    number(metres_per_minute)
                );
            }
        }
        let _ = writeln!(out, "M8");
        // The feed is modal and set once, before the first move, so every
        // motion of the operation, rapids included, is read under it.
        match operation.feed {
            FeedRate::PerMinute(rate) => {
                let _ = writeln!(out, "G94 F{}", number(rate));
            }
            FeedRate::PerRevolution(rate) => {
                let _ = writeln!(out, "G95 F{}", number(rate));
            }
        }
        let mut position: Option<Point3> = None;
        for m in &operation.moves {
            match *m {
                Move::Rapid { to } => {
                    if cycle_active {
                        let _ = writeln!(out, "G80");
                        cycle_active = false;
                    }
                    let _ = writeln!(out, "G0 {}", axes(to, lathe));
                    position = Some(to);
                }
                Move::Feed { to } => {
                    if cycle_active {
                        let _ = writeln!(out, "G80");
                        cycle_active = false;
                    }
                    let _ = writeln!(out, "G1 {}", axes(to, lathe));
                    position = Some(to);
                }
                Move::Arc {
                    to,
                    center,
                    clockwise,
                } => {
                    if cycle_active {
                        let _ = writeln!(out, "G80");
                        cycle_active = false;
                    }
                    let code = if clockwise { "G2" } else { "G3" };
                    if lathe {
                        let from =
                            position.map_or(center, |p| artificer_protocol::Point2::new(p.x, p.z));
                        let radius = (from.x - center.x).hypot(from.y - center.y);
                        let _ = writeln!(out, "{code} {} R{}", axes(to, lathe), number(radius));
                    } else {
                        let _ = writeln!(
                            out,
                            "{code} {} I{} J{}",
                            axes(to, lathe),
                            number(center.x),
                            number(center.y)
                        );
                    }
                    position = Some(to);
                }
                Move::Drill {
                    x,
                    y,
                    depth,
                    retract,
                    peck,
                } => {
                    match peck {
                        Some(peck) if peck > 0.0 => {
                            let _ = writeln!(
                                out,
                                "G99 G83 X{} Y{} Z{} R{} Q{}",
                                number(x),
                                number(y),
                                number(depth),
                                number(retract),
                                number(peck)
                            );
                        }
                        _ => {
                            let _ = writeln!(
                                out,
                                "G99 G81 X{} Y{} Z{} R{}",
                                number(x),
                                number(y),
                                number(depth),
                                number(retract)
                            );
                        }
                    }
                    cycle_active = true;
                    position = Some(Point3::new(x, y, retract));
                }
            }
        }
    }
    if cycle_active {
        let _ = writeln!(out, "G80");
    }
    let _ = writeln!(out, "M5 M9");
    if lathe {
        let _ = writeln!(
            out,
            "G0 X{} Z{}",
            number(plan.safe_height * 2.0),
            number(plan.safe_height)
        );
    } else {
        let _ = writeln!(out, "G0 Z{}", number(plan.safe_height));
    }
    let _ = writeln!(out, "M30");
    let _ = writeln!(out, "%");
    out
}

/// The axis words of a point: `X Y Z` on the mill, `X` (diameter) and `Z`
/// on the lathe.
fn axes(p: Point3, lathe: bool) -> String {
    if lathe {
        format!("X{} Z{}", number(p.x * 2.0), number(p.z))
    } else {
        format!("X{} Y{} Z{}", number(p.x), number(p.y), number(p.z))
    }
}

fn tool_line(tool: &Tool) -> String {
    sanitise(&format!(
        "T{} {} D{} R{} {} flutes",
        tool.number,
        tool.name,
        number(tool.diameter),
        number(tool.corner_radius),
        tool.flutes
    ))
}

/// Comment text with the characters a comment cannot hold removed.
fn sanitise(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '(' => '[',
            ')' => ']',
            '\n' | '\r' => ' ',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_back_exactly_and_never_use_exponents() {
        for value in [
            0.0,
            -0.0,
            1.0 / 3.0,
            1.0e-7,
            12_345.678_901_234_5,
            -2.5,
            1.0e9,
        ] {
            let text = number(value);
            assert!(!text.contains('e'), "{text}");
            let parsed: f64 = text.parse().unwrap();
            assert_eq!(
                parsed.to_bits(),
                if value == 0.0 {
                    0.0_f64.to_bits()
                } else {
                    value.to_bits()
                },
                "{text}"
            );
        }
        assert_eq!(number(-0.0), "0");
    }
}
