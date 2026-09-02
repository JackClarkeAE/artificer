//! Abstract Syntax Tree for the .art CAD scripting language.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AstNode {
    ParamDecl {
        name: String,
        param_type: String,
        default_value: Expression,
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Expression {
    Number(f64),
    String(String),
    Identifier(String),
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
