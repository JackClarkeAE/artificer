//! Abstract Syntax Tree for the .art CAD scripting language.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AstNode {
    ParamDecl {
        name: String,
        param_type: String,
        default_value: Expression,
        /// The unit written in brackets after the type, such as `mm`.
        unit: Option<String>,
        /// `in low..high`: the values the parameter may take.
        range: Option<(Expression, Expression)>,
        /// The string after the default, for a customizer to show.
        description: Option<String>,
        /// The line the declaration starts on, for a customizer to point at.
        line: usize,
    },
    LetBinding {
        name: String,
        value: Expression,
    },
    Statement(Expression),
    /// `for name in start..end { ... }`: the body runs once per whole
    /// number from `start` up to but not including `end`.
    For {
        variable: String,
        start: Expression,
        end: Expression,
        body: Vec<AstNode>,
        line: usize,
        col: usize,
    },
    /// `fn name(a: f64, b: face = ...) -> body { ... }`.
    FnDecl(FnDecl),
    /// `return value;` or `return body with faces { name: selector };`.
    Return {
        value: Option<Expression>,
        faces: Vec<(String, Expression)>,
        line: usize,
        col: usize,
    },
    /// `use "path/to/module.art";`
    Use {
        path: String,
        line: usize,
        col: usize,
    },
}

/// A user-defined function.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<FnParam>,
    pub return_type: Option<TypeSpec>,
    pub body: Vec<AstNode>,
    pub line: usize,
    pub col: usize,
}

/// One declared parameter of a function.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FnParam {
    pub name: String,
    pub param_type: TypeSpec,
    pub default: Option<Expression>,
}

/// A type as a script writes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TypeSpec {
    /// `f64`, `float`, `number`.
    Number,
    /// `int`: a whole number.
    Int,
    /// `str`, `string`.
    Str,
    /// `bool`.
    Bool,
    /// `face`: a face selector.
    Face,
    /// `edge`: an edge selector.
    Edge,
    /// `body`: a step, or a body returned by a function.
    Body,
    /// `[T; N]` or `[T]`.
    Array(Box<TypeSpec>, Option<usize>),
    /// `any`.
    Any,
}

impl TypeSpec {
    /// The type as a script writes it.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Number => "f64".to_owned(),
            Self::Int => "int".to_owned(),
            Self::Str => "str".to_owned(),
            Self::Bool => "bool".to_owned(),
            Self::Face => "face".to_owned(),
            Self::Edge => "edge".to_owned(),
            Self::Body => "body".to_owned(),
            Self::Array(element, Some(length)) => format!("[{}; {length}]", element.describe()),
            Self::Array(element, None) => format!("[{}]", element.describe()),
            Self::Any => "any".to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Expression {
    Number(f64),
    Bool(bool),
    String(String),
    Identifier {
        name: String,
        line: usize,
        col: usize,
    },
    Array(Vec<Expression>),
    BinaryOp {
        left: Box<Expression>,
        op: BinaryOperator,
        right: Box<Expression>,
    },
    UnaryOp {
        op: UnaryOperator,
        operand: Box<Expression>,
    },
    FunctionCall {
        name: String,
        named_args: Vec<(String, Expression)>,
        positional_args: Vec<Expression>,
        /// Where the call is written, so an evaluation error can say so.
        line: usize,
        col: usize,
    },
    MethodCall {
        target: Box<Expression>,
        method: String,
        named_args: Vec<(String, Expression)>,
        positional_args: Vec<Expression>,
        line: usize,
        col: usize,
    },
    /// `array[index]`, zero-based.
    Index {
        target: Box<Expression>,
        index: Box<Expression>,
        line: usize,
        col: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOperator {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnaryOperator {
    Neg,
}
