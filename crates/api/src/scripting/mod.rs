//! Scripting runtime for compiling .art CAD scripts into ApiCommands.

pub mod ast;
pub mod lexer;
pub mod parser;

use std::collections::BTreeMap;

use artificer_protocol::{EntityKind, Point2, Point3, Vector3};
use thiserror::Error;

use crate::commands::{ApiCommand, StepLabel};
use crate::debug::{ApiError, ApiErrorCode};
use crate::scripting::ast::{AstNode, BinaryOperator, Expression, UnaryOperator};
use crate::scripting::lexer::tokenize;
use crate::scripting::parser::Parser;
use crate::selectors::{
    EntitySelector, Extremum, GeometricSelector, Metric, NormalMatch, SurfaceFilter,
};

#[derive(Debug, Error)]
pub enum ScriptError {
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Evaluation error: {0}")]
    Eval(String),
}

impl From<ScriptError> for ApiError {
    fn from(err: ScriptError) -> Self {
        ApiError::new(ApiErrorCode::ScriptError, err.to_string())
    }
}

/// Evaluates a .art script with optional parameter overrides, returning a sequence of ApiCommands.
pub fn compile_script(
    source: &str,
    param_overrides: &BTreeMap<String, f64>,
) -> Result<Vec<ApiCommand>, ScriptError> {
    let tokens = tokenize(source).map_err(ScriptError::Parse)?;
    let mut parser = Parser::new(tokens);
    let ast_nodes = parser.parse_program().map_err(ScriptError::Parse)?;

    let mut env: BTreeMap<String, Value> = BTreeMap::new();
    let mut commands = Vec::new();

    for node in ast_nodes {
        match node {
            AstNode::ParamDecl {
                name,
                default_value,
                ..
            } => {
                let val = if let Some(&override_val) = param_overrides.get(&name) {
                    Value::Number(override_val)
                } else {
                    eval_expr(&default_value, &env)?
                };
                env.insert(name, val);
            }
            AstNode::LetBinding { name, value } => {
                let evaluated = eval_expr(&value, &env)?;
                if let Value::Command(cmd) = &evaluated {
                    commands.push(cmd.clone());
                    env.insert(name, Value::Step(StepLabel(cmd.label().to_owned())));
                } else {
                    env.insert(name, evaluated);
                }
            }
            AstNode::Statement(expr) => {
                let evaluated = eval_expr(&expr, &env)?;
                if let Value::Command(cmd) = evaluated {
                    commands.push(cmd);
                }
            }
        }
    }

    Ok(commands)
}

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Selector(EntitySelector),
    Step(StepLabel),
    Command(ApiCommand),
}

impl Value {
    fn as_number(&self) -> Result<f64, ScriptError> {
        match self {
            Self::Number(n) => Ok(*n),
            other => Err(ScriptError::Eval(format!("Expected number, got {other:?}"))),
        }
    }

    fn as_string(&self) -> Result<&str, ScriptError> {
        match self {
            Self::String(s) => Ok(s.as_str()),
            other => Err(ScriptError::Eval(format!("Expected string, got {other:?}"))),
        }
    }

    fn as_point3(&self) -> Result<Point3, ScriptError> {
        match self {
            Self::Array(arr) if arr.len() == 3 => Ok(Point3::new(
                arr[0].as_number()?,
                arr[1].as_number()?,
                arr[2].as_number()?,
            )),
            other => Err(ScriptError::Eval(format!(
                "Expected [x, y, z] array, got {other:?}"
            ))),
        }
    }

    fn as_vector3(&self) -> Result<Vector3, ScriptError> {
        match self {
            Self::Array(arr) if arr.len() == 3 => Ok(Vector3::new(
                arr[0].as_number()?,
                arr[1].as_number()?,
                arr[2].as_number()?,
            )),
            other => Err(ScriptError::Eval(format!(
                "Expected [x, y, z] vector, got {other:?}"
            ))),
        }
    }

    fn as_point2(&self) -> Result<Point2, ScriptError> {
        match self {
            Self::Array(arr) if arr.len() == 2 => {
                Ok(Point2::new(arr[0].as_number()?, arr[1].as_number()?))
            }
            other => Err(ScriptError::Eval(format!(
                "Expected [x, y] array, got {other:?}"
            ))),
        }
    }

    fn as_selector(&self) -> Result<EntitySelector, ScriptError> {
        match self {
            Self::Selector(sel) => Ok(sel.clone()),
            other => Err(ScriptError::Eval(format!(
                "Expected entity selector, got {other:?}"
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
            .ok_or_else(|| ScriptError::Eval(format!("Undefined identifier: `{id}`"))),
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
            let l = eval_expr(left, env)?.as_number()?;
            let r = eval_expr(right, env)?.as_number()?;
            let res = match op {
                BinaryOperator::Add => l + r,
                BinaryOperator::Sub => l - r,
                BinaryOperator::Mul => l * r,
                BinaryOperator::Div => {
                    if r.abs() < 1e-12 {
                        return Err(ScriptError::Eval("Division by zero".to_owned()));
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
        } => eval_function_call(name, named_args, positional_args, env),
        Expression::MethodCall {
            target,
            method,
            named_args,
            positional_args,
        } => eval_method_call(target, method, named_args, positional_args, env),
    }
}

fn eval_function_call(
    name: &str,
    named_args: &[(String, Expression)],
    positional_args: &[Expression],
    env: &BTreeMap<String, Value>,
) -> Result<Value, ScriptError> {
    let mut args = BTreeMap::new();
    for (k, v) in named_args {
        args.insert(k.as_str(), eval_expr(v, env)?);
    }

    match name {
        "box" => {
            let origin = args
                .get("origin")
                .map(|v| v.as_point3())
                .transpose()?
                .unwrap_or(Point3::new(0.0, 0.0, 0.0));
            let size = args
                .get("size")
                .ok_or_else(|| ScriptError::Eval("box() requires `size` argument".to_owned()))?
                .as_point3()?;
            let label = args
                .get("label")
                .map(|v| v.as_string().map(|s| s.to_owned()))
                .transpose()?
                .unwrap_or_else(|| "box".to_owned());

            Ok(Value::Command(ApiCommand::MakeBox {
                label,
                origin,
                size: [size.x, size.y, size.z],
            }))
        }
        "cylinder" => {
            let center = args
                .get("center")
                .map(|v| v.as_point3())
                .transpose()?
                .unwrap_or(Point3::new(0.0, 0.0, 0.0));
            let axis = args
                .get("axis")
                .map(|v| v.as_vector3())
                .transpose()?
                .unwrap_or(Vector3::new(0.0, 0.0, 1.0));
            let radius = args
                .get("radius")
                .ok_or_else(|| ScriptError::Eval("cylinder() requires `radius`".to_owned()))?
                .as_number()?;
            let height = args
                .get("height")
                .ok_or_else(|| ScriptError::Eval("cylinder() requires `height`".to_owned()))?
                .as_number()?;
            let label = args
                .get("label")
                .map(|v| v.as_string().map(|s| s.to_owned()))
                .transpose()?
                .unwrap_or_else(|| "cylinder".to_owned());

            Ok(Value::Command(ApiCommand::MakeCylinder {
                label,
                center,
                axis,
                radius,
                height,
            }))
        }
        "drill" => {
            let face = args
                .get("face")
                .ok_or_else(|| ScriptError::Eval("drill() requires `face`".to_owned()))?
                .as_selector()?;
            let center = args
                .get("center")
                .ok_or_else(|| ScriptError::Eval("drill() requires `center` [x, y]".to_owned()))?
                .as_point2()?;
            let diameter = args
                .get("diameter")
                .ok_or_else(|| ScriptError::Eval("drill() requires `diameter`".to_owned()))?
                .as_number()?;
            let depth = args
                .get("depth")
                .ok_or_else(|| ScriptError::Eval("drill() requires `depth`".to_owned()))?
                .as_number()?;
            let label = args
                .get("label")
                .map(|v| v.as_string().map(|s| s.to_owned()))
                .transpose()?
                .unwrap_or_else(|| "drill".to_owned());

            Ok(Value::Command(ApiCommand::DrillHole {
                label,
                face,
                center,
                diameter,
                depth,
            }))
        }
        "push_pull" => {
            let face = args
                .get("face")
                .ok_or_else(|| ScriptError::Eval("push_pull() requires `face`".to_owned()))?
                .as_selector()?;
            let distance = args
                .get("distance")
                .ok_or_else(|| ScriptError::Eval("push_pull() requires `distance`".to_owned()))?
                .as_number()?;
            let label = args
                .get("label")
                .map(|v| v.as_string().map(|s| s.to_owned()))
                .transpose()?
                .unwrap_or_else(|| "push_pull".to_owned());

            Ok(Value::Command(ApiCommand::PushPull {
                label,
                face,
                distance,
            }))
        }
        "fillet" => {
            let edges_val = args
                .get("edges")
                .ok_or_else(|| ScriptError::Eval("fillet() requires `edges`".to_owned()))?;
            let edges = match edges_val {
                Value::Selector(s) => vec![s.clone()],
                Value::Array(arr) => arr
                    .iter()
                    .map(|v| v.as_selector())
                    .collect::<Result<Vec<_>, _>>()?,
                other => {
                    return Err(ScriptError::Eval(format!(
                        "Expected edge selector or array of selectors, got {other:?}"
                    )));
                }
            };
            let radius = args
                .get("radius")
                .ok_or_else(|| ScriptError::Eval("fillet() requires `radius`".to_owned()))?
                .as_number()?;
            let label = args
                .get("label")
                .map(|v| v.as_string().map(|s| s.to_owned()))
                .transpose()?
                .unwrap_or_else(|| "fillet".to_owned());

            Ok(Value::Command(ApiCommand::Fillet {
                label,
                edges,
                radius,
            }))
        }
        "chamfer" => {
            let edges_val = args
                .get("edges")
                .ok_or_else(|| ScriptError::Eval("chamfer() requires `edges`".to_owned()))?;
            let edges = match edges_val {
                Value::Selector(s) => vec![s.clone()],
                Value::Array(arr) => arr
                    .iter()
                    .map(|v| v.as_selector())
                    .collect::<Result<Vec<_>, _>>()?,
                other => {
                    return Err(ScriptError::Eval(format!(
                        "Expected edge selector or array of selectors, got {other:?}"
                    )));
                }
            };
            let distance = args
                .get("distance")
                .ok_or_else(|| ScriptError::Eval("chamfer() requires `distance`".to_owned()))?
                .as_number()?;
            let label = args
                .get("label")
                .map(|v| v.as_string().map(|s| s.to_owned()))
                .transpose()?
                .unwrap_or_else(|| "chamfer".to_owned());

            Ok(Value::Command(ApiCommand::Chamfer {
                label,
                edges,
                distance,
            }))
        }
        "faces" => {
            let spec = if !positional_args.is_empty() {
                eval_expr(&positional_args[0], env)?.as_string()?.to_owned()
            } else {
                return Err(ScriptError::Eval(
                    "faces() requires a selector string, e.g. \">Z\"".to_owned(),
                ));
            };

            let sel = match spec.as_str() {
                ">Z" => GeometricSelector::FaceByNormal {
                    direction: Vector3::new(0.0, 0.0, 1.0),
                    match_kind: NormalMatch::Closest,
                },
                "<Z" => GeometricSelector::FaceByNormal {
                    direction: Vector3::new(0.0, 0.0, -1.0),
                    match_kind: NormalMatch::Closest,
                },
                ">Y" => GeometricSelector::FaceByNormal {
                    direction: Vector3::new(0.0, 1.0, 0.0),
                    match_kind: NormalMatch::Closest,
                },
                "<Y" => GeometricSelector::FaceByNormal {
                    direction: Vector3::new(0.0, -1.0, 0.0),
                    match_kind: NormalMatch::Closest,
                },
                ">X" => GeometricSelector::FaceByNormal {
                    direction: Vector3::new(1.0, 0.0, 0.0),
                    match_kind: NormalMatch::Closest,
                },
                "<X" => GeometricSelector::FaceByNormal {
                    direction: Vector3::new(-1.0, 0.0, 0.0),
                    match_kind: NormalMatch::Closest,
                },
                "largest" => GeometricSelector::ByExtremum {
                    metric: Metric::Area,
                    extremum: Extremum::Maximum,
                    kind: EntityKind::Face,
                },
                _ => {
                    return Err(ScriptError::Eval(format!(
                        "Unknown face selector string: `{spec}`"
                    )));
                }
            };
            Ok(Value::Selector(EntitySelector::ByGeometry(sel)))
        }
        "edges" => {
            let spec = if !positional_args.is_empty() {
                eval_expr(&positional_args[0], env)?.as_string()?.to_owned()
            } else {
                return Err(ScriptError::Eval(
                    "edges() requires a selector string, e.g. \"|Z\"".to_owned(),
                ));
            };

            let sel = match spec.as_str() {
                "|Z" => GeometricSelector::ByType {
                    surface_type: SurfaceFilter::Planar,
                    kind: EntityKind::Edge,
                },
                _ => {
                    return Err(ScriptError::Eval(format!(
                        "Unknown edge selector string: `{spec}`"
                    )));
                }
            };
            Ok(Value::Selector(EntitySelector::ByGeometry(sel)))
        }
        other => Err(ScriptError::Eval(format!("Unknown function: `{other}`"))),
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
            return Err(ScriptError::Eval(format!(
                "Method `{method}` called on non-step target: {other:?}"
            )));
        }
    };

    let mut args = BTreeMap::new();
    for (k, v) in named_args {
        args.insert(k.as_str(), eval_expr(v, env)?);
    }

    match method {
        "face" => {
            let role = if !positional_args.is_empty() {
                eval_expr(&positional_args[0], env)?.as_string()?.to_owned()
            } else if let Some(r) = args.get("role") {
                r.as_string()?.to_owned()
            } else {
                "top_face".to_owned()
            };

            let ordinal = args
                .get("ordinal")
                .map(|v| v.as_number().map(|n| n as u32))
                .transpose()?;

            Ok(Value::Selector(EntitySelector::ByHistory {
                from_step: step_label,
                kind: EntityKind::Face,
                role,
                ordinal,
            }))
        }
        "edge" => {
            let role = if !positional_args.is_empty() {
                eval_expr(&positional_args[0], env)?.as_string()?.to_owned()
            } else if let Some(r) = args.get("role") {
                r.as_string()?.to_owned()
            } else {
                "edge".to_owned()
            };

            let ordinal = args
                .get("ordinal")
                .map(|v| v.as_number().map(|n| n as u32))
                .transpose()?;

            Ok(Value::Selector(EntitySelector::ByHistory {
                from_step: step_label,
                kind: EntityKind::Edge,
                role,
                ordinal,
            }))
        }
        "edges" => {
            let role = if !positional_args.is_empty() {
                eval_expr(&positional_args[0], env)?.as_string()?.to_owned()
            } else if let Some(r) = args.get("role") {
                r.as_string()?.to_owned()
            } else {
                "edge".to_owned()
            };

            let edge_selectors = (0..12)
                .map(|i| {
                    Value::Selector(EntitySelector::history_edge_ordinal(
                        step_label.0.clone(),
                        role.clone(),
                        i,
                    ))
                })
                .collect();
            Ok(Value::Array(edge_selectors))
        }
        other => Err(ScriptError::Eval(format!(
            "Unknown method `{other}` on step"
        ))),
    }
}
