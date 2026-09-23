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
//! mirrors, patterns, and the three Booleans. Angles are degrees throughout;
//! an arc also takes its ends in radians, as `start_radians` and
//! `end_radians`, which is how a decompiled script writes an angle that no
//! number of degrees converts to exactly.
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

use artificer_protocol::{EntityKind, PlanarFrame3, Point2, Point3, Vector3};
use serde::{Deserialize, Serialize};

use crate::api::commands::{
    ApiCommand, AxisPlacement, ExtrudeOp, PatternPlacement, SketchConstraint, SketchEntity,
    SketchPlane, StepLabel,
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
    let mut env = Env::over(Rc::new(prelude()));
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

/// The longest chain of modules importing one another a script may open:
/// the script's `use` counts as the first link. A library is a few modules
/// deep; a chain past this is a runaway or an attack on the stack.
pub const MAX_IMPORT_DEPTH: usize = 16;

/// The most modules one compilation may load, however they are reached.
pub const MAX_LOADED_MODULES: usize = 256;

/// The longest string one value may hold, in bytes. Labels are words, not
/// documents; the limit keeps `let s = s + s` in a loop from doubling until
/// memory runs out.
pub const MAX_STRING_BYTES: usize = 1 << 20;

/// The most elements one array value may hold.
pub const MAX_ARRAY_ELEMENTS: usize = 100_000;

/// The most edge selectors `edges(count:)` will spell out at once. A step
/// makes a handful of edges under one role, not thousands.
pub const MAX_EDGE_SELECTORS: usize = 4096;

/// How deeply array values may nest. A literal is bounded by the parser's
/// nesting limit, but an array wrapped in an array through a variable, a
/// level per statement or per loop iteration, is bounded by nothing else;
/// past this, a value would be too deep to check or even to drop.
pub const MAX_ARRAY_DEPTH: usize = 32;

/// The deepest the evaluator goes. Every expression inside another, every
/// block inside another and every function body inside the call that runs
/// it is one level. The parser bounds each construct on its own, but a
/// chain of functions each nesting expressions multiplies those bounds;
/// this one bounds them together, which is what keeps a compilation inside
/// a thread's default stack wherever it runs. At this depth the evaluator
/// takes about half a megabyte of stack at worst in an optimised build and
/// under a megabyte and a half in an unoptimised one, so it fits the two
/// megabytes a spawned thread gets and the one a Windows main thread does.
/// The evaluator's own functions are kept small for the same reason.
pub const MAX_EVALUATION_DEPTH: usize = 256;

/// The most work one compilation may do, in steps. Every expression
/// evaluated and every block run is a step; so is every item, at every
/// level, of an array a call receives or `edges(count:)` makes, and every
/// byte of text a call receives or a literal or a join makes. The loop,
/// call-depth and size limits each allow their pieces; this bounds what
/// the pieces multiply to, such as a function calling the next ten times
/// a level, or one large array handed to every iteration of a loop.
pub const MAX_EVALUATION_STEPS: usize = 10_000_000;

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
    let mut env = Env::over(Rc::clone(&interp.globals));
    interp.run_block(&ast_nodes, &mut env, Scope::TopLevel)?;
    let mut program = interp.program;
    keep_last_binding(&mut program.names);
    Ok(program)
}

/// Keeps, for every name bound more than once, only its last binding, in
/// the order of those last bindings: a rebinding replaces the name rather
/// than listing it twice. Done once at the end rather than at every `let`,
/// which would cost the whole list per binding.
fn keep_last_binding(names: &mut Vec<(String, EntitySelector)>) {
    let mut last = BTreeMap::new();
    for (index, (name, _)) in names.iter().enumerate() {
        last.insert(name.clone(), index);
    }
    let mut index = 0;
    names.retain(|(name, _)| {
        let keep = last.get(name) == Some(&index);
        index += 1;
        keep
    });
}

/// The names every script starts with.
fn prelude() -> Names {
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
    "spline",
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

/// Names and the values they hold.
type Names = BTreeMap<String, Value>;

/// The names a block sees: its own, and beneath them the script's top-level
/// names, shared between every block that sees them rather than copied into
/// each. A function body or a module sees the top-level names as of the
/// last completed statement; at the top level itself, `local` holds only
/// what the statement being run has bound, and it is folded into `shared`
/// when the statement completes.
#[derive(Clone, Debug)]
struct Env {
    /// Names bound in this block: a function's parameters and lets, a
    /// module's constants, or the top-level names the current statement
    /// has bound so far.
    local: Names,
    /// The names beneath.
    shared: Rc<Names>,
}

impl Env {
    /// A block with no names of its own over `shared`.
    fn over(shared: Rc<Names>) -> Self {
        Self {
            local: Names::new(),
            shared,
        }
    }

    fn get(&self, name: &str) -> Option<&Value> {
        self.local.get(name).or_else(|| self.shared.get(name))
    }

    fn insert(&mut self, name: String, value: Value) {
        self.local.insert(name, value);
    }
}

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
    /// How many levels deep evaluation is: see [`MAX_EVALUATION_DEPTH`].
    depth: usize,
    /// The steps left to take: see [`MAX_EVALUATION_STEPS`].
    steps: usize,
    /// Every user function by name, with the module that declared it.
    functions: BTreeMap<String, (Rc<FnDecl>, String)>,
    /// The top-level names as of the last completed statement: what a
    /// function body sees beside its own parameters.
    globals: Rc<Names>,
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
            depth: 0,
            steps: MAX_EVALUATION_STEPS,
            functions: BTreeMap::new(),
            globals: Rc::new(prelude()),
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

    /// Takes `steps` from the work budget, refusing once it is spent.
    fn charge(&mut self, steps: usize) -> Result<(), ScriptError> {
        if let Some(left) = self.steps.checked_sub(steps) {
            self.steps = left;
            return Ok(());
        }
        self.steps = 0;
        Err(ScriptError::eval(format!(
            "The script takes more than {MAX_EVALUATION_STEPS} steps to evaluate; a step is an expression evaluated, a block run, or an item or byte of text a call receives or a join builds"
        )))
    }

    /// Goes one level deeper, refusing past [`MAX_EVALUATION_DEPTH`], and
    /// pays the step. Every `descend` is matched by `self.depth -= 1` when
    /// the level is left, whether it succeeded or not.
    fn descend(&mut self) -> Result<(), ScriptError> {
        if self.depth >= MAX_EVALUATION_DEPTH {
            return Err(ScriptError::eval(format!(
                "The script nests expressions, blocks and function calls more than {MAX_EVALUATION_DEPTH} levels deep"
            )));
        }
        self.charge(1)?;
        self.depth += 1;
        Ok(())
    }

    fn run_block(
        &mut self,
        nodes: &[AstNode],
        env: &mut Env,
        scope: Scope,
    ) -> Result<Flow, ScriptError> {
        self.descend()?;
        let mut flow = Ok(Flow::Next);
        for node in nodes {
            flow = self.run_node(node, env, scope);
            if scope == Scope::TopLevel && flow.is_ok() {
                self.commit(env);
            }
            if !matches!(flow, Ok(Flow::Next)) {
                break;
            }
        }
        self.depth -= 1;
        flow
    }

    /// Folds what a completed top-level statement bound into the shared
    /// names, which function bodies see from here on. The names are shared
    /// by nothing else between statements, so once the interpreter lets go
    /// of its own handle they take the new bindings in place: a statement
    /// costs what it bound, not the size of everything bound before it.
    fn commit(&mut self, env: &mut Env) {
        self.globals = Rc::default();
        if !env.local.is_empty() {
            let shared = Rc::make_mut(&mut env.shared);
            for (name, value) in std::mem::take(&mut env.local) {
                shared.insert(name, value);
            }
        }
        self.globals = Rc::clone(&env.shared);
    }

    // Most functions from here to `build_builtin` recurse into one another
    // as deep as a script nests. Each does one thing and hands the rest to a
    // function of its own, so the frames on that path stay small even in an
    // unoptimised build, where every local of a function has its own slot.

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
            } => self.run_param(
                name,
                param_type,
                default_value,
                range.as_ref(),
                *line,
                env,
                scope,
            ),
            AstNode::LetBinding { name, value } => self.run_let(name, value, env, scope),
            AstNode::Statement(expr) => self.run_statement(expr, env, scope),
            AstNode::For {
                variable,
                start,
                end,
                body,
                line,
                col,
            } => self.run_for(variable, start, end, body, (*line, *col), env, scope),
            AstNode::FnDecl(decl) => self.run_fn_decl(decl, scope),
            AstNode::Return {
                value,
                faces,
                line,
                col,
            } => self.run_return(value.as_ref(), faces, *line, *col, env, scope),
            AstNode::Use { path, line, col } => self.run_use(path, (*line, *col), env, scope),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_param(
        &mut self,
        name: &str,
        param_type: &str,
        default_value: &Expression,
        range: Option<&(Expression, Expression)>,
        line: usize,
        env: &mut Env,
        scope: Scope,
    ) -> Result<Flow, ScriptError> {
        if scope == Scope::Body {
            return Err(ScriptError::Eval {
                message:
                    "A `param` is declared at the top of the script, not inside a loop or a function"
                        .to_owned(),
                location: Some((line, 1)),
            });
        }
        let value = self
            .param_value(name, param_type, default_value, range, env)
            .map_err(|error| error.at(line, 1))?;
        match &value {
            Value::Number(number) => {
                self.program.parameters.insert(name.to_owned(), *number);
            }
            Value::Bool(flag) => {
                self.program
                    .parameters
                    .insert(name.to_owned(), f64::from(u8::from(*flag)));
            }
            _ => {}
        }
        env.insert(name.to_owned(), value);
        Ok(Flow::Next)
    }

    fn run_let(
        &mut self,
        name: &str,
        value: &Expression,
        env: &mut Env,
        scope: Scope,
    ) -> Result<Flow, ScriptError> {
        let evaluated = self.eval_expr(value, env)?;
        self.bind(name, evaluated, env, scope)?;
        Ok(Flow::Next)
    }

    /// Binds a `let`: a feature call becomes a step of the program and the
    /// name its label; a selector or a body's exported faces become names
    /// a host can show.
    fn bind(
        &mut self,
        name: &str,
        evaluated: Value,
        env: &mut Env,
        scope: Scope,
    ) -> Result<(), ScriptError> {
        match evaluated {
            Value::Command(cmd) => {
                if scope == Scope::Module {
                    return Err(module_builds_nothing(cmd.label()));
                }
                let step = Value::Step(StepLabel(cmd.label().to_owned()));
                self.program.commands.push(Rc::unwrap_or_clone(cmd));
                env.insert(name.to_owned(), step);
            }
            Value::Selector(selector) => {
                if scope == Scope::TopLevel {
                    self.program.names.push((name.to_owned(), selector.clone()));
                }
                env.insert(name.to_owned(), Value::Selector(selector));
            }
            Value::Body { step, faces } => {
                if scope == Scope::TopLevel {
                    for (face, selector) in faces.iter() {
                        self.program
                            .names
                            .push((format!("{name}.{face}"), selector.clone()));
                    }
                }
                env.insert(name.to_owned(), Value::Body { step, faces });
            }
            other => {
                env.insert(name.to_owned(), other);
            }
        }
        Ok(())
    }

    fn run_statement(
        &mut self,
        expr: &Expression,
        env: &Env,
        scope: Scope,
    ) -> Result<Flow, ScriptError> {
        let evaluated = self.eval_expr(expr, env)?;
        if let Value::Command(cmd) = evaluated {
            if scope == Scope::Module {
                return Err(module_builds_nothing(cmd.label()));
            }
            self.program.commands.push(Rc::unwrap_or_clone(cmd));
        }
        Ok(Flow::Next)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_for(
        &mut self,
        variable: &str,
        start: &Expression,
        end: &Expression,
        body: &[AstNode],
        (line, col): (usize, usize),
        env: &mut Env,
        scope: Scope,
    ) -> Result<Flow, ScriptError> {
        let (mut index, end) = self
            .loop_range(start, end, env, scope)
            .map_err(|error| error.at(line, col))?;
        while index < end {
            self.take_iteration().map_err(|error| error.at(line, col))?;
            env.insert(variable.to_owned(), Value::Number(index));
            if let Flow::Return(value) = self.run_block(body, env, Scope::Body)? {
                return Ok(Flow::Return(value));
            }
            index += 1.0;
        }
        Ok(Flow::Next)
    }

    /// The whole numbers a `for` counts through, as `start..end`.
    fn loop_range(
        &mut self,
        start: &Expression,
        end: &Expression,
        env: &Env,
        scope: Scope,
    ) -> Result<(f64, f64), ScriptError> {
        if scope == Scope::Module {
            return Err(ScriptError::eval(
                "A module builds nothing at its top level; put the loop in a function",
            ));
        }
        let start = self.eval_expr(start, env)?.as_number()?;
        let end = self.eval_expr(end, env)?.as_number()?;
        if start.fract() != 0.0 || end.fract() != 0.0 {
            return Err(ScriptError::eval(format!(
                "A `for` range counts whole numbers; got {start}..{end}"
            )));
        }
        Ok((start, end))
    }

    /// Takes one loop iteration from the script's allowance.
    fn take_iteration(&mut self) -> Result<(), ScriptError> {
        if self.budget == 0 {
            return Err(ScriptError::eval(format!(
                "The script runs more than {MAX_LOOP_ITERATIONS} loop iterations"
            )));
        }
        self.budget -= 1;
        Ok(())
    }

    fn run_fn_decl(&mut self, decl: &FnDecl, scope: Scope) -> Result<Flow, ScriptError> {
        if scope == Scope::Body {
            return Err(ScriptError::eval(format!(
                "Declare fn {} at the top level, not inside a loop or another function",
                decl.name
            ))
            .at(decl.line, decl.col));
        }
        let module = self
            .loading
            .last()
            .cloned()
            .unwrap_or_else(|| "the script".to_owned());
        self.declare_function(decl, module)
            .map_err(|error| error.at(decl.line, decl.col))?;
        Ok(Flow::Next)
    }

    fn run_return(
        &mut self,
        value: Option<&Expression>,
        faces: &[(String, Expression)],
        line: usize,
        col: usize,
        env: &Env,
        scope: Scope,
    ) -> Result<Flow, ScriptError> {
        let at = |error: ScriptError| error.at(line, col);
        if scope != Scope::Body || self.call_stack.is_empty() {
            return Err(at(ScriptError::eval("`return` belongs inside a function")));
        }
        let returned = match value {
            Some(expression) => self.eval_expr(expression, env)?,
            None => Value::Unit,
        };
        if faces.is_empty() {
            return Ok(Flow::Return(returned));
        }
        let step = returned.as_step().map_err(|_| {
            at(ScriptError::eval(
                "`with faces` exports faces of a body; return a step or a body before it",
            ))
        })?;
        // A feature call returned with faces is built here. The body it
        // becomes is no longer a command, so the caller that binds it would
        // not build it, and every later step naming the body would reach
        // for a step that was never made.
        if let Value::Command(command) = &returned {
            self.program.commands.push((**command).clone());
        }
        let mut exported = BTreeMap::new();
        for (name, expression) in faces {
            let selector = self
                .eval_expr(expression, env)?
                .as_selector()
                .map_err(|error| {
                    at(ScriptError::eval(format!(
                        "exported face `{name}`: {}",
                        error.message()
                    )))
                })?;
            exported.insert(name.clone(), selector);
        }
        // A body returned from an inner function keeps the faces it already
        // exports, under the new ones.
        if let Value::Body { faces: inner, .. } = &returned {
            for (name, selector) in inner.iter() {
                exported
                    .entry(name.clone())
                    .or_insert_with(|| selector.clone());
            }
        }
        Ok(Flow::Return(Value::Body {
            step,
            faces: Rc::new(exported),
        }))
    }

    fn run_use(
        &mut self,
        path: &str,
        (line, col): (usize, usize),
        env: &mut Env,
        scope: Scope,
    ) -> Result<Flow, ScriptError> {
        let at = |error: ScriptError| error.at(line, col);
        if scope == Scope::Body {
            return Err(at(ScriptError::eval(
                "`use` belongs at the top of the script, not inside a loop or a function",
            )));
        }
        let importer = self.loading.last().cloned();
        let constants = self.import(path, importer.as_deref()).map_err(at)?;
        for (name, value) in constants {
            if env.get(&name).is_none() {
                env.insert(name, value);
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
            // An override comes from outside the script, where nothing
            // has checked it: NaN passes every comparison a range makes
            // and infinity is no dimension, so neither is taken.
            Some(&override_value) if !override_value.is_finite() => {
                return Err(ScriptError::eval(format!(
                    "Parameter `{name}`: the override {override_value} is not a finite number"
                )));
            }
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
            // Written as "not inside" rather than "below or above", so a
            // value no comparison holds for is outside rather than in.
            if !(low..=high).contains(&number) {
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
    fn import(&mut self, path: &str, importer: Option<&str>) -> Result<Names, ScriptError> {
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
            return Ok(Names::new());
        }
        if self.loading.len() >= MAX_IMPORT_DEPTH {
            let mut chain = self.loading.clone();
            chain.push(module.name.clone());
            return Err(ScriptError::eval(format!(
                "Modules import one another more than {MAX_IMPORT_DEPTH} deep: {}",
                chain.join(" -> ")
            )));
        }
        if self.loaded.len() >= MAX_LOADED_MODULES {
            return Err(ScriptError::eval(format!(
                "Loading module {} would exceed the {MAX_LOADED_MODULES} modules one script may load",
                module.name
            )));
        }
        let tokens = tokenize(&module.source).map_err(|message| {
            ScriptError::eval(format!("In module {}: {message}", module.name))
        })?;
        let nodes = Parser::new(tokens).parse_program().map_err(|message| {
            ScriptError::eval(format!("In module {}: {message}", module.name))
        })?;
        self.loading.push(module.name.clone());
        let mut env = Env::over(Rc::clone(&self.globals));
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
        // The module's constants are the names it bound that were not
        // already names when it started; a module's own `let pi` shadows
        // the script's inside the module and goes no further.
        let Env { local, shared } = env;
        let constants: Names = local
            .into_iter()
            .filter(|(name, value)| {
                !shared.contains_key(name) && !matches!(value, Value::Command(_) | Value::Step(_))
            })
            .collect();
        drop(shared);
        if !constants.is_empty() {
            if Rc::strong_count(&self.globals) > 1 {
                self.charge(self.globals.len())?;
            }
            let globals = Rc::make_mut(&mut self.globals);
            for (name, value) in &constants {
                globals.insert(name.clone(), value.clone());
            }
        }
        Ok(constants)
    }

    fn eval_expr(&mut self, expr: &Expression, env: &Env) -> Result<Value, ScriptError> {
        self.descend()?;
        let result = self.eval_expr_here(expr, env);
        self.depth -= 1;
        result
    }

    fn eval_expr_here(&mut self, expr: &Expression, env: &Env) -> Result<Value, ScriptError> {
        match expr {
            Expression::Number(n) => Ok(Value::Number(*n)),
            Expression::Bool(flag) => Ok(Value::Bool(*flag)),
            Expression::String(text) => self.eval_string(text),
            Expression::Identifier { name, line, col } => {
                self.eval_identifier(name, *line, *col, env)
            }
            Expression::Array(elements) => self.eval_array(elements, env),
            Expression::UnaryOp { op, operand } => self.eval_unary(*op, operand, env),
            Expression::BinaryOp { left, op, right } => self.eval_binary(left, *op, right, env),
            Expression::FunctionCall {
                name,
                named_args,
                positional_args,
                line,
                col,
            } => self.eval_function_call(name, named_args, positional_args, env, *line, *col),
            Expression::MethodCall {
                target,
                method,
                named_args,
                positional_args,
                line,
                col,
            } => self.eval_method_call(
                target,
                method,
                named_args,
                positional_args,
                env,
                *line,
                *col,
            ),
            Expression::Index {
                target,
                index,
                line,
                col,
            } => self.eval_index(target, index, *line, *col, env),
        }
    }

    fn eval_string(&mut self, text: &str) -> Result<Value, ScriptError> {
        self.charge(text.len())?;
        Ok(Value::String(Rc::from(text)))
    }

    fn eval_identifier(
        &self,
        name: &str,
        line: usize,
        col: usize,
        env: &Env,
    ) -> Result<Value, ScriptError> {
        if let Some(value) = env.get(name) {
            return Ok(value.clone());
        }
        Err(ScriptError::Eval {
            message: format!(
                "Undefined identifier `{name}`{}",
                if self.functions.contains_key(name) {
                    "; it is a function, call it with ( )"
                } else {
                    ""
                }
            ),
            location: Some((line, col)),
        })
    }

    fn eval_array(&mut self, elements: &[Expression], env: &Env) -> Result<Value, ScriptError> {
        if elements.len() > MAX_ARRAY_ELEMENTS {
            return Err(ScriptError::eval(format!(
                "An array may hold at most {MAX_ARRAY_ELEMENTS} elements; this one has {}",
                elements.len()
            )));
        }
        let mut values = Vec::with_capacity(elements.len());
        for element in elements {
            values.push(self.eval_expr(element, env)?);
        }
        Value::array(values)
    }

    fn eval_unary(
        &mut self,
        op: UnaryOperator,
        operand: &Expression,
        env: &Env,
    ) -> Result<Value, ScriptError> {
        let value = self.eval_expr(operand, env)?.as_number()?;
        match op {
            UnaryOperator::Neg => Ok(Value::Number(-value)),
        }
    }

    fn eval_binary(
        &mut self,
        left: &Expression,
        op: BinaryOperator,
        right: &Expression,
        env: &Env,
    ) -> Result<Value, ScriptError> {
        let left = self.eval_expr(left, env)?;
        let right = self.eval_expr(right, env)?;
        self.binary(&left, op, &right)
    }

    fn eval_index(
        &mut self,
        target: &Expression,
        index: &Expression,
        line: usize,
        col: usize,
        env: &Env,
    ) -> Result<Value, ScriptError> {
        let target = self.eval_expr(target, env)?;
        let index = self.eval_expr(index, env)?;
        index_into(&target, &index).map_err(|error| error.at(line, col))
    }

    /// `left op right`, both already evaluated.
    fn binary(
        &mut self,
        left: &Value,
        op: BinaryOperator,
        right: &Value,
    ) -> Result<Value, ScriptError> {
        // `+` joins text: a string with a string or a number, either way
        // round, which is how a loop builds its labels.
        if op == BinaryOperator::Add
            && matches!(left, Value::String(_)) | matches!(right, Value::String(_))
        {
            let text = |value: &Value| -> Result<String, ScriptError> {
                match value {
                    Value::String(text) => Ok(text.to_string()),
                    Value::Number(number) => Ok(number_text(*number)),
                    other => Err(ScriptError::eval(format!(
                        "`+` joins strings and numbers, got {}",
                        other.describe()
                    ))),
                }
            };
            let (left, right) = (text(left)?, text(right)?);
            // Checked before the join, so a string that has already
            // doubled itself to the limit is refused rather than built one
            // more time.
            if left.len() + right.len() > MAX_STRING_BYTES {
                return Err(ScriptError::eval(format!(
                    "A string may hold at most {MAX_STRING_BYTES} bytes; joining these makes {}",
                    left.len() + right.len()
                )));
            }
            self.charge(left.len() + right.len())?;
            return Ok(Value::String(Rc::from(format!("{left}{right}"))));
        }
        let l = left.as_number()?;
        let r = right.as_number()?;
        let (res, symbol) = match op {
            BinaryOperator::Add => (l + r, "+"),
            BinaryOperator::Sub => (l - r, "-"),
            BinaryOperator::Mul => (l * r, "*"),
            BinaryOperator::Div => {
                if r.abs() < 1e-12 {
                    return Err(ScriptError::eval("Division by zero"));
                }
                (l / r, "/")
            }
        };
        // Every number a script holds is finite, so only an overflow can
        // leave one that is not; it would reach a step as a dimension no
        // body has, and a journal that cannot be written.
        if !res.is_finite() {
            return Err(ScriptError::eval(format!(
                "`{symbol}` overflows: {l:e} {symbol} {r:e} is not a finite number"
            )));
        }
        Ok(Value::Number(res))
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
        let result = if MATH_FUNCTIONS.contains(&name) {
            self.eval_math(name, positional_args, env)
        } else if let Some(decl) = self.functions.get(name).map(|(decl, _)| Rc::clone(decl)) {
            self.call_user_function(&decl, named_args, positional_args, env, line, col)
        } else {
            self.eval_builtin(name, named_args, positional_args, env)
        };
        result.map_err(|error| error.at(line, col))
    }

    fn eval_math(
        &mut self,
        name: &str,
        positional_args: &[Expression],
        env: &Env,
    ) -> Result<Value, ScriptError> {
        let mut numbers = Vec::with_capacity(positional_args.len());
        for expression in positional_args {
            numbers.push(self.eval_expr(expression, env)?.as_number()?);
        }
        math(name, &numbers).map(Value::Number)
    }

    /// Evaluates everything a builtin reads before it builds anything, so
    /// the frame that assembles the command, which is large, is never on
    /// the stack while another expression is being evaluated.
    fn eval_builtin(
        &mut self,
        name: &str,
        named_args: &[(String, Expression)],
        positional_args: &[Expression],
        env: &Env,
    ) -> Result<Value, ScriptError> {
        let args = Args::new(name, named_args, env, self)?;
        // A selector spelling is the one positional argument a builtin
        // takes.
        let spelling = match positional_args.first() {
            Some(expression) if matches!(name, "faces" | "edges") => {
                Some(self.eval_argument(expression, env)?)
            }
            _ => None,
        };
        self.build_builtin(name, &args, spelling.as_ref())
    }

    /// An argument a call receives, paid for by its weight: what checking
    /// it against a type or copying it into a command costs.
    fn eval_argument(&mut self, expression: &Expression, env: &Env) -> Result<Value, ScriptError> {
        let value = self.eval_expr(expression, env)?;
        self.charge(value.weight())?;
        Ok(value)
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
        self.check_call(decl)?;
        let mut callee_env = self.bind_arguments(decl, named_args, positional_args, env)?;

        // The call's label scopes every step the body builds: the `label`
        // argument when the function has one, else the function's name and
        // its call count.
        self.enter_call(decl, &callee_env);
        let result = self.run_block(&decl.body, &mut callee_env, Scope::Body);
        self.call_stack.pop();
        self.scopes.pop();

        let returned = match result? {
            Flow::Return(value) => value,
            Flow::Next => Value::Unit,
        };
        self.check_return(decl, &returned, line, col)?;
        Ok(returned)
    }

    /// Refuses a call that would recurse or nest too deep.
    fn check_call(&self, decl: &FnDecl) -> Result<(), ScriptError> {
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
        Ok(())
    }

    /// The names a function body starts with: its parameters, bound
    /// positionally in declaration order or by name, each exactly once,
    /// defaults filling the rest, every one checked against its type.
    fn bind_arguments(
        &mut self,
        decl: &FnDecl,
        named_args: &[(String, Expression)],
        positional_args: &[Expression],
        env: &Env,
    ) -> Result<Env, ScriptError> {
        let name = &decl.name;
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
            let value = self.eval_argument(expression, env)?;
            bound.insert(param.name.clone(), value);
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
            let value = self.eval_argument(expression, env)?;
            bound.insert(arg_name.clone(), value);
        }
        let mut callee_env = Env::over(Rc::clone(&self.globals));
        for param in &decl.params {
            let value = match bound.remove(&param.name) {
                Some(value) => value,
                None => match &param.default {
                    Some(default) => self.eval_argument(default, &callee_env)?,
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
        Ok(callee_env)
    }

    /// Opens the label scope of a call and marks the function as running.
    fn enter_call(&mut self, decl: &FnDecl, callee_env: &Env) {
        let name = &decl.name;
        let count = self.call_counts.entry(name.clone()).or_insert(0);
        *count += 1;
        let raw_scope = match callee_env.get("label") {
            Some(Value::String(label)) => label.to_string(),
            _ => format!("{name}_{count}"),
        };
        let scope = self.scoped_label(&raw_scope);
        self.scopes.push(scope);
        self.call_stack.push(name.clone());
    }

    /// Checks what a function returned against the type it declares,
    /// paying for the check by the value's weight.
    fn check_return(
        &mut self,
        decl: &FnDecl,
        returned: &Value,
        line: usize,
        col: usize,
    ) -> Result<(), ScriptError> {
        let Some(return_type) = &decl.return_type else {
            return Ok(());
        };
        self.charge(returned.weight())?;
        if type_matches(returned, return_type) {
            return Ok(());
        }
        Err(ScriptError::Eval {
            message: format!(
                "fn {} is declared to return {}, but returned {}",
                decl.name,
                return_type.describe(),
                returned.describe()
            ),
            location: Some((line, col)),
        })
    }

    /// The command, sketch entity, plane, axis or selector a builtin makes
    /// from arguments already evaluated. It evaluates nothing itself.
    #[inline(never)]
    fn build_builtin(
        &self,
        name: &str,
        args: &Args<'_>,
        spelling: Option<&Value>,
    ) -> Result<Value, ScriptError> {
        let origin = Point3::new(0.0, 0.0, 0.0);
        let up = Vector3::new(0.0, 0.0, 1.0);

        match name {
            // ---- primitives -------------------------------------------------
            "box" => {
                let size = args.required("size")?.as_point3()?;
                Ok(Value::command(ApiCommand::MakeBox {
                    label: args.label()?,
                    origin: args.point3_or("origin", origin)?,
                    size: [size.x, size.y, size.z],
                }))
            }
            "cylinder" => Ok(Value::command(ApiCommand::MakeCylinder {
                label: args.label()?,
                center: args.point3_or("center", origin)?,
                axis: args.vector3_or("axis", up)?,
                radius: args.radius()?,
                height: args.number("height")?,
            })),
            // ---- sketches and what grows from them --------------------------
            "line" => Ok(Value::entity(SketchEntity::Line {
                start: args.required("start")?.as_point2()?,
                end: args.required("end")?.as_point2()?,
            })),
            "circle" => Ok(Value::entity(SketchEntity::Circle {
                center: args
                    .values
                    .get("center")
                    .map_or(Ok(Point2::new(0.0, 0.0)), Value::as_point2)?,
                radius: args.radius()?,
            })),
            "arc" => Ok(Value::entity(SketchEntity::Arc {
                center: args
                    .values
                    .get("center")
                    .map_or(Ok(Point2::new(0.0, 0.0)), Value::as_point2)?,
                radius: args.radius()?,
                start_angle: args.arc_angle("start")?,
                end_angle: args.arc_angle("end")?,
            })),
            // A spline through fit points, or by its control points
            // (ADR 0050).
            "spline" => {
                let closed = match args.values.get("closed") {
                    None => false,
                    Some(Value::Bool(flag)) => *flag,
                    Some(other) => {
                        return Err(ScriptError::eval(format!(
                            "spline(): `closed` is true or false, got {}",
                            other.describe()
                        )));
                    }
                };
                let points = |value: &Value| -> Result<Vec<Point2>, ScriptError> {
                    match value {
                        Value::Array(items) => items.iter().map(Value::as_point2).collect(),
                        other => Err(ScriptError::eval(format!(
                            "spline(): points are an array of [x, y] arrays, got {}",
                            other.describe()
                        ))),
                    }
                };
                match (args.values.get("points"), args.values.get("control_points")) {
                    (Some(fit), None) => Ok(Value::entity(SketchEntity::Spline {
                        points: points(fit)?,
                        closed,
                    })),
                    (None, Some(control)) => {
                        let degree = args.number_or("degree", 3.0)?;
                        if !((1.0..=5.0).contains(&degree) && degree.fract() == 0.0) {
                            return Err(ScriptError::eval(
                                "spline(): `degree` is a whole number from 1 to 5",
                            ));
                        }
                        Ok(Value::entity(SketchEntity::ControlSpline {
                            control_points: points(control)?,
                            degree: degree as usize,
                            closed,
                        }))
                    }
                    _ => Err(ScriptError::eval(
                        "spline() takes either `points`, the points it passes through, or \
                         `control_points`, the polygon that shapes it",
                    )),
                }
            }
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
                Ok(Value::entity(SketchEntity::Rectangle {
                    origin,
                    width,
                    height,
                }))
            }
            "sketch" => Ok(Value::command(ApiCommand::Sketch {
                label: args.label()?,
                on: sketch_plane(args.required("on")?)?,
                entities: sketch_entities(args.required("entities")?)?,
                constraints: Vec::<SketchConstraint>::new(),
            })),
            "extrude" => Ok(Value::command(ApiCommand::Extrude {
                label: args.label()?,
                sketch: args.required("sketch")?.as_step()?,
                regions: args.regions()?,
                distance: args.number("distance")?,
                operation: args.operation()?,
                draft_degrees: args.number_or("draft", 0.0)?,
            })),
            "loft" => {
                let sections = match args.required("sections")? {
                    Value::Array(items) => items
                        .iter()
                        .map(Value::as_step)
                        .collect::<Result<Vec<_>, _>>()?,
                    other => {
                        return Err(ScriptError::eval(format!(
                            "loft(): `sections` is an array of sketches, got {}",
                            other.describe()
                        )));
                    }
                };
                Ok(Value::command(ApiCommand::Loft {
                    label: args.label()?,
                    sections,
                    operation: args.operation()?,
                }))
            }
            "plane" => script_plane(args).map(Value::Plane),
            "axis" => script_axis(args).map(Value::Axis),
            "revolve" => {
                // `axis` is a direction through `axis_origin`, or an
                // axis(...) that says where it runs itself.
                let (axis_origin, axis_direction, axis_placement) = match args.values.get("axis") {
                    Some(Value::Axis(axis)) => {
                        if args.values.contains_key("axis_origin") {
                            return Err(ScriptError::eval(
                                "revolve(): an axis(...) says where it runs; leave out `axis_origin`",
                            ));
                        }
                        match axis {
                            ScriptAxis::Line { origin, direction } => (*origin, *direction, None),
                            ScriptAxis::Placed(placement) => (origin, up, Some(placement.clone())),
                        }
                    }
                    _ => (
                        args.point3_or("axis_origin", origin)?,
                        args.vector3_or("axis", up)?,
                        None,
                    ),
                };
                Ok(Value::command(ApiCommand::Revolve {
                    label: args.label()?,
                    sketch: args.required("sketch")?.as_step()?,
                    regions: args.regions()?,
                    axis_origin,
                    axis_direction,
                    angle_degrees: args.number_or("angle", 360.0)?,
                    operation: args.operation()?,
                    axis_placement,
                }))
            }
            // ---- face and edge features ------------------------------------
            "drill" => Ok(Value::command(ApiCommand::DrillHole {
                label: args.label()?,
                face: args.required("face")?.as_selector()?,
                center: args
                    .values
                    .get("center")
                    .map_or(Ok(Point2::new(0.0, 0.0)), Value::as_point2)?,
                diameter: args.radius()? * 2.0,
                depth: args.number("depth")?,
            })),
            "push_pull" => Ok(Value::command(ApiCommand::PushPull {
                label: args.label()?,
                face: args.required("face")?.as_selector()?,
                distance: args.number("distance")?,
            })),
            "fillet" => Ok(Value::command(ApiCommand::Fillet {
                label: args.label()?,
                edges: args.required("edges")?.as_selectors()?,
                radius: args.number("radius")?,
            })),
            "chamfer" => Ok(Value::command(ApiCommand::Chamfer {
                label: args.label()?,
                edges: args.required("edges")?.as_selectors()?,
                distance: args.number("distance")?,
            })),
            "shell" => Ok(Value::command(ApiCommand::Shell {
                label: args.label()?,
                open: match args.values.get("open") {
                    None => Vec::new(),
                    Some(open) => open.as_selectors()?,
                },
                wall: args.number("wall")?,
            })),
            // ---- whole-body operations -------------------------------------
            "mirror" => Ok(Value::command(ApiCommand::Mirror {
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
                    return Ok(Value::command(ApiCommand::FeaturePattern {
                        label: args.label()?,
                        step,
                        placement,
                    }));
                }
                Ok(Value::command(ApiCommand::LinearPattern {
                    label: args.label()?,
                    direction: args.required("direction")?.as_vector3()?,
                    spacing: args.number("spacing")?,
                    count: count as u16,
                }))
            }
            "union" => Ok(Value::command(ApiCommand::BooleanUnion {
                label: args.label()?,
                target: args.required("target")?.as_step()?,
                tool: args.required("tool")?.as_step()?,
            })),
            "difference" => Ok(Value::command(ApiCommand::BooleanDifference {
                label: args.label()?,
                target: args.required("target")?.as_step()?,
                tool: args.required("tool")?.as_step()?,
            })),
            "intersection" => Ok(Value::command(ApiCommand::BooleanIntersection {
                label: args.label()?,
                target: args.required("target")?.as_step()?,
                tool: args.required("tool")?.as_step()?,
            })),
            // ---- selectors --------------------------------------------------
            "faces" => match spelling {
                None => named_selector(EntityKind::Face, args).map(Value::Selector),
                Some(spelling) => Ok(Value::Selector(face_selector(spelling.as_string()?)?)),
            },
            "edges" => match spelling {
                None => named_selector(EntityKind::Edge, args).map(Value::Selector),
                Some(spelling) => Ok(Value::Selector(edge_selector(spelling.as_string()?)?)),
            },
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
                "Unknown function `{other}`; the features are box, cylinder, sketch, plane, extrude, loft, revolve, drill, push_pull, fillet, chamfer, mirror, pattern, union, difference and intersection{}",
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

    #[allow(clippy::too_many_arguments)]
    fn eval_method_call(
        &mut self,
        target_expr: &Expression,
        method: &str,
        named_args: &[(String, Expression)],
        positional_args: &[Expression],
        env: &Env,
        line: usize,
        col: usize,
    ) -> Result<Value, ScriptError> {
        let at = |error: ScriptError| error.at(line, col);
        let target = self.eval_expr(target_expr, env).map_err(at)?;
        let has_args = !named_args.is_empty() || !positional_args.is_empty();
        match method_receiver(target, method, has_args).map_err(at)? {
            Receiver::Done(value) => Ok(value),
            Receiver::Step { step, exported } => self
                .apply_method(&step, &exported, method, named_args, positional_args, env)
                .map_err(at),
        }
    }

    /// A method on a step or a body. As for a builtin, everything is
    /// evaluated before the selector is made: the arguments, then the role
    /// written positionally.
    fn apply_method(
        &mut self,
        step: &StepLabel,
        exported: &BTreeMap<String, EntitySelector>,
        method: &str,
        named_args: &[(String, Expression)],
        positional_args: &[Expression],
        env: &Env,
    ) -> Result<Value, ScriptError> {
        let args = Args::new(method, named_args, env, self)?;
        let role = match positional_args.first() {
            Some(expression) if matches!(method, "face" | "faces" | "edge" | "edges") => {
                Some(self.eval_argument(expression, env)?)
            }
            _ => None,
        };
        let value = build_method(step, exported, method, &args, role.as_ref())?;
        // `edges(count:)` spells out an array of selectors from one number.
        self.charge(value.weight())?;
        Ok(value)
    }
}

/// What a method is called on, once the target is evaluated.
enum Receiver {
    /// The method is already answered: a method on a face selector, or a
    /// face a body exports by the method's name.
    Done(Value),
    /// A step or a body, whose method still has arguments to evaluate.
    Step {
        step: StepLabel,
        exported: Rc<BTreeMap<String, EntitySelector>>,
    },
}

fn method_receiver(target: Value, method: &str, has_args: bool) -> Result<Receiver, ScriptError> {
    let (step, exported) = match target {
        Value::Step(step) => (step, Rc::default()),
        Value::Command(cmd) => (StepLabel(cmd.label().to_owned()), Rc::default()),
        Value::Body { step, faces } => (step, faces),
        Value::Selector(face) => {
            return face_selector_method(face, method, has_args).map(Receiver::Done);
        }
        other => {
            return Err(ScriptError::eval(format!(
                "`.{method}` is used on a step, a body or a face selector, got {}",
                other.describe()
            )));
        }
    };
    // `body.top` is the face the function exported as `top`.
    if !has_args && let Some(selector) = exported.get(method) {
        return Ok(Receiver::Done(Value::Selector(selector.clone())));
    }
    Ok(Receiver::Step { step, exported })
}

/// A method on a face selector: `faces(">Z").edges()` is every edge
/// bounding that face, holes included; `.rim()` is the outer loop alone.
/// Either resolves when the step using it runs.
fn face_selector_method(
    face: EntitySelector,
    method: &str,
    has_args: bool,
) -> Result<Value, ScriptError> {
    if has_args {
        return Err(ScriptError::eval(format!(
            "`.{method}` on a face selector takes no arguments"
        )));
    }
    match method {
        "edges" => Ok(Value::Selector(EntitySelector::edges_of_face(face))),
        "rim" => Ok(Value::Selector(EntitySelector::rim_of_face(face))),
        other => Err(ScriptError::eval(format!(
            "Unknown method `.{other}` on a face selector; use .edges() for every edge of the face or .rim() for its outer loop"
        ))),
    }
}

/// The selector a method on a step or a body makes from arguments already
/// evaluated: `.face(...)`, `.faces()`, `.edge(...)` or `.edges(...)`. It
/// evaluates nothing itself.
#[inline(never)]
fn build_method(
    step_label: &StepLabel,
    exported: &BTreeMap<String, EntitySelector>,
    method: &str,
    args: &Args<'_>,
    positional_role: Option<&Value>,
) -> Result<Value, ScriptError> {
    let role = |default: &str| -> Result<String, ScriptError> {
        if let Some(role) = positional_role {
            Ok(role.as_string()?.to_owned())
        } else if let Some(role) = args.values.get("role") {
            Ok(role.as_string()?.to_owned())
        } else {
            Ok(default.to_owned())
        }
    };
    let ordinal = args
        .values
        .get("ordinal")
        .map(|value| index_value(value, "`ordinal`"))
        .transpose()?;

    match method {
        "face" => {
            let role = role("top_face")?;
            // An exported face by name first; a history role otherwise.
            if let Some(selector) = exported.get(&role) {
                return Ok(Value::Selector(selector.clone()));
            }
            Ok(Value::Selector(EntitySelector::ByHistory {
                from_step: step_label.clone(),
                kind: EntityKind::Face,
                role,
                ordinal,
            }))
        }
        // Every face the step made whatever its role, as a set: the face
        // counterpart of `.edges()`, and how a decompiled script writes a
        // selector for every face of a step.
        "faces" => {
            if positional_role.is_some() || !args.values.is_empty() {
                return Err(ScriptError::eval(
                    "`.faces()` takes no arguments; name one face with .face(\"role\", ordinal: n)",
                ));
            }
            Ok(Value::Selector(EntitySelector::history_faces(
                step_label.0.clone(),
            )))
        }
        "edge" => Ok(Value::Selector(EntitySelector::ByHistory {
            from_step: step_label.clone(),
            kind: EntityKind::Edge,
            role: role("edge")?,
            ordinal,
        })),
        "edges" => {
            // With nothing named, every edge the step made whatever its
            // role, as a set, so `cyl.edges()` is a cylinder's rims.
            if positional_role.is_none()
                && !args.values.contains_key("role")
                && !args.values.contains_key("count")
            {
                return Ok(Value::Selector(EntitySelector::history_edges(
                    step_label.0.clone(),
                )));
            }
            // Every edge the step produced under the role, by ordinal; the
            // session ignores ordinals the step never made.
            let role = role("edge")?;
            let count = args
                .values
                .get("count")
                .map_or(Ok(12.0), Value::as_number)?;
            if !(count.is_finite() && count >= 0.0 && count.fract() == 0.0) {
                return Err(ScriptError::eval(format!(
                    "`edges(count:)` takes a whole number of edges, got {count}"
                )));
            }
            if count > MAX_EDGE_SELECTORS as f64 {
                return Err(ScriptError::eval(format!(
                    "`edges(count:)` spells out at most {MAX_EDGE_SELECTORS} edges, not {count}"
                )));
            }
            let count = count as u32;
            Value::array(
                (0..count)
                    .map(|index| {
                        Value::Selector(EntitySelector::history_edge_ordinal(
                            step_label.0.clone(),
                            role.clone(),
                            index,
                        ))
                    })
                    .collect(),
            )
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
                "Unknown method `.{other}` on a step; use .face(\"role\"), .faces(), .edge(\"role\") or .edges(\"role\"){exports}"
            )))
        }
    }
}

/// The math functions, which take their numbers positionally.
const MATH_FUNCTIONS: &[&str] = &[
    "sqrt", "abs", "floor", "ceil", "round", "sin", "cos", "tan", "asin", "acos", "atan", "atan2",
    "pow", "hypot", "min", "max", "clamp",
];

/// A math function applied to numbers already evaluated.
fn math(name: &str, numbers: &[f64]) -> Result<f64, ScriptError> {
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
            let value = one(numbers)?;
            if value < 0.0 {
                return Err(ScriptError::eval("sqrt() of a negative number"));
            }
            value.sqrt()
        }
        "abs" => one(numbers)?.abs(),
        "floor" => one(numbers)?.floor(),
        "ceil" => one(numbers)?.ceil(),
        "round" => one(numbers)?.round(),
        "sin" => one(numbers)?.to_radians().sin(),
        "cos" => one(numbers)?.to_radians().cos(),
        "tan" => one(numbers)?.to_radians().tan(),
        "asin" => one(numbers)?.asin().to_degrees(),
        "acos" => one(numbers)?.acos().to_degrees(),
        "atan" => one(numbers)?.atan().to_degrees(),
        "atan2" => {
            let (y, x) = two(numbers)?;
            y.atan2(x).to_degrees()
        }
        "pow" => {
            let (base, exponent) = two(numbers)?;
            base.powf(exponent)
        }
        "hypot" => {
            let (a, b) = two(numbers)?;
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
        "clamp" => match numbers {
            // `f64::clamp` panics when the bounds are the wrong way round.
            [_, low, high] if low > high => {
                return Err(ScriptError::eval(format!(
                    "clamp(): the low {} is above the high {}",
                    number_text(*low),
                    number_text(*high)
                )));
            }
            [value, low, high] => value.clamp(*low, *high),
            _ => return Err(ScriptError::eval("clamp() takes a value, a low and a high")),
        },
        other => unreachable!("{other} is not among the math functions"),
    };
    if !value.is_finite() {
        return Err(ScriptError::eval(format!(
            "{name}() did not produce a finite number"
        )));
    }
    Ok(value)
}

/// `target[index]`, both already evaluated.
fn index_into(target: &Value, index: &Value) -> Result<Value, ScriptError> {
    let index = index.as_number()?;
    let Value::Array(items) = target else {
        return Err(ScriptError::eval(format!(
            "Only an array can be indexed, got {}",
            target.describe()
        )));
    };
    if index.fract() != 0.0 || index < 0.0 || index as usize >= items.len() {
        return Err(ScriptError::eval(format!(
            "Index {} is outside the array of {} items",
            number_text(index),
            items.len()
        )));
    }
    Ok(items[index as usize].clone())
}

/// A value that must be a whole number from zero up, such as an ordinal or
/// a region index. A fraction or a negative number is refused rather than
/// cut to the whole number below it, which would name a different entity
/// than the one written.
fn index_value(value: &Value, what: &str) -> Result<u32, ScriptError> {
    let number = value.as_number()?;
    if number.fract() != 0.0 || number < 0.0 || number > f64::from(u32::MAX) {
        return Err(ScriptError::eval(format!(
            "{what} is a whole number from 0 up, got {}",
            number_text(number)
        )));
    }
    Ok(number as u32)
}

fn module_builds_nothing(label: &str) -> ScriptError {
    ScriptError::eval(format!(
        "A module builds nothing at its top level; put the step \"{label}\" in a function"
    ))
}

/// A step label under a call's label prefix. A label already under the
/// prefix — the prefix and a slash — is left alone, so it does not double
/// up. A label that merely begins with the prefix's letters is not under
/// it: `base` in a call labelled `b` is `b/base`, or a call labelled `b`
/// and one labelled `ba` would both build a step `base`.
fn scoped(prefix: Option<&str>, raw: &str) -> String {
    match prefix {
        None => raw.to_owned(),
        Some(prefix)
            if raw
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/')) =>
        {
            raw.to_owned()
        }
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
            GeometricSelector::EdgeBetween { .. }
            | GeometricSelector::EdgesParallelTo { .. }
            | GeometricSelector::EdgesOfFace { .. } => Some(EntityKind::Edge),
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

/// A script value. Text, arrays and exported faces are shared rather than
/// copied when a value is: reading a variable, indexing an array, or
/// passing either to a call costs the same however large the value is.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Number(f64),
    Bool(bool),
    String(Rc<str>),
    Array(Items),
    Selector(EntitySelector),
    Step(StepLabel),
    /// A feature call. It is by far the largest value, and behind a
    /// pointer every value on the evaluator's stack is smaller for it.
    Command(Rc<ApiCommand>),
    /// A sketch entity awaiting a `sketch(...)` call to gather it.
    Entity(Rc<SketchEntity>),
    /// What a function returns with `with faces`: a step and the faces it
    /// exports by name.
    Body {
        step: StepLabel,
        faces: Rc<BTreeMap<String, EntitySelector>>,
    },
    /// A plane named by `plane(...)`, for `sketch(on: ...)`: a frame in
    /// space, or a plane placed by the body's faces and edges, which is
    /// resolved when the sketch runs.
    Plane(SketchPlane),
    /// An axis named by `axis(...)`, for `revolve(axis: ...)`: a line in
    /// space, or one placed by the body's edges and faces, which is resolved
    /// when the revolve runs.
    Axis(ScriptAxis),
    /// What a function without a `return` value evaluates to.
    Unit,
}

/// What `axis(...)` names.
#[derive(Clone, Debug, PartialEq)]
enum ScriptAxis {
    /// A line in space, fixed as written.
    Line { origin: Point3, direction: Vector3 },
    /// A line the body places.
    Placed(AxisPlacement),
}

/// An array value's items, shared between every value that holds them, with
/// the two measures the limits need, worked out once when the array is made
/// rather than by walking it each time they are asked.
#[derive(Clone, Debug, PartialEq)]
struct Items {
    values: Rc<[Value]>,
    /// How many arrays deep the value nests: one for an array of numbers.
    depth: usize,
    /// What a call that receives the array pays for it: see
    /// [`Value::weight`].
    weight: usize,
}

impl std::ops::Deref for Items {
    type Target = [Value];

    fn deref(&self) -> &[Value] {
        &self.values
    }
}

impl Value {
    fn command(command: ApiCommand) -> Self {
        Self::Command(Rc::new(command))
    }

    fn entity(entity: SketchEntity) -> Self {
        Self::Entity(Rc::new(entity))
    }

    /// An array of `values`, refused past [`MAX_ARRAY_ELEMENTS`] items or
    /// [`MAX_ARRAY_DEPTH`] levels of nesting.
    fn array(values: Vec<Self>) -> Result<Self, ScriptError> {
        if values.len() > MAX_ARRAY_ELEMENTS {
            return Err(ScriptError::eval(format!(
                "An array may hold at most {MAX_ARRAY_ELEMENTS} elements; this one has {}",
                values.len()
            )));
        }
        let depth = 1 + values.iter().map(Self::depth).max().unwrap_or(0);
        if depth > MAX_ARRAY_DEPTH {
            return Err(ScriptError::eval(format!(
                "Arrays may nest at most {MAX_ARRAY_DEPTH} deep"
            )));
        }
        let weight = values
            .iter()
            .map(Self::weight)
            .fold(values.len(), usize::saturating_add);
        Ok(Self::Array(Items {
            values: values.into(),
            depth,
            weight,
        }))
    }

    /// How many arrays deep the value nests; zero for anything else.
    fn depth(&self) -> usize {
        match self {
            Self::Array(items) => items.depth,
            _ => 0,
        }
    }

    /// The work a call receiving the value does with it at most: checking
    /// it against a type, or copying it into a command. One for a single
    /// value, one for every byte of text and every point of a spline, and
    /// for an array one for every item on top of its items' own weights.
    /// Arrays share their items, so the weight can far outgrow the memory a
    /// value holds; it saturates rather than overflows.
    fn weight(&self) -> usize {
        match self {
            Self::String(text) => text.len().max(1),
            Self::Array(items) => items.weight,
            Self::Body { faces, .. } => faces.len().saturating_add(1),
            // A spline is copied into its sketch point by point.
            Self::Entity(entity) => match &**entity {
                SketchEntity::Spline { points, .. } => points.len().max(1),
                SketchEntity::ControlSpline { control_points, .. } => control_points.len().max(1),
                _ => 1,
            },
            _ => 1,
        }
    }

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
            Self::Plane(_) => "a plane".to_owned(),
            Self::Axis(_) => "an axis".to_owned(),
            Self::Unit => "nothing".to_owned(),
        }
    }

    /// The value as text, for a parameter listing.
    fn text(&self) -> String {
        match self {
            Self::Number(number) => number_text(*number),
            Self::Bool(flag) => flag.to_string(),
            Self::String(text) => text.to_string(),
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
            Self::String(s) => Ok(s),
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
            Self::String(label) if !label.is_empty() => Ok(StepLabel(label.to_string())),
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
            let value = interp.eval_expr(expression, env)?;
            // What the call may copy into its command is paid for here, so
            // one large array handed to every iteration of a loop costs
            // what it would take to build that many.
            interp.charge(value.weight())?;
            values.insert(key.as_str(), value);
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

    /// One end angle of an arc, in the radians the arc holds: `start_angle`
    /// or `end_angle` in degrees, as scripts write angles, or
    /// `start_radians` or `end_radians` as they are. Not every angle in
    /// radians is some number of degrees converted, so a decompiled script
    /// writes the radians where no degree value gives them back exactly.
    fn arc_angle(&self, end: &str) -> Result<f64, ScriptError> {
        let degrees = format!("{end}_angle");
        let radians = format!("{end}_radians");
        match (
            self.values.get(degrees.as_str()),
            self.values.get(radians.as_str()),
        ) {
            (Some(value), None) => Ok(value.as_number()?.to_radians()),
            (None, Some(value)) => value.as_number(),
            (Some(_), Some(_)) => Err(ScriptError::eval(format!(
                "{}(): give `{degrees}` or `{radians}`, not both",
                self.call
            ))),
            (None, None) => Err(ScriptError::eval(format!(
                "{}() requires `{degrees}`",
                self.call
            ))),
        }
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
        let what = format!("{}(): a region index", self.call);
        match self.values.get("regions") {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => {
                items.iter().map(|item| index_value(item, &what)).collect()
            }
            Some(number @ Value::Number(_)) => Ok(vec![index_value(number, &what)?]),
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

/// The axis a face spelling points along: `">Z"` and `"top"` are +Z, and
/// so on for the six directions.
fn face_direction(spec: &str) -> Option<Vector3> {
    Some(match spec {
        ">Z" | "top" => Vector3::new(0.0, 0.0, 1.0),
        "<Z" | "bottom" => Vector3::new(0.0, 0.0, -1.0),
        ">Y" | "back" => Vector3::new(0.0, 1.0, 0.0),
        "<Y" | "front" => Vector3::new(0.0, -1.0, 0.0),
        ">X" | "right" => Vector3::new(1.0, 0.0, 0.0),
        "<X" | "left" => Vector3::new(-1.0, 0.0, 0.0),
        _ => return None,
    })
}

fn face_selector(spec: &str) -> Result<EntitySelector, ScriptError> {
    if let Some(direction) = face_direction(spec) {
        return Ok(EntitySelector::ByGeometry {
            selector: GeometricSelector::FaceByNormal {
                direction,
                match_kind: NormalMatch::Closest,
            },
        });
    }
    let selector = match spec {
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
            // A face direction names that face's edges: `edges(">Z")` is
            // `faces(">Z").edges()`. The other face spellings are not
            // taken, so an edge selector never quietly means many faces.
            if face_direction(spec).is_some() {
                return Ok(EntitySelector::edges_of_face(face_selector(spec)?));
            }
            return Err(ScriptError::eval(format!(
                "Unknown edge selector `{spec}`; use |X, |Y, |Z, longest, shortest, or a face direction such as >Z or top for the edges of that face"
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
                "sketch(): `on` is \"XY\", \"XZ\", \"YZ\", a plane(...) or a face selector, not \"{name}\""
            ))),
        },
        Value::Selector(selector) => Ok(SketchPlane::OnFace {
            face: selector.clone(),
        }),
        Value::Plane(plane) => Ok(plane.clone()),
        other => Err(ScriptError::eval(format!(
            "sketch(): `on` is \"XY\", \"XZ\", \"YZ\", a plane(...) or a face selector, got {}",
            other.describe()
        ))),
    }
}

/// The frame of one of the three world planes: its origin, and the axes a
/// sketch on it uses, whose cross product is the side it faces.
pub(crate) fn world_plane_frame(name: &str) -> Option<PlanarFrame3> {
    let origin = Point3::new(0.0, 0.0, 0.0);
    let (u, v) = match name.to_ascii_uppercase().as_str() {
        "XY" => (Vector3::new(1.0, 0.0, 0.0), Vector3::new(0.0, 1.0, 0.0)),
        "XZ" => (Vector3::new(1.0, 0.0, 0.0), Vector3::new(0.0, 0.0, 1.0)),
        "YZ" => (Vector3::new(0.0, 1.0, 0.0), Vector3::new(0.0, 0.0, 1.0)),
        _ => return None,
    };
    Some(PlanarFrame3::new(origin, u, v))
}

/// `plane(...)`: a plane for `sketch(on: ...)`.
///
/// The forms placed by the body (ADR 0048) are resolved when the sketch runs,
/// against the body as it then stands:
///
/// - `plane(on: face, offset: d)` is the face's own frame moved `d` along its
///   outward normal.
/// - `plane(between: [a, b], offset: d)` is halfway between two parallel
///   faces, facing as the first does.
/// - `plane(through: edge, face: f, angle: a)` hangs off a straight edge,
///   turned `a` degrees from the face `f` (the edge's only planar face when
///   left out).
///
/// Each takes `flip: true` to face the other way. The rest are frames in
/// space, fixed as they are written.
fn script_plane(args: &Args<'_>) -> Result<SketchPlane, ScriptError> {
    let offset = args.number_or("offset", 0.0)?;
    let flip = match args.values.get("flip") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(other) => {
            return Err(ScriptError::eval(format!(
                "plane(): `flip` is true or false, got {}",
                other.describe()
            )));
        }
    };
    if let Some(face) = args.values.get("on") {
        return Ok(SketchPlane::OffsetFace {
            face: Box::new(face.as_selector()?),
            offset,
            flip,
        });
    }
    if let Some(faces) = args.values.get("between") {
        let faces = faces.as_selectors()?;
        let [first, second] = <[EntitySelector; 2]>::try_from(faces).map_err(|faces| {
            ScriptError::eval(format!(
                "plane(): `between` is an array of two faces, got {}",
                faces.len()
            ))
        })?;
        return Ok(SketchPlane::Midplane {
            first: Box::new(first),
            second: Box::new(second),
            offset,
            flip,
        });
    }
    if let Some(edge) = args.values.get("through") {
        return Ok(SketchPlane::ThroughEdge {
            edge: Box::new(edge.as_selector()?),
            face: args
                .values
                .get("face")
                .map(Value::as_selector)
                .transpose()?
                .map(Box::new),
            angle_degrees: args.number_or("angle", 0.0)?,
            offset,
            flip,
        });
    }
    if flip {
        return Err(ScriptError::eval(
            "plane(): `flip` goes with `on`, `between` or `through`; a frame in space faces the way its axes say",
        ));
    }
    world_plane(args).map(|frame| SketchPlane::Frame { frame })
}

/// `axis(...)`: an axis for `revolve(axis: ...)`.
///
/// - `axis(from: "Z")` is a world axis through the origin.
/// - `axis(origin: [...], direction: [...])` is a line in space.
/// - `axis(along: edge)` runs along a straight edge, from its start to its
///   end.
/// - `axis(through: face)` is a curved face's own axis: a cylinder's, a
///   cone's, a sphere's or a torus's.
/// - `axis(between: [a, b])` is where two flat faces meet.
///
/// The last three are placed by the body and resolved when the revolve
/// runs, against the body as it then stands. Each form takes `flip: true`
/// to run the other way, which turns a partial revolve the other way.
fn script_axis(args: &Args<'_>) -> Result<ScriptAxis, ScriptError> {
    let flip = match args.values.get("flip") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(other) => {
            return Err(ScriptError::eval(format!(
                "axis(): `flip` is true or false, got {}",
                other.describe()
            )));
        }
    };
    if let Some(edge) = args.values.get("along") {
        return Ok(ScriptAxis::Placed(AxisPlacement::Along {
            edge: Box::new(edge.as_selector()?),
            flip,
        }));
    }
    if let Some(face) = args.values.get("through") {
        return Ok(ScriptAxis::Placed(AxisPlacement::Through {
            face: Box::new(face.as_selector()?),
            flip,
        }));
    }
    if let Some(faces) = args.values.get("between") {
        let faces = faces.as_selectors()?;
        let [first, second] = <[EntitySelector; 2]>::try_from(faces).map_err(|faces| {
            ScriptError::eval(format!(
                "axis(): `between` is an array of two flat faces, got {}",
                faces.len()
            ))
        })?;
        return Ok(ScriptAxis::Placed(AxisPlacement::Between {
            first: Box::new(first),
            second: Box::new(second),
            flip,
        }));
    }
    let sign = if flip { -1.0 } else { 1.0 };
    if let Some(from) = args.values.get("from") {
        let name = from.as_string()?;
        let direction = match name.to_ascii_uppercase().as_str() {
            "X" => Vector3::new(sign, 0.0, 0.0),
            "Y" => Vector3::new(0.0, sign, 0.0),
            "Z" => Vector3::new(0.0, 0.0, sign),
            _ => {
                return Err(ScriptError::eval(format!(
                    "axis(): `from` is \"X\", \"Y\" or \"Z\", not \"{name}\""
                )));
            }
        };
        return Ok(ScriptAxis::Line {
            origin: Point3::new(0.0, 0.0, 0.0),
            direction,
        });
    }
    let origin = args.point3_or("origin", Point3::new(0.0, 0.0, 0.0))?;
    let direction = args.required("direction").map_err(|_| {
        ScriptError::eval(
            "axis(): give `from`, `origin` and `direction`, `along` an edge, `through` a curved face, or `between` two flat faces",
        )
    })?;
    let direction = direction.as_vector3()?;
    let length =
        (direction.x * direction.x + direction.y * direction.y + direction.z * direction.z).sqrt();
    if !length.is_finite() || length == 0.0 {
        return Err(ScriptError::eval(
            "axis(): `direction` must be a non-zero direction",
        ));
    }
    Ok(ScriptAxis::Line {
        origin,
        direction: Vector3::new(sign * direction.x, sign * direction.y, sign * direction.z),
    })
}

/// A plane in space as a frame: a world plane moved along its normal, or
/// one given by its origin and axes.
///
/// - `plane(from: "XY", offset: 30)` is a world plane moved along the side it
///   faces — +Z for XY, −Y for XZ, +X for YZ, the sides their sketches face.
/// - `plane(origin: [...], normal: [...], x_axis: [...])` faces `normal`,
///   with its `u` axis along `x_axis` turned into the plane.
/// - `plane(origin: [...], x_axis: [...], y_axis: [...])` takes its two axes
///   as given, which is how a decompiled script writes a plane back exactly.
fn world_plane(args: &Args<'_>) -> Result<PlanarFrame3, ScriptError> {
    let length =
        |vector: Vector3| (vector.x * vector.x + vector.y * vector.y + vector.z * vector.z).sqrt();
    if let Some(from) = args.values.get("from") {
        let name = from.as_string()?;
        let frame = world_plane_frame(name).ok_or_else(|| {
            ScriptError::eval(format!(
                "plane(): `from` is \"XY\", \"XZ\" or \"YZ\", not \"{name}\""
            ))
        })?;
        let offset = args.number_or("offset", 0.0)?;
        if !offset.is_finite() {
            return Err(ScriptError::eval(
                "plane(): `offset` must be a finite length",
            ));
        }
        let (u, v) = (frame.u, frame.v);
        let normal = Vector3::new(
            u.y * v.z - u.z * v.y,
            u.z * v.x - u.x * v.z,
            u.x * v.y - u.y * v.x,
        );
        return Ok(PlanarFrame3::new(
            Point3::new(normal.x * offset, normal.y * offset, normal.z * offset),
            u,
            v,
        ));
    }
    let origin = args.point3_or("origin", Point3::new(0.0, 0.0, 0.0))?;
    let x_axis = args.required("x_axis")?.as_vector3()?;
    if let Some(y_axis) = args.values.get("y_axis") {
        return Ok(PlanarFrame3::new(origin, x_axis, y_axis.as_vector3()?));
    }
    let normal = args.required("normal")?.as_vector3()?;
    let normal_length = length(normal);
    if !normal_length.is_finite() || normal_length == 0.0 {
        return Err(ScriptError::eval(
            "plane(): `normal` must be a non-zero direction",
        ));
    }
    let n = Vector3::new(
        normal.x / normal_length,
        normal.y / normal_length,
        normal.z / normal_length,
    );
    let along = x_axis.x * n.x + x_axis.y * n.y + x_axis.z * n.z;
    let u = Vector3::new(
        x_axis.x - n.x * along,
        x_axis.y - n.y * along,
        x_axis.z - n.z * along,
    );
    let u_length = length(u);
    if !u_length.is_finite() || u_length <= 1.0e-9 * length(x_axis).max(1.0) {
        return Err(ScriptError::eval(
            "plane(): `x_axis` must not run along `normal`; it names the direction in the plane its sketches take as x",
        ));
    }
    let u = Vector3::new(u.x / u_length, u.y / u_length, u.z / u_length);
    let v = Vector3::new(
        n.y * u.z - n.z * u.y,
        n.z * u.x - n.x * u.z,
        n.x * u.y - n.y * u.x,
    );
    Ok(PlanarFrame3::new(origin, u, v))
}

fn sketch_entities(value: &Value) -> Result<Vec<SketchEntity>, ScriptError> {
    let items = match value {
        Value::Array(items) => &items[..],
        Value::Entity(_) => std::slice::from_ref(value),
        other => {
            return Err(ScriptError::eval(format!(
                "sketch(): `entities` is an array of line(), circle(), arc(), rect() or spline() calls, got {}",
                other.describe()
            )));
        }
    };
    items
        .iter()
        .map(|item| match item {
            Value::Entity(entity) => Ok((**entity).clone()),
            other => Err(ScriptError::eval(format!(
                "sketch(): every entity is a line(), circle(), arc(), rect() or spline(), got {}",
                other.describe()
            ))),
        })
        .collect()
}
