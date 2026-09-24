//! A G-code interpreter (ADR 0057 §2.6): the simulation consumes the posted
//! program, not the plan, so what the user watches is what the machine
//! would do.
//!
//! Modal groups follow RS274/NGC as LinuxCNC reads it: motion (`G0`–`G3`,
//! `G80`–`G83`), plane (`G17`–`G19`), distance (`G90`), arc distance
//! (`G90.1`/`G91.1`), feed mode (`G94`/`G95`), spindle mode (`G96`/`G97`),
//! lathe diameter mode (`G7`/`G8`), cycle return (`G98`/`G99`), and the
//! `M` words for the spindle, coolant, tool change and program end. Arcs
//! come in every plane by centre offset or by radius. Anything else is
//! refused by line rather than guessed at.

use artificer_protocol::{Point2, Point3};

use crate::plan::{FeedRate, Machine, Move, Spindle};
use crate::simulate::{Motion, MotionKind, expand_move};

/// Why a program could not be read.
#[derive(Clone, Debug, PartialEq)]
pub struct InterpretError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for InterpretError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for InterpretError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MotionMode {
    Rapid,
    Linear,
    ArcClockwise,
    ArcCounterClockwise,
    Drill,
    Peck,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Plane {
    Xy,
    Xz,
    Yz,
}

/// The interpreter's state, one program at a time.
#[derive(Clone, Debug)]
pub struct Interpreter {
    machine: Machine,
    /// Position in work coordinates; on the lathe `(r, 0, z)`.
    position: Point3,
    motion: MotionMode,
    plane: Plane,
    absolute: bool,
    absolute_arc_centres: bool,
    feed: FeedRate,
    feed_value: f64,
    spindle: Spindle,
    spindle_max_rpm: f64,
    spindle_on: bool,
    tool: u32,
    tool_selected: u32,
    diameter_mode: bool,
    /// The cycle's retract plane and peck.
    cycle_retract: f64,
    cycle_peck: f64,
    operation: usize,
    motions: Vec<Motion>,
    ended: bool,
}

impl Interpreter {
    /// A fresh interpreter for a machine, starting at `start`.
    #[must_use]
    pub fn new(machine: Machine, start: Point3) -> Self {
        Self {
            machine,
            position: start,
            motion: MotionMode::None,
            plane: match machine {
                Machine::Mill => Plane::Xy,
                Machine::Lathe => Plane::Xz,
            },
            absolute: true,
            absolute_arc_centres: false,
            feed: match machine {
                Machine::Mill => FeedRate::PerMinute(0.0),
                Machine::Lathe => FeedRate::PerRevolution(0.0),
            },
            feed_value: 0.0,
            spindle: Spindle::Rpm(0.0),
            spindle_max_rpm: 0.0,
            spindle_on: false,
            tool: 0,
            tool_selected: 0,
            diameter_mode: machine == Machine::Lathe,
            cycle_retract: 0.0,
            cycle_peck: 0.0,
            operation: 0,
            motions: Vec::new(),
            ended: false,
        }
    }

    /// Reads a whole program.
    pub fn run(mut self, text: &str) -> Result<Vec<Motion>, InterpretError> {
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            if self.ended {
                break;
            }
            self.block(raw, line)?;
        }
        Ok(self.motions)
    }

    fn block(&mut self, raw: &str, line: usize) -> Result<(), InterpretError> {
        let (code, comments) = strip_comments(raw);
        for comment in &comments {
            if let Some(rest) = comment.trim().strip_prefix("Operation ")
                && let Some(number) = rest.split(':').next()
                && let Ok(number) = number.trim().parse::<usize>()
            {
                self.operation = number.saturating_sub(1);
            }
        }
        let code = code.trim();
        if code.is_empty() || code == "%" {
            return Ok(());
        }
        let words = parse_words(code, line)?;
        let error = |message: String| InterpretError { line, message };

        // Words that set state before any motion.
        let mut axis: [Option<f64>; 3] = [None, None, None];
        let mut centre: [Option<f64>; 3] = [None, None, None];
        let mut radius: Option<f64> = None;
        let mut retract: Option<f64> = None;
        let mut peck: Option<f64> = None;
        let mut g_codes = Vec::new();
        let mut m_codes = Vec::new();
        let mut feed_word = None;
        let mut speed_word = None;
        let mut max_rpm_word = None;
        let mut tool_word = None;
        for (letter, value) in &words {
            match letter {
                'N' | 'O' => {}
                'G' => g_codes.push(*value),
                'M' => m_codes.push(*value),
                'X' => axis[0] = Some(*value),
                'Y' => axis[1] = Some(*value),
                'Z' => axis[2] = Some(*value),
                'I' => centre[0] = Some(*value),
                'J' => centre[1] = Some(*value),
                'K' => centre[2] = Some(*value),
                'R' => {
                    radius = Some(*value);
                    retract = Some(*value);
                }
                'Q' => peck = Some(*value),
                'F' => feed_word = Some(*value),
                'S' => speed_word = Some(*value),
                'D' => max_rpm_word = Some(*value),
                'T' => tool_word = Some(*value),
                'H' | 'P' | 'L' => {}
                other => return Err(error(format!("unsupported word {other}"))),
            }
        }
        for code in &g_codes {
            let tenths = (*code * 10.0).round() as i64;
            match tenths {
                0 => self.motion = MotionMode::Rapid,
                10 => self.motion = MotionMode::Linear,
                20 => self.motion = MotionMode::ArcClockwise,
                30 => self.motion = MotionMode::ArcCounterClockwise,
                40 => {}
                70 => self.diameter_mode = true,
                80 => self.diameter_mode = false,
                170 => self.plane = Plane::Xy,
                180 => self.plane = Plane::Xz,
                190 => self.plane = Plane::Yz,
                210 => {}
                200 => return Err(error("inch programming is not supported".to_owned())),
                400 | 410 | 420 | 430 | 490 => {}
                540..=590 => {}
                800 => self.motion = MotionMode::None,
                810 => self.motion = MotionMode::Drill,
                830 => self.motion = MotionMode::Peck,
                900 => self.absolute = true,
                901 => self.absolute_arc_centres = true,
                910 => {
                    return Err(error(
                        "incremental distance mode is not supported".to_owned(),
                    ));
                }
                911 => self.absolute_arc_centres = false,
                940 => {
                    self.feed = FeedRate::PerMinute(self.feed_value);
                }
                950 => {
                    self.feed = FeedRate::PerRevolution(self.feed_value);
                }
                960 => {
                    let speed = speed_word.unwrap_or(match self.spindle {
                        Spindle::SurfaceSpeed {
                            metres_per_minute, ..
                        } => metres_per_minute,
                        Spindle::Rpm(_) => 0.0,
                    });
                    if let Some(max) = max_rpm_word {
                        self.spindle_max_rpm = max;
                    }
                    self.spindle = Spindle::SurfaceSpeed {
                        metres_per_minute: speed,
                        max_rpm: self.spindle_max_rpm,
                    };
                    speed_word = None;
                }
                970 => {
                    let speed = speed_word.unwrap_or(match self.spindle {
                        Spindle::Rpm(rpm) => rpm,
                        Spindle::SurfaceSpeed { .. } => 0.0,
                    });
                    self.spindle = Spindle::Rpm(speed);
                    speed_word = None;
                }
                980 | 990 => {}
                other => return Err(error(format!("unsupported G{}", other as f64 / 10.0))),
            }
        }
        if let Some(feed) = feed_word {
            self.feed_value = feed;
            self.feed = match self.feed {
                FeedRate::PerMinute(_) => FeedRate::PerMinute(feed),
                FeedRate::PerRevolution(_) => FeedRate::PerRevolution(feed),
            };
        }
        if let Some(speed) = speed_word {
            self.spindle = match self.spindle {
                Spindle::Rpm(_) => Spindle::Rpm(speed),
                Spindle::SurfaceSpeed { max_rpm, .. } => Spindle::SurfaceSpeed {
                    metres_per_minute: speed,
                    max_rpm,
                },
            };
        }
        if let Some(tool) = tool_word {
            self.tool_selected = tool.round() as u32;
        }
        for code in &m_codes {
            match code.round() as i64 {
                0 | 1 => {}
                2 | 30 => self.ended = true,
                3 | 4 => self.spindle_on = true,
                5 => self.spindle_on = false,
                6 => self.tool = self.tool_selected,
                7..=9 => {}
                other => return Err(error(format!("unsupported M{other}"))),
            }
        }

        // Motion.
        let has_axis = axis.iter().any(Option::is_some);
        if !has_axis {
            return Ok(());
        }
        let lathe = self.machine == Machine::Lathe;
        let mut target = self.position;
        if let Some(x) = axis[0] {
            target.x = if lathe && self.diameter_mode {
                x / 2.0
            } else {
                x
            };
        }
        if let Some(y) = axis[1] {
            target.y = y;
        }
        if let Some(z) = axis[2] {
            target.z = z;
        }
        if lathe {
            target.y = 0.0;
        }
        if self.tool == 0 && self.motion != MotionMode::Rapid {
            return Err(error(
                "a cutting move before any tool was loaded".to_owned(),
            ));
        }
        let mv = match self.motion {
            MotionMode::None => {
                return Err(error("an axis word with no motion mode active".to_owned()));
            }
            MotionMode::Rapid => Move::Rapid { to: target },
            MotionMode::Linear => Move::Feed { to: target },
            MotionMode::ArcClockwise | MotionMode::ArcCounterClockwise => {
                let clockwise = self.motion == MotionMode::ArcClockwise;
                let (from, to) = crate::simulate::plane_points(self.position, target, self.machine);
                let centre = if let Some(r) = radius {
                    arc_centre_from_radius(from, to, r, clockwise).ok_or_else(|| {
                        error("an arc whose ends are further apart than its diameter".to_owned())
                    })?
                } else {
                    let (i, j) = match self.plane {
                        Plane::Xy => (centre[0], centre[1]),
                        Plane::Xz => (centre[0], centre[2]),
                        Plane::Yz => (centre[1], centre[2]),
                    };
                    let (Some(i), Some(j)) = (i, j) else {
                        return Err(error(
                            "an arc needs its centre offsets or a radius".to_owned(),
                        ));
                    };
                    let i = if lathe && self.diameter_mode && self.plane == Plane::Xz {
                        i / 2.0
                    } else {
                        i
                    };
                    if self.absolute_arc_centres {
                        Point2::new(i, j)
                    } else {
                        Point2::new(from.x + i, from.y + j)
                    }
                };
                if self.plane == Plane::Yz {
                    return Err(error("arcs in the YZ plane are not simulated".to_owned()));
                }
                Move::Arc {
                    to: target,
                    center: centre,
                    clockwise,
                }
            }
            MotionMode::Drill | MotionMode::Peck => {
                if let Some(r) = retract {
                    self.cycle_retract = r;
                }
                if let Some(q) = peck {
                    self.cycle_peck = q;
                }
                let depth = axis[2].ok_or_else(|| error("a drilling cycle needs Z".to_owned()))?;
                Move::Drill {
                    x: target.x,
                    y: target.y,
                    depth,
                    retract: self.cycle_retract,
                    peck: (self.motion == MotionMode::Peck).then_some(self.cycle_peck),
                }
            }
        };
        expand_move(
            &mut self.position,
            &mv,
            self.tool,
            self.operation,
            self.feed,
            self.spindle,
            line,
            &mut self.motions,
        );
        Ok(())
    }
}

/// The centre of an arc from its ends and radius: the one on the left of
/// travel for a counter-clockwise arc under one half turn, the other for a
/// clockwise one; a negative radius asks for the longer arc.
#[must_use]
pub fn arc_centre_from_radius(
    from: Point2,
    to: Point2,
    radius: f64,
    clockwise: bool,
) -> Option<Point2> {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let chord = dx.hypot(dy);
    if chord <= 0.0 || radius.abs() * 2.0 < chord - 1.0e-9 {
        return None;
    }
    let mid = Point2::new((from.x + to.x) / 2.0, (from.y + to.y) / 2.0);
    let half = chord / 2.0;
    let height = (radius * radius - half * half).max(0.0).sqrt();
    // Left normal of the chord.
    let (nx, ny) = (-dy / chord, dx / chord);
    let sign = if clockwise { -1.0 } else { 1.0 } * radius.signum();
    Some(Point2::new(
        nx.mul_add(height * sign, mid.x),
        ny.mul_add(height * sign, mid.y),
    ))
}

/// Splits a line into its code and its comments.
fn strip_comments(raw: &str) -> (String, Vec<String>) {
    let mut code = String::new();
    let mut comments = Vec::new();
    let mut depth = 0_usize;
    let mut current = String::new();
    for c in raw.chars() {
        match c {
            '(' => {
                depth += 1;
                if depth == 1 {
                    current.clear();
                }
            }
            ')' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    comments.push(std::mem::take(&mut current));
                }
            }
            ';' if depth == 0 => {
                break;
            }
            _ if depth > 0 => current.push(c),
            _ => code.push(c),
        }
    }
    (code, comments)
}

fn parse_words(code: &str, line: usize) -> Result<Vec<(char, f64)>, InterpretError> {
    let mut words = Vec::new();
    let chars: Vec<char> = code.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c.is_whitespace() || c == '/' {
            index += 1;
            continue;
        }
        if !c.is_ascii_alphabetic() {
            return Err(InterpretError {
                line,
                message: format!("unexpected character {c:?}"),
            });
        }
        let letter = c.to_ascii_uppercase();
        index += 1;
        let start = index;
        while index < chars.len()
            && (chars[index].is_ascii_digit() || matches!(chars[index], '.' | '-' | '+'))
        {
            index += 1;
        }
        let text: String = chars[start..index].iter().collect();
        let value = text.parse::<f64>().map_err(|_| InterpretError {
            line,
            message: format!("{letter} needs a number, found {text:?}"),
        })?;
        words.push((letter, value));
    }
    Ok(words)
}

/// Reads a program for a machine from the start position.
pub fn interpret(
    text: &str,
    machine: Machine,
    start: Point3,
) -> Result<Vec<Motion>, InterpretError> {
    Interpreter::new(machine, start).run(text)
}

/// The cutting motions of a program as the post would have written them,
/// for tests that compare a plan with its own G-code.
#[must_use]
pub fn cutting_kinds(motions: &[Motion]) -> Vec<MotionKind> {
    motions
        .iter()
        .filter(|motion| motion.is_cutting())
        .map(|motion| motion.kind)
        .collect()
}
