//! Abstract Syntax Tree for the .art CAD scripting language.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AstNode {
    ParamDecl {
        name: String,
        param_type: String,
        default_value: Expression,
    },
    LetBinding {
        name: String,
        value: Expression,
    },
    Statement(Expression),
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
    },
    MethodCall {
        target: Box<Expression>,
        method: String,
        named_args: Vec<(String, Expression)>,
        positional_args: Vec<Expression>,
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
