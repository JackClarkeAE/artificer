//! Scripting runtime for compiling `.art` CAD scripts into [`ApiCommand`]s.
//!
//! A script is a straight line of feature calls with parameters at the top:
//!
//! ```text
//! param width: f64 [mm] in 20..200 = 60.0 "overall width";
//! let base = box(size: [width, 40, 25], label: "base");
//! drill(face: base.face("top_face"), center: [0, 0], diameter: 14, depth: 25, label: "bore");
//! ```
//!
//! Every builtin below maps onto one API command, so anything the JSON-RPC
//! server can do a script can do: primitives, sketches on a plane or a face
//! with extrusions and revolves, drills, push-pulls, fillets and chamfers,
//! mirrors, patterns, and the three Booleans. Angles are degrees throughout.
//!
//! Reusable geometry lives in functions, which take typed values, faces and
//! bodies, build steps under labels scoped to the call, and return a body
//! with exported faces; modules (`use "file.art";`) hold functions and
//! constants for several scripts to share.

pub mod ast;
pub mod lexer;
pub mod parser;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use artificer_protocol::{EntityKind, Point2, Point3, Vector3};
use serde::{Deserialize, Serialize};

use crate::api::commands::{
    ApiCommand, ExtrudeOp, PatternPlacement, SketchConstraint, SketchEntity, SketchPlane, StepLabel,
};
use crate::api::debug::{ApiError, ApiErrorCode};
use crate::api::scripting::ast::{
    AstNode, BinaryOperator, Expression, FnDecl, TypeSpec, UnaryOperator,
};
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

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

/// One `param` declaration as a customizer sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptParameter {
    pub name: String,
    /// The declared type: `f64`, `int`, `bool` or `str`; `f64` when the
    /// script wrote none.
    pub param_type: String,
    /// The default the script gives it, evaluated with earlier parameters
    /// in scope; `None` when the default is not a number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<f64>,
    /// The default as text, for every type.
    pub default_text: String,
    /// The unit written in brackets after the type, such as `mm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// The `in low..high` range, when the script gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// The description string after the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The line the declaration starts on, one-based.
    pub line: usize,
}

/// Parses a script far enough to list its parameters, without building
/// anything. Defaults that depend on earlier parameters evaluate in order.
pub fn script_parameters(source: &str) -> Result<Vec<ScriptParameter>, ScriptError> {
    let tokens = tokenize(source).map_err(ScriptError::parse)?;
    let mut parser = Parser::new(tokens);
    let ast_nodes = parser.parse_program().map_err(ScriptError::parse)?;
    let overrides = BTreeMap::new();
    let mut interp = Interp::new(&overrides, &NoModules);
    let mut env = prelude();
    let mut parameters = Vec::new();
    for node in ast_nodes {
        if let AstNode::ParamDecl {
            name,
            param_type,
            default_value,
            unit,
            range,
            description,
            line,
        } = node
        {
            let value = interp.eval_expr(&default_value, &env)?;
            let (min, max) = match &range {
                Some((low, high)) => (
                    Some(interp.eval_expr(low, &env)?.as_number()?),
                    Some(interp.eval_expr(high, &env)?.as_number()?),
                ),
                None => (None, None),
            };
            let default = match &value {
                Value::Number(number) => Some(*number),
                Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
                _ => None,
            };
            parameters.push(ScriptParameter {
                name: name.clone(),
                param_type: canonical_param_type(&param_type)?,
                default,
                default_text: value.text(),
                unit,
                min,
                max,
                description,
                line,
            });
            env.insert(name, value);
        }
    }
    Ok(parameters)
}

/// The parameter type as the script may write it, normalised.
fn canonical_param_type(written: &str) -> Result<String, ScriptError> {
    Ok(match written {
        "f64" | "float" | "number" => "f64",
        "int" | "i64" => "int",
        "bool" => "bool",
        "str" | "string" => "str",
        other => {
            return Err(ScriptError::eval(format!(
                "Unknown parameter type `{other}`; a param is f64, int, bool or str"
            )));
        }
    }
    .to_owned())
}

// ---------------------------------------------------------------------------
// Modules
// ---------------------------------------------------------------------------

/// A module's source, with the name that identifies it in cycle chains and
/// messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedModule {
    pub name: String,
    pub source: String,
}

/// How `use "path"` finds a module. The host decides: files under a search
/// path, sources supplied inline over the wire, or nothing at all.
pub trait ModuleResolver {
    /// Loads the module `path` names, as written in the `use`, from the
    /// module `importer` when the `use` sits in one.
    fn load(&self, path: &str, importer: Option<&str>) -> Result<LoadedModule, String>;
}

/// A host that loads no modules: every `use` is an error that says so.
pub struct NoModules;

impl ModuleResolver for NoModules {
    fn load(&self, path: &str, _importer: Option<&str>) -> Result<LoadedModule, String> {
        Err(format!(
            "Cannot load module \"{path}\": this host does not load modules"
        ))
    }
}

/// Modules supplied as sources keyed by the path a `use` writes.
#[derive(Clone, Debug, Default)]
pub struct InlineModules {
    pub modules: BTreeMap<String, String>,
}

impl InlineModules {
    #[must_use]
    pub fn new(modules: BTreeMap<String, String>) -> Self {
        Self { modules }
    }
}

impl ModuleResolver for InlineModules {
    fn load(&self, path: &str, _importer: Option<&str>) -> Result<LoadedModule, String> {
        self.modules
            .get(path)
            .map(|source| LoadedModule {
                name: path.to_owned(),
                source: source.clone(),
            })
            .ok_or_else(|| {
                format!(
                    "Cannot load module \"{path}\": it is not among the modules supplied ({})",
                    if self.modules.is_empty() {
                        "none".to_owned()
                    } else {
                        self.modules.keys().cloned().collect::<Vec<_>>().join(", ")
                    }
                )
            })
    }
}

/// Modules read from files: a path is resolved against the directory of
/// the file that imports it, then against the script's own directory, then
/// along the search path, in that order.
#[derive(Clone, Debug, Default)]
pub struct FileModules {
    /// The directory of the script being compiled, when it came from a file.
    pub base: Option<PathBuf>,
    /// Further directories to look in, in order.
    pub search_path: Vec<PathBuf>,
}

impl FileModules {
    /// Resolves relative to the directory holding `script`.
    #[must_use]
    pub fn beside(script: &Path) -> Self {
        Self {
            base: script.parent().map(Path::to_path_buf),
            search_path: Vec::new(),
        }
    }

    /// Adds a directory to search after the importer's and the script's.
    #[must_use]
    pub fn with_search_path(mut self, directory: impl Into<PathBuf>) -> Self {
        self.search_path.push(directory.into());
        self
    }
}

impl ModuleResolver for FileModules {
    fn load(&self, path: &str, importer: Option<&str>) -> Result<LoadedModule, String> {
        let requested = Path::new(path);
        let mut candidates: Vec<PathBuf> = Vec::new();
        if requested.is_absolute() {
            candidates.push(requested.to_path_buf());
        } else {
            if let Some(parent) = importer.and_then(|importer| Path::new(importer).parent()) {
                candidates.push(parent.join(requested));
            }
            if let Some(base) = &self.base {
                candidates.push(base.join(requested));
            }
            for directory in &self.search_path {
                candidates.push(directory.join(requested));
            }
            if candidates.is_empty() {
                candidates.push(requested.to_path_buf());
            }
        }
        for candidate in &candidates {
            if candidate.is_file() {
                let source = std::fs::read_to_string(candidate).map_err(|error| {
                    format!("Cannot read module \"{}\": {error}", candidate.display())
                })?;
                let name = std::fs::canonicalize(candidate)
                    .unwrap_or_else(|_| candidate.clone())
                    .display()
                    .to_string();
                return Ok(LoadedModule { name, source });
            }
        }
        Err(format!(
            "Cannot find module \"{path}\"; looked in {}",
            candidates
                .iter()
                .map(|candidate| candidate.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

// ---------------------------------------------------------------------------
// Compiling
// ---------------------------------------------------------------------------

/// Evaluates a `.art` script with optional parameter overrides, returning
/// its commands. Modules are not loaded; see [`compile_program_with`].
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
    /// Every top-level `let name = <selector>` in script order, and every
    /// face a top-level `let` received from a function, as `name.face`. A
    /// host resolves each against the finished body to show the user which
    /// face or edge the script calls by that name.
    pub names: Vec<(String, EntitySelector)>,
    /// Every numeric `param` with the value it took in this run: the
    /// override when one was given, the evaluated default otherwise.
    pub parameters: BTreeMap<String, f64>,
}

/// The most loop iterations one script may run in total, so a runaway range
/// is an error rather than a session that never returns.
pub const MAX_LOOP_ITERATIONS: usize = 10_000;

/// The deepest chain of function calls a script may make. Recursion is
/// refused outright; this bounds long chains of helpers calling helpers.
pub const MAX_CALL_DEPTH: usize = 32;

/// Evaluates a `.art` script with optional parameter overrides, returning
/// its commands and the selector names it bound. Modules are not loaded.
pub fn compile_program(
    source: &str,
    param_overrides: &BTreeMap<String, f64>,
) -> Result<ScriptProgram, ScriptError> {
    compile_program_with(source, param_overrides, &NoModules)
}

/// Evaluates a `.art` script, loading the modules its `use` lines name
/// through `modules`.
pub fn compile_program_with(
    source: &str,
    param_overrides: &BTreeMap<String, f64>,
    modules: &dyn ModuleResolver,
) -> Result<ScriptProgram, ScriptError> {
    let tokens = tokenize(source).map_err(ScriptError::parse)?;
    let mut parser = Parser::new(tokens);
    let ast_nodes = parser.parse_program().map_err(ScriptError::parse)?;

    let mut interp = Interp::new(param_overrides, modules);
    let mut env = prelude();
    interp.run_block(&ast_nodes, &mut env, Scope::TopLevel)?;
    Ok(interp.program)
}

/// The names every script starts with.
fn prelude() -> BTreeMap<String, Value> {
    let mut env = BTreeMap::new();
    env.insert("pi".to_owned(), Value::Number(std::f64::consts::PI));
    env
}

/// The functions the language provides; a script cannot redefine them.
const BUILTINS: &[&str] = &[
    "box",
    "cylinder",
    "line",
    "circle",
    "arc",
    "rect",
    "sketch",
    "extrude",
    "revolve",
    "drill",
    "push_pull",
    "fillet",
    "chamfer",
    "mirror",
    "pattern",
    "shell",
    "union",
    "difference",
    "intersection",
    "faces",
    "edges",
    "edge_between",
    "nearest",
    "sqrt",
    "abs",
    "floor",
    "ceil",
    "round",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "atan2",
    "pow",
    "hypot",
    "min",
    "max",
    "clamp",
];

type Env = BTreeMap<String, Value>;

/// Where a block runs: the script itself, a module's top level, or the
/// body of a function or loop.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    TopLevel,
    Module,
    Body,
}

/// How a block ended.
enum Flow {
    Next,
    Return(Value),
}

/// The evaluator's state across one compilation: the program being built,
/// the functions declared so far, the label scope of the current call, and
/// the modules being loaded.
struct Interp<'a> {
    overrides: &'a BTreeMap<String, f64>,
    modules: &'a dyn ModuleResolver,
    program: ScriptProgram,
    budget: usize,
    /// Every user function by name, with the module that declared it.
    functions: BTreeMap<String, (Rc<FnDecl>, String)>,
    /// The top-level names as of the last completed statement: what a
    /// function body sees beside its own parameters.
    globals: Env,
    /// The label prefix of the call being run, innermost last.
    scopes: Vec<String>,
    /// The functions being run, outermost first, for recursion refusals.
    call_stack: Vec<String>,
    /// How many times each function has been called, for unlabelled scopes.
    call_counts: BTreeMap<String, usize>,
    /// The modules being loaded, outermost first, for cycle refusals.
    loading: Vec<String>,
    /// Modules already imported; a second `use` of one is a no-op.
    loaded: BTreeSet<String>,
}

impl<'a> Interp<'a> {
    fn new(overrides: &'a BTreeMap<String, f64>, modules: &'a dyn ModuleResolver) -> Self {
        Self {
            overrides,
            modules,
            program: ScriptProgram {
                commands: Vec::new(),
                names: Vec::new(),
                parameters: BTreeMap::new(),
            },
            budget: MAX_LOOP_ITERATIONS,
            functions: BTreeMap::new(),
            globals: prelude(),
            scopes: Vec::new(),
            call_stack: Vec::new(),
            call_counts: BTreeMap::new(),
            loading: Vec::new(),
            loaded: BTreeSet::new(),
        }
    }

    /// The label a step gets inside the current call: the call's label,
    /// a slash, and the label the step wrote, unless the step already
    /// wrote the call's label in.
    fn scoped_label(&self, raw: &str) -> String {
        scoped(self.scopes.last().map(String::as_str), raw)
    }

    fn run_block(
        &mut self,
        nodes: &[AstNode],
        env: &mut Env,
        scope: Scope,
    ) -> Result<Flow, ScriptError> {
        for node in nodes {
            let flow = self.run_node(node, env, scope)?;
            if scope == Scope::TopLevel {
                self.globals = env.clone();
            }
            if let Flow::Return(value) = flow {
                return Ok(Flow::Return(value));
            }
        }
        Ok(Flow::Next)
    }

    fn run_node(
        &mut self,
        node: &AstNode,
        env: &mut Env,
        scope: Scope,
    ) -> Result<Flow, ScriptError> {
        match node {
            AstNode::ParamDecl {
                name,
                param_type,
                default_value,
                unit: _,
                range,
                description: _,
                line,
            } => {
                if scope == Scope::Body {
                    return Err(ScriptError::Eval {
                        message:
                            "A `param` is declared at the top of the script, not inside a loop or a function"
                                .to_owned(),
                        location: Some((*line, 1)),
                    });
                }
                let value = self
                    .param_value(name, param_type, default_value, range.as_ref(), env)
                    .map_err(|error| error.at(*line, 1))?;
                match &value {
                    Value::Number(number) => {
                        self.program.parameters.insert(name.clone(), *number);
                    }
                    Value::Bool(flag) => {
                        self.program
                            .parameters
                            .insert(name.clone(), f64::from(u8::from(*flag)));
                    }
                    _ => {}
                }
                env.insert(name.clone(), value);
            }
            AstNode::LetBinding { name, value } => {
                let evaluated = self.eval_expr(value, env)?;
                match &evaluated {
                    Value::Command(cmd) => {
                        if scope == Scope::Module {
                            return Err(module_builds_nothing(cmd.label()));
                        }
                        self.program.commands.push(cmd.clone());
                        env.insert(name.clone(), Value::Step(StepLabel(cmd.label().to_owned())));
                    }
                    Value::Selector(selector) => {
                        if scope == Scope::TopLevel {
                            self.program.names.retain(|(existing, _)| existing != name);
                            self.program.names.push((name.clone(), selector.clone()));
                        }
                        env.insert(name.clone(), evaluated);
                    }
                    Value::Body { faces, .. } => {
                        if scope == Scope::TopLevel {
                            for (face, selector) in faces {
                                let full = format!("{name}.{face}");
                                self.program.names.retain(|(existing, _)| existing != &full);
                                self.program.names.push((full, selector.clone()));
                            }
                        }
                        env.insert(name.clone(), evaluated);
                    }
                    _ => {
                        env.insert(name.clone(), evaluated);
                    }
                }
            }
            AstNode::Statement(expr) => {
                let evaluated = self.eval_expr(expr, env)?;
                if let Value::Command(cmd) = evaluated {
                    if scope == Scope::Module {
                        return Err(module_builds_nothing(cmd.label()));
                    }
                    self.program.commands.push(cmd);
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
                if scope == Scope::Module {
                    return Err(at(ScriptError::eval(
                        "A module builds nothing at its top level; put the loop in a function",
                    )));
                }
                let start = self
                    .eval_expr(start, env)
                    .map_err(at)?
                    .as_number()
                    .map_err(at)?;
                let end = self
                    .eval_expr(end, env)
                    .map_err(at)?
                    .as_number()
                    .map_err(at)?;
                if start.fract() != 0.0 || end.fract() != 0.0 {
                    return Err(at(ScriptError::eval(format!(
                        "A `for` range counts whole numbers; got {start}..{end}"
                    ))));
                }
                let mut index = start;
                while index < end {
                    if self.budget == 0 {
                        return Err(at(ScriptError::eval(format!(
                            "The script runs more than {MAX_LOOP_ITERATIONS} loop iterations"
                        ))));
                    }
                    self.budget -= 1;
                    env.insert(variable.clone(), Value::Number(index));
                    if let Flow::Return(value) = self.run_block(body, env, Scope::Body)? {
                        return Ok(Flow::Return(value));
                    }
                    index += 1.0;
                }
            }
            AstNode::FnDecl(decl) => {
                let at = |error: ScriptError| error.at(decl.line, decl.col);
                if scope == Scope::Body {
                    return Err(at(ScriptError::eval(format!(
                        "Declare fn {} at the top level, not inside a loop or another function",
                        decl.name
                    ))));
                }
                let module = self
                    .loading
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "the script".to_owned());
                self.declare_function(decl, module).map_err(at)?;
            }
            AstNode::Return {
                value,
                faces,
                line,
                col,
            } => {
                let at = |error: ScriptError| error.at(*line, *col);
                if scope != Scope::Body || self.call_stack.is_empty() {
                    return Err(at(ScriptError::eval("`return` belongs inside a function")));
                }
                let mut returned = match value {
                    Some(expression) => self.eval_expr(expression, env)?,
                    None => Value::Unit,
                };
                if !faces.is_empty() {
                    let step = returned.as_step().map_err(|_| {
                        at(ScriptError::eval(
                            "`with faces` exports faces of a body; return a step or a body before it",
                        ))
                    })?;
                    let mut exported = BTreeMap::new();
                    for (name, expression) in faces {
                        let selector =
                            self.eval_expr(expression, env)?
                                .as_selector()
                                .map_err(|error| {
                                    at(ScriptError::eval(format!(
                                        "exported face `{name}`: {}",
                                        error.message()
                                    )))
                                })?;
                        exported.insert(name.clone(), selector);
                    }
                    // A body returned from an inner function keeps the
                    // faces it already exports, under the new ones.
                    if let Value::Body { faces: inner, .. } = &returned {
                        for (name, selector) in inner {
                            exported
                                .entry(name.clone())
                                .or_insert_with(|| selector.clone());
                        }
                    }
                    returned = Value::Body {
                        step,
                        faces: exported,
                    };
                }
                return Ok(Flow::Return(returned));
            }
            AstNode::Use { path, line, col } => {
                let at = |error: ScriptError| error.at(*line, *col);
                if scope == Scope::Body {
                    return Err(at(ScriptError::eval(
                        "`use` belongs at the top of the script, not inside a loop or a function",
                    )));
                }
                let importer = self.loading.last().cloned();
                let constants = self.import(path, importer.as_deref()).map_err(at)?;
                for (name, value) in constants {
                    env.entry(name).or_insert(value);
                }
            }
        }
        Ok(Flow::Next)
    }

    /// The value a `param` takes: the override when one was given, else
    /// the default; checked against the declared type and range.
    fn param_value(
        &mut self,
        name: &str,
        param_type: &str,
        default_value: &Expression,
        range: Option<&(Expression, Expression)>,
        env: &Env,
    ) -> Result<Value, ScriptError> {
        let param_type = canonical_param_type(param_type)?;
        let value = match self.overrides.get(name) {
            Some(&override_value) => match param_type.as_str() {
                "f64" => Value::Number(override_value),
                "int" => {
                    if override_value.fract() != 0.0 {
                        return Err(ScriptError::eval(format!(
                            "Parameter `{name}` is an int; the override {override_value} is not a whole number"
                        )));
                    }
                    Value::Number(override_value)
                }
                "bool" => Value::Bool(override_value != 0.0),
                _ => {
                    return Err(ScriptError::eval(format!(
                        "Parameter `{name}` is a string; set it in the script, not by override"
                    )));
                }
            },
            None => self.eval_expr(default_value, env)?,
        };
        let expected = match param_type.as_str() {
            "f64" => TypeSpec::Number,
            "int" => TypeSpec::Int,
            "bool" => TypeSpec::Bool,
            _ => TypeSpec::Str,
        };
        if !type_matches(&value, &expected) {
            return Err(ScriptError::eval(format!(
                "Parameter `{name}` is declared {}, but its value is {}",
                expected.describe(),
                value.describe()
            )));
        }
        if let Some((low, high)) = range {
            let low = self.eval_expr(low, env)?.as_number()?;
            let high = self.eval_expr(high, env)?.as_number()?;
            let number = value.as_number().map_err(|_| {
                ScriptError::eval(format!(
                    "Parameter `{name}` has a range, so it must be a number"
                ))
            })?;
            if number < low || number > high {
                return Err(ScriptError::eval(format!(
                    "Parameter `{name}` is {number}, outside its range {low}..{high}"
                )));
            }
        }
        Ok(value)
    }

    fn declare_function(&mut self, decl: &FnDecl, module: String) -> Result<(), ScriptError> {
        if BUILTINS.contains(&decl.name.as_str()) {
            return Err(ScriptError::eval(format!(
                "`{}` is a built-in function and cannot be redefined",
                decl.name
            )));
        }
        if let Some((_, existing)) = self.functions.get(&decl.name) {
            return Err(ScriptError::eval(format!(
                "fn {} is already defined by {existing}",
                decl.name
            )));
        }
        let mut seen = BTreeSet::new();
        for param in &decl.params {
            if !seen.insert(&param.name) {
                return Err(ScriptError::eval(format!(
                    "fn {} declares parameter `{}` twice",
                    decl.name, param.name
                )));
            }
        }
        self.functions
            .insert(decl.name.clone(), (Rc::new(decl.clone()), module));
        Ok(())
    }

    /// Loads a module and everything it imports, declaring its functions
    /// and returning its constants.
    fn import(&mut self, path: &str, importer: Option<&str>) -> Result<Env, ScriptError> {
        let module = self
            .modules
            .load(path, importer)
            .map_err(ScriptError::eval)?;
        if self.loading.iter().any(|name| name == &module.name) {
            let mut chain = self.loading.clone();
            chain.push(module.name.clone());
            return Err(ScriptError::eval(format!(
                "Import cycle: {}",
                chain.join(" -> ")
            )));
        }
        if self.loaded.contains(&module.name) {
            return Ok(Env::new());
        }
        let tokens = tokenize(&module.source).map_err(|message| {
            ScriptError::eval(format!("In module {}: {message}", module.name))
        })?;
        let nodes = Parser::new(tokens).parse_program().map_err(|message| {
            ScriptError::eval(format!("In module {}: {message}", module.name))
        })?;
        self.loading.push(module.name.clone());
        let mut env = self.globals.clone();
        let before: BTreeSet<String> = env.keys().cloned().collect();
        let result = self.run_block(&nodes, &mut env, Scope::Module);
        self.loading.pop();
        result.map_err(|error| {
            let location = error.location().map_or(String::new(), |(line, col)| {
                format!(" at line {line}, column {col}")
            });
            ScriptError::eval(format!(
                "In module {}{location}: {}",
                module.name,
                error.message()
            ))
        })?;
        self.loaded.insert(module.name);
        let constants: Env = env
            .into_iter()
            .filter(|(name, value)| {
                !before.contains(name) && !matches!(value, Value::Command(_) | Value::Step(_))
            })
            .collect();
        self.globals.extend(constants.clone());
        Ok(constants)
    }

    fn eval_expr(&mut self, expr: &Expression, env: &Env) -> Result<Value, ScriptError> {
        match expr {
            Expression::Number(n) => Ok(Value::Number(*n)),
            Expression::Bool(flag) => Ok(Value::Bool(*flag)),
            Expression::String(s) => Ok(Value::String(s.clone())),
            Expression::Identifier { name, line, col } => {
                env.get(name).cloned().ok_or_else(|| ScriptError::Eval {
                    message: format!(
                        "Undefined identifier `{name}`{}",
                        if self.functions.contains_key(name) {
                            "; it is a function, call it with ( )"
                        } else {
                            ""
                        }
                    ),
                    location: Some((*line, *col)),
                })
            }
            Expression::Array(elements) => {
                let mut arr = Vec::new();
                for el in elements {
                    arr.push(self.eval_expr(el, env)?);
                }
                Ok(Value::Array(arr))
            }
            Expression::UnaryOp { op, operand } => {
                let val = self.eval_expr(operand, env)?.as_number()?;
                match op {
                    UnaryOperator::Neg => Ok(Value::Number(-val)),
                }
            }
            Expression::BinaryOp { left, op, right } => {
                let left = self.eval_expr(left, env)?;
                let right = self.eval_expr(right, env)?;
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
            } => self
                .eval_function_call(name, named_args, positional_args, env, *line, *col)
                .map_err(|error| error.at(*line, *col)),
            Expression::MethodCall {
                target,
                method,
                named_args,
                positional_args,
                line,
                col,
            } => self
                .eval_method_call(target, method, named_args, positional_args, env)
                .map_err(|error| error.at(*line, *col)),
            Expression::Index {
                target,
                index,
                line,
                col,
            } => {
                let at = |error: ScriptError| error.at(*line, *col);
                let target = self.eval_expr(target, env)?;
                let index = self.eval_expr(index, env)?.as_number().map_err(at)?;
                let Value::Array(items) = target else {
                    return Err(at(ScriptError::eval(format!(
                        "Only an array can be indexed, got {}",
                        target.describe()
                    ))));
                };
                if index.fract() != 0.0 || index < 0.0 || index as usize >= items.len() {
                    return Err(at(ScriptError::eval(format!(
                        "Index {} is outside the array of {} items",
                        number_text(index),
                        items.len()
                    ))));
                }
                Ok(items[index as usize].clone())
            }
        }
    }

    fn eval_function_call(
        &mut self,
        name: &str,
        named_args: &[(String, Expression)],
        positional_args: &[Expression],
        env: &Env,
        line: usize,
        col: usize,
    ) -> Result<Value, ScriptError> {
        if let Some(value) = self.math_call(name, positional_args, env)? {
            return Ok(value);
        }
        if let Some((decl, _)) = self.functions.get(name).cloned() {
            return self.call_user_function(&decl, named_args, positional_args, env, line, col);
        }
        self.eval_builtin(name, named_args, positional_args, env)
    }

    /// Runs a user function: binds and checks its arguments, scopes the
    /// labels of the steps it builds to the call, and returns what it
    /// returns.
    fn call_user_function(
        &mut self,
        decl: &Rc<FnDecl>,
        named_args: &[(String, Expression)],
        positional_args: &[Expression],
        env: &Env,
        line: usize,
        col: usize,
    ) -> Result<Value, ScriptError> {
        let name = &decl.name;
        if self.call_stack.iter().any(|active| active == name) {
            let mut chain = self.call_stack.clone();
            chain.push(name.clone());
            return Err(ScriptError::eval(format!(
                "Recursion is not supported: {}",
                chain.join(" -> ")
            )));
        }
        if self.call_stack.len() >= MAX_CALL_DEPTH {
            return Err(ScriptError::eval(format!(
                "Function calls nested deeper than {MAX_CALL_DEPTH} levels"
            )));
        }

        // Bind the arguments: positional ones in declaration order, named
        // ones by name, each exactly once.
        let mut bound: BTreeMap<String, Value> = BTreeMap::new();
        if positional_args.len() > decl.params.len() {
            return Err(ScriptError::eval(format!(
                "{name}() takes {} argument{}, got {} positional",
                decl.params.len(),
                if decl.params.len() == 1 { "" } else { "s" },
                positional_args.len()
            )));
        }
        for (param, expression) in decl.params.iter().zip(positional_args) {
            bound.insert(param.name.clone(), self.eval_expr(expression, env)?);
        }
        for (arg_name, expression) in named_args {
            if !decl.params.iter().any(|param| &param.name == arg_name) {
                return Err(ScriptError::eval(format!(
                    "{name}() has no argument `{arg_name}`; its arguments are {}",
                    decl.params
                        .iter()
                        .map(|param| param.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            if bound.contains_key(arg_name) {
                return Err(ScriptError::eval(format!(
                    "{name}(): argument `{arg_name}` is given twice"
                )));
            }
            bound.insert(arg_name.clone(), self.eval_expr(expression, env)?);
        }
        let mut callee_env = self.globals.clone();
        for param in &decl.params {
            let value = match bound.remove(&param.name) {
                Some(value) => value,
                None => match &param.default {
                    Some(default) => self.eval_expr(default, &callee_env)?,
                    None => {
                        return Err(ScriptError::eval(format!(
                            "{name}() requires `{}`",
                            param.name
                        )));
                    }
                },
            };
            if !type_matches(&value, &param.param_type) {
                return Err(ScriptError::eval(format!(
                    "{name}(): `{}` expects {}, got {}",
                    param.name,
                    param.param_type.describe(),
                    value.describe()
                )));
            }
            callee_env.insert(param.name.clone(), value);
        }

        // The call's label scopes every step the body builds: the `label`
        // argument when the function has one, else the function's name and
        // its call count.
        let count = self.call_counts.entry(name.clone()).or_insert(0);
        *count += 1;
        let raw_scope = match callee_env.get("label") {
            Some(Value::String(label)) => label.clone(),
            _ => format!("{name}_{count}"),
        };
        let scope = self.scoped_label(&raw_scope);
        self.scopes.push(scope);
        self.call_stack.push(name.clone());
        let result = self.run_block(&decl.body, &mut callee_env, Scope::Body);
        self.call_stack.pop();
        self.scopes.pop();

        let returned = match result? {
            Flow::Return(value) => value,
            Flow::Next => Value::Unit,
        };
        if let Some(return_type) = &decl.return_type
            && !type_matches(&returned, return_type)
        {
            return Err(ScriptError::Eval {
                message: format!(
                    "fn {name} is declared to return {}, but returned {}",
                    return_type.describe(),
                    returned.describe()
                ),
                location: Some((line, col)),
            });
        }
        Ok(returned)
    }

    fn math_call(
        &mut self,
        name: &str,
        positional_args: &[Expression],
        env: &Env,
    ) -> Result<Option<Value>, ScriptError> {
        if !matches!(
            name,
            "sqrt"
                | "abs"
                | "floor"
                | "ceil"
                | "round"
                | "sin"
                | "cos"
                | "tan"
                | "asin"
                | "acos"
                | "atan"
                | "atan2"
                | "pow"
                | "hypot"
                | "min"
                | "max"
                | "clamp"
        ) {
            return Ok(None);
        }
        let mut numbers = Vec::new();
        for expression in positional_args {
            numbers.push(self.eval_expr(expression, env)?.as_number()?);
        }
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
                let value = one(&numbers)?;
                if value < 0.0 {
                    return Err(ScriptError::eval("sqrt() of a negative number"));
                }
                value.sqrt()
            }
            "abs" => one(&numbers)?.abs(),
            "floor" => one(&numbers)?.floor(),
            "ceil" => one(&numbers)?.ceil(),
            "round" => one(&numbers)?.round(),
            "sin" => one(&numbers)?.to_radians().sin(),
            "cos" => one(&numbers)?.to_radians().cos(),
            "tan" => one(&numbers)?.to_radians().tan(),
            "asin" => one(&numbers)?.asin().to_degrees(),
            "acos" => one(&numbers)?.acos().to_degrees(),
            "atan" => one(&numbers)?.atan().to_degrees(),
            "atan2" => {
                let (y, x) = two(&numbers)?;
                y.atan2(x).to_degrees()
            }
            "pow" => {
                let (base, exponent) = two(&numbers)?;
                base.powf(exponent)
            }
            "hypot" => {
                let (a, b) = two(&numbers)?;
                a.hypot(b)
            }
            "min" | "max" => {
                if numbers.is_empty() {
                    return Err(ScriptError::eval(format!(
                        "{name}() takes at least one number"
                    )));
                }
                numbers.iter().copied().fold(
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
            "clamp" => match numbers.as_slice() {
                [value, low, high] => value.clamp(*low, *high),
                _ => return Err(ScriptError::eval("clamp() takes a value, a low and a high")),
            },
            _ => unreachable!("matched above"),
        };
        if !value.is_finite() {
            return Err(ScriptError::eval(format!(
                "{name}() did not produce a finite number"
            )));
        }
        Ok(Some(Value::Number(value)))
    }

    fn eval_builtin(
        &mut self,
        name: &str,
        named_args: &[(String, Expression)],
        positional_args: &[Expression],
        env: &Env,
    ) -> Result<Value, ScriptError> {
        let args = Args::new(name, named_args, env, self)?;
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
            "shell" => Ok(Value::Command(ApiCommand::Shell {
                label: args.label()?,
                open: match args.values.get("open") {
                    None => Vec::new(),
                    Some(open) => open.as_selectors()?,
                },
                wall: args.number("wall")?,
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
                // With `step:` the pattern replays one feature at each
                // placement; without it, it copies the whole body.
                if let Some(step) = args.values.get("step") {
                    let step = step.as_step()?;
                    let placement = if let Some(axis) = args.values.get("axis") {
                        PatternPlacement::Circular {
                            axis_origin: args.point3_or("axis_origin", origin)?,
                            axis_direction: axis.as_vector3()?,
                            count: count as u16,
                            angle_step_degrees: args.number_or("angle", 0.0)?,
                        }
                    } else if args.values.contains_key("direction") {
                        PatternPlacement::Linear {
                            direction: args.required("direction")?.as_vector3()?,
                            spacing: args.number("spacing")?,
                            count: count as u16,
                        }
                    } else {
                        return Err(ScriptError::eval(
                            "pattern(step: ...) takes `axis:` (with `axis_origin:` and `angle:`) for a circular array, or `direction:` and `spacing:` for a row",
                        ));
                    };
                    return Ok(Value::Command(ApiCommand::FeaturePattern {
                        label: args.label()?,
                        step,
                        placement,
                    }));
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
                if positional_args.is_empty() {
                    return named_selector(EntityKind::Face, &args).map(Value::Selector);
                }
                let spec = self.positional_string("faces", positional_args, env, "\">Z\"")?;
                Ok(Value::Selector(face_selector(&spec)?))
            }
            "edges" => {
                if positional_args.is_empty() {
                    return named_selector(EntityKind::Edge, &args).map(Value::Selector);
                }
                let spec = self.positional_string("edges", positional_args, env, "\"|Z\"")?;
                Ok(Value::Selector(edge_selector(&spec)?))
            }
            "edge_between" => Ok(Value::Selector(EntitySelector::ByGeometry {
                selector: GeometricSelector::EdgeBetween {
                    face_a: Box::new(args.required("a")?.as_selector()?),
                    face_b: Box::new(args.required("b")?.as_selector()?),
                },
            })),
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
                "Unknown function `{other}`; the features are box, cylinder, sketch, extrude, revolve, drill, push_pull, fillet, chamfer, mirror, pattern, union, difference and intersection{}",
                if self.functions.is_empty() {
                    String::new()
                } else {
                    format!(
                        ", and the script defines {}",
                        self.functions
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            ))),
        }
    }

    /// The one positional argument a selector call takes.
    fn positional_string(
        &mut self,
        name: &str,
        positional_args: &[Expression],
        env: &Env,
        example: &str,
    ) -> Result<String, ScriptError> {
        match positional_args.first() {
            Some(expression) => Ok(self.eval_expr(expression, env)?.as_string()?.to_owned()),
            None => Err(ScriptError::eval(format!(
                "{name}() requires a selector string, e.g. {example}"
            ))),
        }
    }

    fn eval_method_call(
        &mut self,
        target_expr: &Expression,
        method: &str,
        named_args: &[(String, Expression)],
        positional_args: &[Expression],
        env: &Env,
    ) -> Result<Value, ScriptError> {
        let target = self.eval_expr(target_expr, env)?;
        let (step_label, exported) = match target {
            Value::Step(s) => (s, BTreeMap::new()),
            Value::Command(cmd) => (StepLabel(cmd.label().to_owned()), BTreeMap::new()),
            Value::Body { step, faces } => (step, faces),
            other => {
                return Err(ScriptError::eval(format!(
                    "`.{method}` is used on a step or a body, got {}",
                    other.describe()
                )));
            }
        };
        // `body.top` is the face the function exported as `top`.
        if named_args.is_empty()
            && positional_args.is_empty()
            && let Some(selector) = exported.get(method)
        {
            return Ok(Value::Selector(selector.clone()));
        }
        let args = Args::new(method, named_args, env, self)?;
        let role = |interp: &mut Self, default: &str| -> Result<String, ScriptError> {
            if let Some(expression) = positional_args.first() {
                Ok(interp.eval_expr(expression, env)?.as_string()?.to_owned())
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
            "face" => {
                let role = role(self, "top_face")?;
                // An exported face by name first; a history role otherwise.
                if let Some(selector) = exported.get(&role) {
                    return Ok(Value::Selector(selector.clone()));
                }
                Ok(Value::Selector(EntitySelector::ByHistory {
                    from_step: step_label,
                    kind: EntityKind::Face,
                    role,
                    ordinal,
                }))
            }
            "edge" => Ok(Value::Selector(EntitySelector::ByHistory {
                from_step: step_label,
                kind: EntityKind::Edge,
                role: role(self, "edge")?,
                ordinal,
            })),
            "edges" => {
                // Every edge the step produced under the role, by ordinal; the
                // session ignores ordinals the step never made.
                let role = role(self, "edge")?;
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
            other => {
                let exports = if exported.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; the body exports {}",
                        exported.keys().cloned().collect::<Vec<_>>().join(", ")
                    )
                };
                Err(ScriptError::eval(format!(
                    "Unknown method `.{other}` on a step; use .face(\"role\"), .edge(\"role\") or .edges(\"role\"){exports}"
                )))
            }
        }
    }
}

fn module_builds_nothing(label: &str) -> ScriptError {
    ScriptError::eval(format!(
        "A module builds nothing at its top level; put the step \"{label}\" in a function"
    ))
}

/// A step label under a call's label prefix. A label that already begins
/// with the prefix is left alone, so `label + "_boss"` inside a function
/// with a `label` argument does not double up.
fn scoped(prefix: Option<&str>, raw: &str) -> String {
    match prefix {
        None => raw.to_owned(),
        Some(prefix) if raw.starts_with(prefix) => raw.to_owned(),
        // The step that carries the call's own label is the call's step:
        // `label: label` inside `fn boss(label: str)` names `.../boss`,
        // not `.../boss/boss`.
        Some(prefix) if prefix == raw || prefix.ends_with(&format!("/{raw}")) => prefix.to_owned(),
        Some(prefix) => format!("{prefix}/{raw}"),
    }
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

/// The kind of entity a selector names, when the selector says.
fn selector_kind(selector: &EntitySelector) -> Option<EntityKind> {
    match selector {
        EntitySelector::ByHistory { kind, .. } => Some(*kind),
        EntitySelector::Direct { entity_ref } => Some(entity_ref.kind),
        EntitySelector::ByGeometry { selector } => match selector {
            GeometricSelector::FaceByNormal { .. } => Some(EntityKind::Face),
            GeometricSelector::NearestTo { kind, .. }
            | GeometricSelector::ByType { kind, .. }
            | GeometricSelector::ByExtremum { kind, .. } => Some(*kind),
            GeometricSelector::EdgeBetween { .. } | GeometricSelector::EdgesParallelTo { .. } => {
                Some(EntityKind::Edge)
            }
        },
    }
}

/// Whether a value is of the declared type.
fn type_matches(value: &Value, expected: &TypeSpec) -> bool {
    match expected {
        TypeSpec::Any => true,
        TypeSpec::Number => matches!(value, Value::Number(_)),
        TypeSpec::Int => matches!(value, Value::Number(number) if number.fract() == 0.0),
        TypeSpec::Str => matches!(value, Value::String(_)),
        TypeSpec::Bool => matches!(value, Value::Bool(_)),
        TypeSpec::Face => match value {
            Value::Selector(selector) => selector_kind(selector) != Some(EntityKind::Edge),
            _ => false,
        },
        TypeSpec::Edge => match value {
            Value::Selector(selector) => selector_kind(selector) != Some(EntityKind::Face),
            Value::Array(items) => items.iter().all(|item| type_matches(item, expected)),
            _ => false,
        },
        TypeSpec::Body => matches!(
            value,
            Value::Step(_) | Value::Command(_) | Value::Body { .. }
        ),
        TypeSpec::Array(element, length) => match value {
            Value::Array(items) => {
                length.is_none_or(|length| length == items.len())
                    && items.iter().all(|item| type_matches(item, element))
            }
            _ => false,
        },
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Number(f64),
    Bool(bool),
    String(String),
    Array(Vec<Value>),
    Selector(EntitySelector),
    Step(StepLabel),
    Command(ApiCommand),
    /// A sketch entity awaiting a `sketch(...)` call to gather it.
    Entity(SketchEntity),
    /// What a function returns with `with faces`: a step and the faces it
    /// exports by name.
    Body {
        step: StepLabel,
        faces: BTreeMap<String, EntitySelector>,
    },
    /// What a function without a `return` value evaluates to.
    Unit,
}

impl Value {
    fn describe(&self) -> String {
        match self {
            Self::Number(number) => format!("the number {number}"),
            Self::Bool(flag) => format!("the boolean {flag}"),
            Self::String(text) => format!("the string \"{text}\""),
            Self::Array(items) => format!("an array of {} items", items.len()),
            Self::Selector(_) => "an entity selector".to_owned(),
            Self::Step(label) => format!("the step \"{label}\""),
            Self::Command(command) => format!("the step \"{}\"", command.label()),
            Self::Entity(_) => "a sketch entity".to_owned(),
            Self::Body { step, faces } => format!(
                "the body from step \"{step}\" exporting {}",
                if faces.is_empty() {
                    "no faces".to_owned()
                } else {
                    faces.keys().cloned().collect::<Vec<_>>().join(", ")
                }
            ),
            Self::Unit => "nothing".to_owned(),
        }
    }

    /// The value as text, for a parameter listing.
    fn text(&self) -> String {
        match self {
            Self::Number(number) => number_text(*number),
            Self::Bool(flag) => flag.to_string(),
            Self::String(text) => text.clone(),
            other => other.describe(),
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
            Self::Body { step, .. } => Ok(step.clone()),
            Self::String(label) if !label.is_empty() => Ok(StepLabel(label.clone())),
            other => Err(ScriptError::eval(format!(
                "Expected a step (a `let` bound to a feature call, or its label as a string) or a body, got {}",
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
    /// The label prefix of the call being run, applied to `label`.
    scope: Option<String>,
}

impl<'a> Args<'a> {
    fn new(
        call: &'a str,
        named_args: &'a [(String, Expression)],
        env: &Env,
        interp: &mut Interp<'_>,
    ) -> Result<Self, ScriptError> {
        let mut values = BTreeMap::new();
        for (key, expression) in named_args {
            values.insert(key.as_str(), interp.eval_expr(expression, env)?);
        }
        Ok(Self {
            call,
            values,
            scope: interp.scopes.last().cloned(),
        })
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

    /// The step's label, scoped to the call it is built in.
    fn label(&self) -> Result<String, ScriptError> {
        let raw = self.values.get("label").map_or_else(
            || Ok(self.call.to_owned()),
            |value| value.as_string().map(str::to_owned),
        )?;
        Ok(scoped(self.scope.as_deref(), &raw))
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

/// The named forms of `faces(...)` and `edges(...)`, which reach every
/// geometric selector the API has: `direction:` with an optional `match:`
/// for faces by normal or edges by direction, and `metric:` with
/// `extremum:` for the largest or smallest.
fn named_selector(kind: EntityKind, args: &Args<'_>) -> Result<EntitySelector, ScriptError> {
    let call = if kind == EntityKind::Face {
        "faces"
    } else {
        "edges"
    };
    if let Some(direction) = args.values.get("direction") {
        let direction = direction.as_vector3()?;
        let selector = if kind == EntityKind::Face {
            let match_kind = match args.values.get("match") {
                None => NormalMatch::Closest,
                Some(value) => match value.as_string()? {
                    "closest" => NormalMatch::Closest,
                    "farthest" => NormalMatch::Farthest,
                    "parallel" => NormalMatch::Parallel,
                    "perpendicular" => NormalMatch::Perpendicular,
                    other => {
                        return Err(ScriptError::eval(format!(
                            "faces(): `match` is \"closest\", \"farthest\", \"parallel\" or \"perpendicular\", not \"{other}\""
                        )));
                    }
                },
            };
            GeometricSelector::FaceByNormal {
                direction,
                match_kind,
            }
        } else {
            GeometricSelector::EdgesParallelTo { direction }
        };
        return Ok(EntitySelector::ByGeometry { selector });
    }
    if let Some(metric) = args.values.get("metric") {
        let metric = match metric.as_string()? {
            "area" => Metric::Area,
            "length" => Metric::Length,
            "radius" => Metric::Radius,
            other => {
                return Err(ScriptError::eval(format!(
                    "{call}(): `metric` is \"area\", \"length\" or \"radius\", not \"{other}\""
                )));
            }
        };
        let extremum = match args.required("extremum")?.as_string()? {
            "max" | "largest" | "longest" => Extremum::Maximum,
            "min" | "smallest" | "shortest" => Extremum::Minimum,
            other => {
                return Err(ScriptError::eval(format!(
                    "{call}(): `extremum` is \"max\" or \"min\", not \"{other}\""
                )));
            }
        };
        return Ok(EntitySelector::ByGeometry {
            selector: GeometricSelector::ByExtremum {
                metric,
                extremum,
                kind,
            },
        });
    }
    Err(ScriptError::eval(format!(
        "{call}() takes a selector string, or `direction:` with `match:`, or `metric:` with `extremum:`"
    )))
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
        "spherical" => GeometricSelector::ByType {
            surface_type: SurfaceFilter::Spherical,
            kind: EntityKind::Face,
        },
        "conical" => GeometricSelector::ByType {
            surface_type: SurfaceFilter::Conical,
            kind: EntityKind::Face,
        },
        "toroidal" => GeometricSelector::ByType {
            surface_type: SurfaceFilter::Toroidal,
            kind: EntityKind::Face,
        },
        _ => {
            return Err(ScriptError::eval(format!(
                "Unknown face selector `{spec}`; use >X <X >Y <Y >Z <Z, top/bottom/front/back/left/right, largest, smallest, planar, cylindrical, spherical, conical or toroidal"
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
        Value::Selector(selector) => Ok(SketchPlane::OnFace {
            face: selector.clone(),
        }),
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
