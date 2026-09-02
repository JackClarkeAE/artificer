//! Scripting runtime for compiling `.art` CAD scripts into [`ApiCommand`]s.
//!
//! A script is a straight line of feature calls with parameters at the top:
//!
//! ```text
//! param width: f64 = 60.0;
//! let base = box(size: [width, 40, 25], label: "base");
//! drill(face: base.face("top_face"), center: [0, 0], diameter: 14, depth: 25, label: "bore");
//! ```
//!
//! Every builtin below maps onto one API command, so anything the JSON-RPC
//! server can do a script can do: primitives, sketches on a plane or a face
//! with extrusions and revolves, drills, push-pulls, fillets and chamfers,
//! mirrors, patterns, and the three Booleans. Angles are degrees throughout.

pub mod ast;
pub mod lexer;
pub mod parser;

use std::collections::BTreeMap;
use std::fmt;

use artificer_protocol::{EntityKind, Point2, Point3, Vector3};

use crate::api::commands::{
    ApiCommand, ExtrudeOp, SketchConstraint, SketchEntity, SketchPlane, StepLabel,
};
use crate::api::debug::{ApiError, ApiErrorCode};
use crate::api::scripting::ast::{AstNode, BinaryOperator, Expression, UnaryOperator};
use crate::api::scripting::lexer::tokenize;
use crate::api::scripting::parser::Parser;
use crate::api::selectors::{
    EntitySelector, Extremum, GeometricSelector, Metric, NormalMatch, SurfaceFilter,
};

/// Why a script did not compile: a parse failure or an evaluation failure,
/// each with the line and column it happened on when that is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptError {
    Parse {
        message: String,
        location: Option<(usize, usize)>,
    },
    Eval {
        message: String,
        location: Option<(usize, usize)>,
    },
}

impl ScriptError {
    /// A one-word kind for consoles that colour by it.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Parse { .. } => "Parse error",
            Self::Eval { .. } => "Evaluation error",
        }
    }

    /// The message without the kind prefix.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Parse { message, .. } | Self::Eval { message, .. } => message,
        }
    }

    /// The `(line, column)` the error points at, one-based, if known.
    #[must_use]
    pub const fn location(&self) -> Option<(usize, usize)> {
        match self {
            Self::Parse { location, .. } | Self::Eval { location, .. } => *location,
        }
    }

    fn eval(message: impl Into<String>) -> Self {
        Self::Eval {
            message: message.into(),
            location: None,
        }
    }

    /// Attaches a location to an error that has none yet: the innermost
    /// call is the one that names where it went wrong.
    fn at(self, line: usize, col: usize) -> Self {
        match self {
            Self::Eval {
                message,
                location: None,
            } => Self::Eval {
                message,
                location: Some((line, col)),
            },
            other => other,
        }
    }

    /// Lifts the lexer's and parser's `... at L:C` messages into a location.
    fn parse(message: String) -> Self {
        let location = message.rsplit_once(" at ").and_then(|(_, tail)| {
            let tail = tail.trim_end_matches(|c: char| !c.is_ascii_digit());
            let (line, col) = tail.split_once(':')?;
            Some((line.parse().ok()?, col.parse().ok()?))
        });
        Self::Parse { message, location }
    }
}

impl fmt::Display for ScriptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.location() {
            Some((line, col)) if !self.message().contains(" at ") => {
                write!(
                    formatter,
                    "{} at line {line}, column {col}: {}",
                    self.kind(),
                    self.message()
                )
            }
            _ => write!(formatter, "{}: {}", self.kind(), self.message()),
        }
    }
}

impl std::error::Error for ScriptError {}

impl From<ScriptError> for ApiError {
    fn from(err: ScriptError) -> Self {
        ApiError::new(ApiErrorCode::ScriptError, err.to_string())
    }
}

/// One `param` declaration as a customizer sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptParameter {
    pub name: String,
    /// The declared type, `f64` when the script wrote none.
    pub param_type: String,
    /// The default the script gives it, evaluated with earlier parameters
    /// in scope; `None` when the default is not a number.
    pub default: Option<f64>,
    /// The line the declaration starts on, one-based.
    pub line: usize,
}

/// Parses a script far enough to list its parameters, without building
/// anything. Defaults that depend on earlier parameters evaluate in order.
pub fn script_parameters(source: &str) -> Result<Vec<ScriptParameter>, ScriptError> {
    let tokens = tokenize(source).map_err(ScriptError::parse)?;
    let mut parser = Parser::new(tokens);
    let ast_nodes = parser.parse_program().map_err(ScriptError::parse)?;
    let mut env = prelude();
    let mut parameters = Vec::new();
    for node in ast_nodes {
        if let AstNode::ParamDecl {
            name,
            param_type,
            default_value,
            line,
        } = node
        {
            let value = eval_expr(&default_value, &env)?;
            let default = match &value {
                Value::Number(number) => Some(*number),
                _ => None,
            };
            env.insert(name.clone(), value);
            parameters.push(ScriptParameter {
                name,
                param_type,
                default,
                line,
            });
        }
    }
    Ok(parameters)
}

/// Evaluates a `.art` script with optional parameter overrides, returning
/// the sequence of API commands it describes.
pub fn compile_script(
    source: &str,
    param_overrides: &BTreeMap<String, f64>,
) -> Result<Vec<ApiCommand>, ScriptError> {
    compile_program(source, param_overrides).map(|program| program.commands)
}

/// A compiled script: its commands, and the names it gave to faces and
/// edges along the way.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptProgram {
    pub commands: Vec<ApiCommand>,
    /// Every top-level `let name = <selector>` in script order. A host
    /// resolves each against the finished body to show the user which face
    /// or edge the script calls by that name.
    pub names: Vec<(String, EntitySelector)>,
}

/// The most loop iterations one script may run in total, so a runaway range
/// is an error rather than a session that never returns.
pub const MAX_LOOP_ITERATIONS: usize = 10_000;

/// Evaluates a `.art` script with optional parameter overrides, returning
/// its commands and the selector names it bound.
pub fn compile_program(
    source: &str,
    param_overrides: &BTreeMap<String, f64>,
) -> Result<ScriptProgram, ScriptError> {
    let tokens = tokenize(source).map_err(ScriptError::parse)?;
    let mut parser = Parser::new(tokens);
    let ast_nodes = parser.parse_program().map_err(ScriptError::parse)?;

    let mut env = prelude();
    let mut program = ScriptProgram {
        commands: Vec::new(),
        names: Vec::new(),
    };
    let mut budget = MAX_LOOP_ITERATIONS;
    run_block(
        &ast_nodes,
        param_overrides,
        &mut env,
        &mut program,
        &mut budget,
        true,
    )?;
    Ok(program)
}

fn run_block(
    nodes: &[AstNode],
    param_overrides: &BTreeMap<String, f64>,
    env: &mut BTreeMap<String, Value>,
    program: &mut ScriptProgram,
    budget: &mut usize,
    top_level: bool,
) -> Result<(), ScriptError> {
    for node in nodes {
        match node {
            AstNode::ParamDecl {
                name,
                default_value,
                line,
                ..
            } => {
                if !top_level {
                    return Err(ScriptError::Eval {
                        message:
                            "A `param` is declared at the top of the script, not inside a loop"
                                .to_owned(),
                        location: Some((*line, 1)),
                    });
                }
                let val = if let Some(&override_val) = param_overrides.get(name) {
                    Value::Number(override_val)
                } else {
                    eval_expr(default_value, env)?
                };
                env.insert(name.clone(), val);
            }
            AstNode::LetBinding { name, value } => {
                let evaluated = eval_expr(value, env)?;
                match &evaluated {
                    Value::Command(cmd) => {
                        program.commands.push(cmd.clone());
                        env.insert(name.clone(), Value::Step(StepLabel(cmd.label().to_owned())));
                    }
                    Value::Selector(selector) => {
                        if top_level {
                            program.names.retain(|(existing, _)| existing != name);
                            program.names.push((name.clone(), selector.clone()));
                        }
                        env.insert(name.clone(), evaluated);
                    }
                    _ => {
                        env.insert(name.clone(), evaluated);
                    }
                }
            }
            AstNode::Statement(expr) => {
                let evaluated = eval_expr(expr, env)?;
                if let Value::Command(cmd) = evaluated {
                    program.commands.push(cmd);
                }
            }
            AstNode::For {
                variable,
                start,
                end,
                body,
                line,
                col,
            } => {
                let at = |error: ScriptError| error.at(*line, *col);
                let start = eval_expr(start, env).map_err(at)?.as_number().map_err(at)?;
                let end = eval_expr(end, env).map_err(at)?.as_number().map_err(at)?;
                if start.fract() != 0.0 || end.fract() != 0.0 {
                    return Err(at(ScriptError::eval(format!(
                        "A `for` range counts whole numbers; got {start}..{end}"
                    ))));
                }
                let mut index = start;
                while index < end {
                    if *budget == 0 {
                        return Err(at(ScriptError::eval(format!(
                            "The script runs more than {MAX_LOOP_ITERATIONS} loop iterations"
                        ))));
                    }
                    *budget -= 1;
                    env.insert(variable.clone(), Value::Number(index));
                    run_block(body, param_overrides, env, program, budget, false)?;
                    index += 1.0;
                }
            }
        }
    }
    Ok(())
}

/// A number as script text: whole numbers without a fraction, so
/// `"bolt_" + 3` is `bolt_3`.
fn number_text(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 1.0e15 {
        format!("{}", number as i64)
    } else {
        number.to_string()
    }
}

/// The names every script starts with.
fn prelude() -> BTreeMap<String, Value> {
    let mut env = BTreeMap::new();
    env.insert("pi".to_owned(), Value::Number(std::f64::consts::PI));
    env
}

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Selector(EntitySelector),
    Step(StepLabel),
    Command(ApiCommand),
    /// A sketch entity awaiting a `sketch(...)` call to gather it.
    Entity(SketchEntity),
}

impl Value {
    fn describe(&self) -> String {
        match self {
            Self::Number(number) => format!("the number {number}"),
            Self::String(text) => format!("the string \"{text}\""),
            Self::Array(items) => format!("an array of {} items", items.len()),
            Self::Selector(_) => "an entity selector".to_owned(),
            Self::Step(label) => format!("the step \"{label}\""),
            Self::Command(command) => format!("the step \"{}\"", command.label()),
            Self::Entity(_) => "a sketch entity".to_owned(),
        }
    }

    fn as_number(&self) -> Result<f64, ScriptError> {
        match self {
            Self::Number(n) => Ok(*n),
            other => Err(ScriptError::eval(format!(
                "Expected a number, got {}",
                other.describe()
            ))),
        }
    }

    fn as_string(&self) -> Result<&str, ScriptError> {
        match self {
            Self::String(s) => Ok(s.as_str()),
            other => Err(ScriptError::eval(format!(
                "Expected a string, got {}",
                other.describe()
            ))),
        }
    }

    fn as_point3(&self) -> Result<Point3, ScriptError> {
        match self {
            Self::Array(arr) if arr.len() == 3 => Ok(Point3::new(
                arr[0].as_number()?,
                arr[1].as_number()?,
                arr[2].as_number()?,
            )),
            other => Err(ScriptError::eval(format!(
                "Expected an [x, y, z] array, got {}",
                other.describe()
            ))),
        }
    }

    fn as_vector3(&self) -> Result<Vector3, ScriptError> {
        let point = self.as_point3()?;
        Ok(Vector3::new(point.x, point.y, point.z))
    }

    fn as_point2(&self) -> Result<Point2, ScriptError> {
        match self {
            Self::Array(arr) if arr.len() == 2 => {
                Ok(Point2::new(arr[0].as_number()?, arr[1].as_number()?))
            }
            other => Err(ScriptError::eval(format!(
                "Expected an [x, y] array, got {}",
                other.describe()
            ))),
        }
    }

    fn as_selector(&self) -> Result<EntitySelector, ScriptError> {
        match self {
            Self::Selector(sel) => Ok(sel.clone()),
            other => Err(ScriptError::eval(format!(
                "Expected an entity selector such as faces(\">Z\") or a step's .face(...), got {}",
                other.describe()
            ))),
        }
    }

    fn as_step(&self) -> Result<StepLabel, ScriptError> {
        match self {
            Self::Step(label) => Ok(label.clone()),
            Self::Command(command) => Ok(StepLabel(command.label().to_owned())),
            other => Err(ScriptError::eval(format!(
                "Expected a step (a `let` bound to a feature call), got {}",
                other.describe()
            ))),
        }
    }

    fn as_selectors(&self) -> Result<Vec<EntitySelector>, ScriptError> {
        match self {
            Self::Selector(s) => Ok(vec![s.clone()]),
            Self::Array(arr) => arr.iter().map(Value::as_selector).collect(),
            other => Err(ScriptError::eval(format!(
                "Expected an edge selector or an array of them, got {}",
                other.describe()
            ))),
        }
    }
}

/// The named arguments of one call, with typed accessors that name the
/// call and the argument in every refusal.
struct Args<'a> {
    call: &'a str,
    values: BTreeMap<&'a str, Value>,
}

impl<'a> Args<'a> {
    fn new(
        call: &'a str,
        named_args: &'a [(String, Expression)],
        env: &BTreeMap<String, Value>,
    ) -> Result<Self, ScriptError> {
        let mut values = BTreeMap::new();
        for (key, expression) in named_args {
            values.insert(key.as_str(), eval_expr(expression, env)?);
        }
        Ok(Self { call, values })
    }

    fn required(&self, name: &str) -> Result<&Value, ScriptError> {
        self.values
            .get(name)
            .ok_or_else(|| ScriptError::eval(format!("{}() requires `{name}`", self.call)))
    }

    fn number(&self, name: &str) -> Result<f64, ScriptError> {
        self.required(name)?.as_number()
    }

    fn number_or(&self, name: &str, default: f64) -> Result<f64, ScriptError> {
        self.values.get(name).map_or(Ok(default), Value::as_number)
    }

    fn point3_or(&self, name: &str, default: Point3) -> Result<Point3, ScriptError> {
        self.values.get(name).map_or(Ok(default), Value::as_point3)
    }

    fn vector3_or(&self, name: &str, default: Vector3) -> Result<Vector3, ScriptError> {
        self.values.get(name).map_or(Ok(default), Value::as_vector3)
    }

    fn label(&self) -> Result<String, ScriptError> {
        self.values.get("label").map_or_else(
            || Ok(self.call.to_owned()),
            |value| value.as_string().map(str::to_owned),
        )
    }

    /// A radius given directly or as a diameter.
    fn radius(&self) -> Result<f64, ScriptError> {
        if let Some(radius) = self.values.get("radius") {
            return radius.as_number();
        }
        if let Some(diameter) = self.values.get("diameter") {
            return Ok(diameter.as_number()? / 2.0);
        }
        Err(ScriptError::eval(format!(
            "{}() requires `radius` or `diameter`",
            self.call
        )))
    }

    fn operation(&self) -> Result<ExtrudeOp, ScriptError> {
        match self.values.get("operation") {
            None => Ok(ExtrudeOp::New),
            Some(value) => match value.as_string()? {
                "new" => Ok(ExtrudeOp::New),
                "add" | "join" | "union" => Ok(ExtrudeOp::Add),
                "cut" | "subtract" => Ok(ExtrudeOp::Cut),
                other => Err(ScriptError::eval(format!(
                    "{}(): `operation` is \"new\", \"add\" or \"cut\", not \"{other}\"",
                    self.call
                ))),
            },
        }
    }

    fn regions(&self) -> Result<Vec<u32>, ScriptError> {
        match self.values.get("regions") {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| item.as_number().map(|number| number as u32))
                .collect(),
            Some(Value::Number(number)) => Ok(vec![*number as u32]),
            Some(other) => Err(ScriptError::eval(format!(
                "{}(): `regions` is an array of region indices, got {}",
                self.call,
                other.describe()
            ))),
        }
    }
}

fn eval_expr(expr: &Expression, env: &BTreeMap<String, Value>) -> Result<Value, ScriptError> {
    match expr {
        Expression::Number(n) => Ok(Value::Number(*n)),
        Expression::String(s) => Ok(Value::String(s.clone())),
        Expression::Identifier(id) => env
            .get(id)
            .cloned()
            .ok_or_else(|| ScriptError::eval(format!("Undefined identifier: `{id}`"))),
        Expression::Array(elements) => {
            let mut arr = Vec::new();
            for el in elements {
                arr.push(eval_expr(el, env)?);
            }
            Ok(Value::Array(arr))
        }
        Expression::UnaryOp { op, operand } => {
            let val = eval_expr(operand, env)?.as_number()?;
            match op {
                UnaryOperator::Neg => Ok(Value::Number(-val)),
            }
        }
        Expression::BinaryOp { left, op, right } => {
            let left = eval_expr(left, env)?;
            let right = eval_expr(right, env)?;
            // `+` joins text: a string with a string or a number, either
            // way round, which is how a loop builds its labels.
            if *op == BinaryOperator::Add
                && matches!(left, Value::String(_)) | matches!(right, Value::String(_))
            {
                let text = |value: &Value| -> Result<String, ScriptError> {
                    match value {
                        Value::String(text) => Ok(text.clone()),
                        Value::Number(number) => Ok(number_text(*number)),
                        other => Err(ScriptError::eval(format!(
                            "`+` joins strings and numbers, got {}",
                            other.describe()
                        ))),
                    }
                };
                return Ok(Value::String(format!("{}{}", text(&left)?, text(&right)?)));
            }
            let l = left.as_number()?;
            let r = right.as_number()?;
            let res = match op {
                BinaryOperator::Add => l + r,
                BinaryOperator::Sub => l - r,
                BinaryOperator::Mul => l * r,
                BinaryOperator::Div => {
                    if r.abs() < 1e-12 {
                        return Err(ScriptError::eval("Division by zero"));
                    }
                    l / r
                }
            };
            Ok(Value::Number(res))
        }
        Expression::FunctionCall {
            name,
            named_args,
            positional_args,
            line,
            col,
        } => eval_function_call(name, named_args, positional_args, env)
            .map_err(|error| error.at(*line, *col)),
        Expression::MethodCall {
            target,
            method,
            named_args,
            positional_args,
            line,
            col,
        } => eval_method_call(target, method, named_args, positional_args, env)
            .map_err(|error| error.at(*line, *col)),
    }
}

/// The one positional argument a selector call takes.
fn positional_string(
    name: &str,
    positional_args: &[Expression],
    env: &BTreeMap<String, Value>,
    example: &str,
) -> Result<String, ScriptError> {
    match positional_args.first() {
        Some(expression) => Ok(eval_expr(expression, env)?.as_string()?.to_owned()),
        None => Err(ScriptError::eval(format!(
            "{name}() requires a selector string, e.g. {example}"
        ))),
    }
}

fn math_call(
    name: &str,
    positional_args: &[Expression],
    env: &BTreeMap<String, Value>,
) -> Result<Option<Value>, ScriptError> {
    let numbers = || -> Result<Vec<f64>, ScriptError> {
        positional_args
            .iter()
            .map(|expression| eval_expr(expression, env)?.as_number())
            .collect()
    };
    let one = |numbers: &[f64]| -> Result<f64, ScriptError> {
        match numbers {
            [value] => Ok(*value),
            _ => Err(ScriptError::eval(format!("{name}() takes one number"))),
        }
    };
    let two = |numbers: &[f64]| -> Result<(f64, f64), ScriptError> {
        match numbers {
            [a, b] => Ok((*a, *b)),
            _ => Err(ScriptError::eval(format!("{name}() takes two numbers"))),
        }
    };
    let value = match name {
        "sqrt" => {
            let value = one(&numbers()?)?;
            if value < 0.0 {
                return Err(ScriptError::eval("sqrt() of a negative number"));
            }
            value.sqrt()
        }
        "abs" => one(&numbers()?)?.abs(),
        "floor" => one(&numbers()?)?.floor(),
        "ceil" => one(&numbers()?)?.ceil(),
        "round" => one(&numbers()?)?.round(),
        "sin" => one(&numbers()?)?.to_radians().sin(),
        "cos" => one(&numbers()?)?.to_radians().cos(),
        "tan" => one(&numbers()?)?.to_radians().tan(),
        "asin" => one(&numbers()?)?.asin().to_degrees(),
        "acos" => one(&numbers()?)?.acos().to_degrees(),
        "atan" => one(&numbers()?)?.atan().to_degrees(),
        "atan2" => {
            let (y, x) = two(&numbers()?)?;
            y.atan2(x).to_degrees()
        }
        "pow" => {
            let (base, exponent) = two(&numbers()?)?;
            base.powf(exponent)
        }
        "hypot" => {
            let (a, b) = two(&numbers()?)?;
            a.hypot(b)
        }
        "min" | "max" => {
            let numbers = numbers()?;
            if numbers.is_empty() {
                return Err(ScriptError::eval(format!(
                    "{name}() takes at least one number"
                )));
            }
            numbers.into_iter().fold(
                if name == "min" {
                    f64::INFINITY
                } else {
                    f64::NEG_INFINITY
                },
                |acc, x| {
                    if name == "min" {
                        acc.min(x)
                    } else {
                        acc.max(x)
                    }
                },
            )
        }
        "clamp" => {
            let numbers = numbers()?;
            match numbers.as_slice() {
                [value, low, high] => value.clamp(*low, *high),
                _ => return Err(ScriptError::eval("clamp() takes a value, a low and a high")),
            }
        }
        _ => return Ok(None),
    };
    if !value.is_finite() {
        return Err(ScriptError::eval(format!(
            "{name}() did not produce a finite number"
        )));
    }
    Ok(Some(Value::Number(value)))
}

fn face_selector(spec: &str) -> Result<EntitySelector, ScriptError> {
    let by_normal = |direction: Vector3| GeometricSelector::FaceByNormal {
        direction,
        match_kind: NormalMatch::Closest,
    };
    let selector = match spec {
        ">Z" | "top" => by_normal(Vector3::new(0.0, 0.0, 1.0)),
        "<Z" | "bottom" => by_normal(Vector3::new(0.0, 0.0, -1.0)),
        ">Y" | "back" => by_normal(Vector3::new(0.0, 1.0, 0.0)),
        "<Y" | "front" => by_normal(Vector3::new(0.0, -1.0, 0.0)),
        ">X" | "right" => by_normal(Vector3::new(1.0, 0.0, 0.0)),
        "<X" | "left" => by_normal(Vector3::new(-1.0, 0.0, 0.0)),
        "largest" => GeometricSelector::ByExtremum {
            metric: Metric::Area,
            extremum: Extremum::Maximum,
            kind: EntityKind::Face,
        },
        "smallest" => GeometricSelector::ByExtremum {
            metric: Metric::Area,
            extremum: Extremum::Minimum,
            kind: EntityKind::Face,
        },
        "planar" => GeometricSelector::ByType {
            surface_type: SurfaceFilter::Planar,
            kind: EntityKind::Face,
        },
        "cylindrical" => GeometricSelector::ByType {
            surface_type: SurfaceFilter::Cylindrical,
            kind: EntityKind::Face,
        },
        _ => {
            return Err(ScriptError::eval(format!(
                "Unknown face selector `{spec}`; use >X <X >Y <Y >Z <Z, top/bottom/front/back/left/right, largest, smallest, planar or cylindrical"
            )));
        }
    };
    Ok(EntitySelector::ByGeometry { selector })
}

fn edge_selector(spec: &str) -> Result<EntitySelector, ScriptError> {
    let parallel = |direction: Vector3| GeometricSelector::EdgesParallelTo { direction };
    let selector = match spec {
        "|Z" => parallel(Vector3::new(0.0, 0.0, 1.0)),
        "|Y" => parallel(Vector3::new(0.0, 1.0, 0.0)),
        "|X" => parallel(Vector3::new(1.0, 0.0, 0.0)),
        "longest" => GeometricSelector::ByExtremum {
            metric: Metric::Length,
            extremum: Extremum::Maximum,
            kind: EntityKind::Edge,
        },
        "shortest" => GeometricSelector::ByExtremum {
            metric: Metric::Length,
            extremum: Extremum::Minimum,
            kind: EntityKind::Edge,
        },
        _ => {
            return Err(ScriptError::eval(format!(
                "Unknown edge selector `{spec}`; use |X, |Y, |Z, longest or shortest"
            )));
        }
    };
    Ok(EntitySelector::ByGeometry { selector })
}

fn sketch_plane(value: &Value) -> Result<SketchPlane, ScriptError> {
    match value {
        Value::String(name) => match name.to_ascii_uppercase().as_str() {
            "XY" => Ok(SketchPlane::XY),
            "XZ" => Ok(SketchPlane::XZ),
            "YZ" => Ok(SketchPlane::YZ),
            _ => Err(ScriptError::eval(format!(
                "sketch(): `on` is \"XY\", \"XZ\", \"YZ\" or a face selector, not \"{name}\""
            ))),
        },
        Value::Selector(selector) => Ok(SketchPlane::OnFace(selector.clone())),
        other => Err(ScriptError::eval(format!(
            "sketch(): `on` is \"XY\", \"XZ\", \"YZ\" or a face selector, got {}",
            other.describe()
        ))),
    }
}

fn sketch_entities(value: &Value) -> Result<Vec<SketchEntity>, ScriptError> {
    let items = match value {
        Value::Array(items) => items.as_slice(),
        Value::Entity(_) => std::slice::from_ref(value),
        other => {
            return Err(ScriptError::eval(format!(
                "sketch(): `entities` is an array of line(), circle(), arc() or rect() calls, got {}",
                other.describe()
            )));
        }
    };
    items
        .iter()
        .map(|item| match item {
            Value::Entity(entity) => Ok(entity.clone()),
            other => Err(ScriptError::eval(format!(
                "sketch(): every entity is a line(), circle(), arc() or rect(), got {}",
                other.describe()
            ))),
        })
        .collect()
}

fn eval_function_call(
    name: &str,
    named_args: &[(String, Expression)],
    positional_args: &[Expression],
    env: &BTreeMap<String, Value>,
) -> Result<Value, ScriptError> {
    if let Some(value) = math_call(name, positional_args, env)? {
        return Ok(value);
    }
    let args = Args::new(name, named_args, env)?;
    let origin = Point3::new(0.0, 0.0, 0.0);
    let up = Vector3::new(0.0, 0.0, 1.0);

    match name {
        // ---- primitives -------------------------------------------------
        "box" => {
            let size = args.required("size")?.as_point3()?;
            Ok(Value::Command(ApiCommand::MakeBox {
                label: args.label()?,
                origin: args.point3_or("origin", origin)?,
                size: [size.x, size.y, size.z],
            }))
        }
        "cylinder" => Ok(Value::Command(ApiCommand::MakeCylinder {
            label: args.label()?,
            center: args.point3_or("center", origin)?,
            axis: args.vector3_or("axis", up)?,
            radius: args.radius()?,
            height: args.number("height")?,
        })),
        // ---- sketches and what grows from them --------------------------
        "line" => Ok(Value::Entity(SketchEntity::Line {
            start: args.required("start")?.as_point2()?,
            end: args.required("end")?.as_point2()?,
        })),
        "circle" => Ok(Value::Entity(SketchEntity::Circle {
            center: args
                .values
                .get("center")
                .map_or(Ok(Point2::new(0.0, 0.0)), Value::as_point2)?,
            radius: args.radius()?,
        })),
        "arc" => Ok(Value::Entity(SketchEntity::Arc {
            center: args
                .values
                .get("center")
                .map_or(Ok(Point2::new(0.0, 0.0)), Value::as_point2)?,
            radius: args.radius()?,
            start_angle: args.number("start_angle")?.to_radians(),
            end_angle: args.number("end_angle")?.to_radians(),
        })),
        "rect" => {
            let width = args.number("width")?;
            let height = args.number("height")?;
            let origin = match (args.values.get("origin"), args.values.get("center")) {
                (Some(origin), _) => origin.as_point2()?,
                (None, Some(center)) => {
                    let center = center.as_point2()?;
                    Point2::new(center.x - width / 2.0, center.y - height / 2.0)
                }
                (None, None) => Point2::new(-width / 2.0, -height / 2.0),
            };
            Ok(Value::Entity(SketchEntity::Rectangle {
                origin,
                width,
                height,
            }))
        }
        "sketch" => Ok(Value::Command(ApiCommand::Sketch {
            label: args.label()?,
            on: sketch_plane(args.required("on")?)?,
            entities: sketch_entities(args.required("entities")?)?,
            constraints: Vec::<SketchConstraint>::new(),
        })),
        "extrude" => Ok(Value::Command(ApiCommand::Extrude {
            label: args.label()?,
            sketch: args.required("sketch")?.as_step()?,
            regions: args.regions()?,
            distance: args.number("distance")?,
            operation: args.operation()?,
            draft_degrees: args.number_or("draft", 0.0)?,
        })),
        "revolve" => Ok(Value::Command(ApiCommand::Revolve {
            label: args.label()?,
            sketch: args.required("sketch")?.as_step()?,
            regions: args.regions()?,
            axis_origin: args.point3_or("axis_origin", origin)?,
            axis_direction: args.vector3_or("axis", up)?,
            angle_degrees: args.number_or("angle", 360.0)?,
            operation: args.operation()?,
        })),
        // ---- face and edge features ------------------------------------
        "drill" => Ok(Value::Command(ApiCommand::DrillHole {
            label: args.label()?,
            face: args.required("face")?.as_selector()?,
            center: args
                .values
                .get("center")
                .map_or(Ok(Point2::new(0.0, 0.0)), Value::as_point2)?,
            diameter: args.radius()? * 2.0,
            depth: args.number("depth")?,
        })),
        "push_pull" => Ok(Value::Command(ApiCommand::PushPull {
            label: args.label()?,
            face: args.required("face")?.as_selector()?,
            distance: args.number("distance")?,
        })),
        "fillet" => Ok(Value::Command(ApiCommand::Fillet {
            label: args.label()?,
            edges: args.required("edges")?.as_selectors()?,
            radius: args.number("radius")?,
        })),
        "chamfer" => Ok(Value::Command(ApiCommand::Chamfer {
            label: args.label()?,
            edges: args.required("edges")?.as_selectors()?,
            distance: args.number("distance")?,
        })),
        // ---- whole-body operations -------------------------------------
        "mirror" => Ok(Value::Command(ApiCommand::Mirror {
            label: args.label()?,
            plane_origin: args.point3_or("origin", origin)?,
            plane_normal: args.required("normal")?.as_vector3()?,
        })),
        "pattern" => {
            let count = args.number("count")?;
            if !(1.0..=f64::from(u16::MAX)).contains(&count) || count.fract() != 0.0 {
                return Err(ScriptError::eval(
                    "pattern(): `count` is a whole number of copies",
                ));
            }
            Ok(Value::Command(ApiCommand::LinearPattern {
                label: args.label()?,
                direction: args.required("direction")?.as_vector3()?,
                spacing: args.number("spacing")?,
                count: count as u16,
            }))
        }
        "union" => Ok(Value::Command(ApiCommand::BooleanUnion {
            label: args.label()?,
            target: args.required("target")?.as_step()?,
            tool: args.required("tool")?.as_step()?,
        })),
        "difference" => Ok(Value::Command(ApiCommand::BooleanDifference {
            label: args.label()?,
            target: args.required("target")?.as_step()?,
            tool: args.required("tool")?.as_step()?,
        })),
        "intersection" => Ok(Value::Command(ApiCommand::BooleanIntersection {
            label: args.label()?,
            target: args.required("target")?.as_step()?,
            tool: args.required("tool")?.as_step()?,
        })),
        // ---- selectors --------------------------------------------------
        "faces" => {
            let spec = positional_string("faces", positional_args, env, "\">Z\"")?;
            Ok(Value::Selector(face_selector(&spec)?))
        }
        "edges" => {
            let spec = positional_string("edges", positional_args, env, "\"|Z\"")?;
            Ok(Value::Selector(edge_selector(&spec)?))
        }
        "nearest" => {
            let point = args.required("point")?.as_point3()?;
            let kind = match args.values.get("kind") {
                None => EntityKind::Face,
                Some(value) => match value.as_string()? {
                    "face" => EntityKind::Face,
                    "edge" => EntityKind::Edge,
                    "vertex" => EntityKind::Vertex,
                    other => {
                        return Err(ScriptError::eval(format!(
                            "nearest(): `kind` is \"face\", \"edge\" or \"vertex\", not \"{other}\""
                        )));
                    }
                },
            };
            Ok(Value::Selector(EntitySelector::ByGeometry {
                selector: GeometricSelector::NearestTo { point, kind },
            }))
        }
        other => Err(ScriptError::eval(format!(
            "Unknown function `{other}`; the features are box, cylinder, sketch, extrude, revolve, drill, push_pull, fillet, chamfer, mirror, pattern, union, difference and intersection"
        ))),
    }
}

fn eval_method_call(
    target_expr: &Expression,
    method: &str,
    named_args: &[(String, Expression)],
    positional_args: &[Expression],
    env: &BTreeMap<String, Value>,
) -> Result<Value, ScriptError> {
    let target = eval_expr(target_expr, env)?;
    let step_label = match target {
        Value::Step(s) => s,
        Value::Command(cmd) => StepLabel(cmd.label().to_owned()),
        other => {
            return Err(ScriptError::eval(format!(
                "`.{method}()` is called on a step, got {}",
                other.describe()
            )));
        }
    };
    let args = Args::new(method, named_args, env)?;
    let role = |default: &str| -> Result<String, ScriptError> {
        if let Some(expression) = positional_args.first() {
            Ok(eval_expr(expression, env)?.as_string()?.to_owned())
        } else if let Some(role) = args.values.get("role") {
            Ok(role.as_string()?.to_owned())
        } else {
            Ok(default.to_owned())
        }
    };
    let ordinal = args
        .values
        .get("ordinal")
        .map(|value| value.as_number().map(|number| number as u32))
        .transpose()?;

    match method {
        "face" => Ok(Value::Selector(EntitySelector::ByHistory {
            from_step: step_label,
            kind: EntityKind::Face,
            role: role("top_face")?,
            ordinal,
        })),
        "edge" => Ok(Value::Selector(EntitySelector::ByHistory {
            from_step: step_label,
            kind: EntityKind::Edge,
            role: role("edge")?,
            ordinal,
        })),
        "edges" => {
            // Every edge the step produced under the role, by ordinal; the
            // session ignores ordinals the step never made.
            let role = role("edge")?;
            let count = args
                .values
                .get("count")
                .map_or(Ok(12.0), Value::as_number)? as u32;
            Ok(Value::Array(
                (0..count)
                    .map(|index| {
                        Value::Selector(EntitySelector::history_edge_ordinal(
                            step_label.0.clone(),
                            role.clone(),
                            index,
                        ))
                    })
                    .collect(),
            ))
        }
        other => Err(ScriptError::eval(format!(
            "Unknown method `.{other}()` on a step; use .face(\"role\"), .edge(\"role\") or .edges(\"role\")"
        ))),
    }
}
