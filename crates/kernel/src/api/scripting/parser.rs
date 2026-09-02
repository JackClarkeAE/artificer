//! Recursive-descent parser for the .art language.

use crate::api::scripting::ast::{
    AstNode, BinaryOperator, Expression, FnDecl, FnParam, TypeSpec, UnaryOperator,
};
use crate::api::scripting::lexer::{SpannedToken, Token};

pub type ParsedArgs = (Vec<(String, Expression)>, Vec<Expression>);

/// The deepest expression nesting the parser follows. A script is a few
/// hundred lines of feature calls, never a deeply nested term; the limit
/// exists so that a hostile `((((...` over the wire is an error rather than
/// a stack overflow that takes the whole server with it.
pub const MAX_EXPRESSION_DEPTH: usize = 64;

pub struct Parser {
    tokens: Vec<SpannedToken>,
    pos: usize,
    depth: usize,
}

impl Parser {
    pub fn new(tokens: Vec<SpannedToken>) -> Self {
        Self {
            tokens,
            pos: 0,
            depth: 0,
        }
    }

    fn peek(&self) -> &Token {
        if self.pos < self.tokens.len() {
            &self.tokens[self.pos].token
        } else {
            &Token::Eof
        }
    }

    fn peek_at(&self, offset: usize) -> &Token {
        self.tokens
            .get(self.pos + offset)
            .map_or(&Token::Eof, |token| &token.token)
    }

    fn current_span(&self) -> (usize, usize) {
        if self.pos < self.tokens.len() {
            (self.tokens[self.pos].line, self.tokens[self.pos].col)
        } else {
            (0, 0)
        }
    }

    fn advance(&mut self) -> Token {
        if self.pos < self.tokens.len() {
            let token = self.tokens[self.pos].token.clone();
            self.pos += 1;
            token
        } else {
            Token::Eof
        }
    }

    fn expect(&mut self, expected: Token) -> Result<(), String> {
        let (line, col) = self.current_span();
        let token = self.advance();
        if token == expected {
            Ok(())
        } else {
            Err(format!(
                "Expected {expected:?} but found {token:?} at {line}:{col}"
            ))
        }
    }

    fn expect_ident(&mut self, what: &str) -> Result<String, String> {
        let (line, col) = self.current_span();
        match self.advance() {
            Token::Ident(name) => Ok(name),
            other => Err(format!("Expected {what} at {line}:{col}, got {other:?}")),
        }
    }

    fn skip_semi(&mut self) {
        if self.peek() == &Token::Semi {
            self.advance();
        }
    }

    pub fn parse_program(&mut self) -> Result<Vec<AstNode>, String> {
        let mut nodes = Vec::new();
        while self.peek() != &Token::Eof {
            nodes.push(self.parse_statement()?);
        }
        Ok(nodes)
    }

    fn parse_statement(&mut self) -> Result<AstNode, String> {
        match self.peek() {
            Token::Param => self.parse_param_decl(),
            Token::Let => self.parse_let_binding(),
            Token::For => self.parse_for_loop(),
            Token::Fn => self.parse_fn_decl(),
            Token::Return => self.parse_return(),
            Token::Use => self.parse_use(),
            _ => {
                let expr = self.parse_expression()?;
                self.skip_semi();
                Ok(AstNode::Statement(expr))
            }
        }
    }

    /// `param name[: type] [[unit]] [in low..high] = default ["description"];`
    fn parse_param_decl(&mut self) -> Result<AstNode, String> {
        let (decl_line, _) = self.current_span();
        self.advance(); // 'param'
        let name = self.expect_ident("a parameter name")?;

        let mut param_type = "f64".to_owned();
        if self.peek() == &Token::Colon {
            self.advance();
            param_type = self.expect_ident("a parameter type")?;
        }

        let mut unit = None;
        if self.peek() == &Token::LBracket {
            self.advance();
            unit = Some(self.expect_ident("a unit such as mm")?);
            self.expect(Token::RBracket)?;
        }

        let mut range = None;
        if self.peek() == &Token::In {
            self.advance();
            let low = self.parse_expression()?;
            self.expect(Token::DotDot)?;
            let high = self.parse_expression()?;
            range = Some((low, high));
        }

        self.expect(Token::Equal)?;
        let default_value = self.parse_expression()?;

        let mut description = None;
        if let Token::StringLit(text) = self.peek() {
            description = Some(text.clone());
            self.advance();
        }
        self.skip_semi();

        Ok(AstNode::ParamDecl {
            name,
            param_type,
            default_value,
            unit,
            range,
            description,
            line: decl_line,
        })
    }

    fn parse_for_loop(&mut self) -> Result<AstNode, String> {
        let (line, col) = self.current_span();
        self.advance(); // 'for'
        let variable = self.expect_ident("a loop variable")?;
        self.expect(Token::In)?;
        let start = self.parse_expression()?;
        self.expect(Token::DotDot)?;
        let end = self.parse_expression()?;
        let body = self.parse_block("loop", line, col)?;
        Ok(AstNode::For {
            variable,
            start,
            end,
            body,
            line,
            col,
        })
    }

    /// `{ statements }`, naming the construct that opened it when the brace
    /// never closes.
    fn parse_block(&mut self, what: &str, line: usize, col: usize) -> Result<Vec<AstNode>, String> {
        self.expect(Token::LBrace)?;
        let mut body = Vec::new();
        while self.peek() != &Token::RBrace {
            if self.peek() == &Token::Eof {
                let (eof_line, eof_col) = self.current_span();
                return Err(format!(
                    "The {what} starting at {line}:{col} has no closing brace at {eof_line}:{eof_col}"
                ));
            }
            body.push(self.parse_statement()?);
        }
        self.expect(Token::RBrace)?;
        Ok(body)
    }

    /// `fn name(param: type [= default], ...) [-> type] { body }`
    fn parse_fn_decl(&mut self) -> Result<AstNode, String> {
        let (line, col) = self.current_span();
        self.advance(); // 'fn'
        let name = self.expect_ident("a function name")?;
        self.expect(Token::LParen)?;
        let mut params = Vec::new();
        while self.peek() != &Token::RParen {
            if self.peek() == &Token::Eof {
                return Err(format!(
                    "The parameter list of fn {name} at {line}:{col} never closes"
                ));
            }
            let param_name = self.expect_ident("a parameter name")?;
            let param_type = if self.peek() == &Token::Colon {
                self.advance();
                self.parse_type()?
            } else {
                TypeSpec::Any
            };
            let default = if self.peek() == &Token::Equal {
                self.advance();
                Some(self.parse_expression()?)
            } else {
                None
            };
            params.push(FnParam {
                name: param_name,
                param_type,
                default,
            });
            if self.peek() == &Token::Comma {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(Token::RParen)?;
        let return_type = if self.peek() == &Token::Arrow {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = self.parse_block(&format!("fn {name}"), line, col)?;
        Ok(AstNode::FnDecl(FnDecl {
            name,
            params,
            return_type,
            body,
            line,
            col,
        }))
    }

    /// `f64`, `int`, `str`, `bool`, `face`, `edge`, `body`, `any`,
    /// `[type; N]` or `[type]`.
    fn parse_type(&mut self) -> Result<TypeSpec, String> {
        let (line, col) = self.current_span();
        match self.advance() {
            Token::Ident(name) => match name.as_str() {
                "f64" | "float" | "number" => Ok(TypeSpec::Number),
                "int" | "i64" => Ok(TypeSpec::Int),
                "str" | "string" => Ok(TypeSpec::Str),
                "bool" => Ok(TypeSpec::Bool),
                "face" => Ok(TypeSpec::Face),
                "edge" => Ok(TypeSpec::Edge),
                "body" | "step" => Ok(TypeSpec::Body),
                "any" => Ok(TypeSpec::Any),
                other => Err(format!(
                    "Unknown type `{other}` at {line}:{col}; the types are f64, int, str, bool, face, edge, body, any and [type; N]"
                )),
            },
            Token::LBracket => {
                let element = self.parse_type()?;
                let length = if self.peek() == &Token::Semi {
                    self.advance();
                    let (length_line, length_col) = self.current_span();
                    match self.advance() {
                        Token::Number(number) if number.fract() == 0.0 && number >= 0.0 => {
                            Some(number as usize)
                        }
                        other => {
                            return Err(format!(
                                "Expected an array length at {length_line}:{length_col}, got {other:?}"
                            ));
                        }
                    }
                } else {
                    None
                };
                self.expect(Token::RBracket)?;
                Ok(TypeSpec::Array(Box::new(element), length))
            }
            other => Err(format!("Expected a type at {line}:{col}, got {other:?}")),
        }
    }

    /// `return [value] [with faces { name: selector, ... }];`
    fn parse_return(&mut self) -> Result<AstNode, String> {
        let (line, col) = self.current_span();
        self.advance(); // 'return'
        let value = if matches!(self.peek(), Token::Semi | Token::RBrace | Token::Eof) {
            None
        } else {
            Some(self.parse_expression()?)
        };
        let mut faces = Vec::new();
        if self.peek() == &Token::With {
            self.advance();
            let (kw_line, kw_col) = self.current_span();
            match self.advance() {
                Token::Ident(word) if word == "faces" => {}
                other => {
                    return Err(format!(
                        "Expected `faces` after `with` at {kw_line}:{kw_col}, got {other:?}"
                    ));
                }
            }
            self.expect(Token::LBrace)?;
            while self.peek() != &Token::RBrace {
                if self.peek() == &Token::Eof {
                    return Err(format!(
                        "The `with faces` list at {line}:{col} never closes"
                    ));
                }
                let name = self.expect_ident("an exported face name")?;
                self.expect(Token::Colon)?;
                let selector = self.parse_expression()?;
                faces.push((name, selector));
                if self.peek() == &Token::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            self.expect(Token::RBrace)?;
        }
        self.skip_semi();
        Ok(AstNode::Return {
            value,
            faces,
            line,
            col,
        })
    }

    /// `use "path/to/module.art";`
    fn parse_use(&mut self) -> Result<AstNode, String> {
        let (line, col) = self.current_span();
        self.advance(); // 'use'
        let (path_line, path_col) = self.current_span();
        let path = match self.advance() {
            Token::StringLit(path) => path,
            other => {
                return Err(format!(
                    "Expected a module path in quotes at {path_line}:{path_col}, got {other:?}"
                ));
            }
        };
        self.skip_semi();
        Ok(AstNode::Use { path, line, col })
    }

    fn parse_let_binding(&mut self) -> Result<AstNode, String> {
        self.advance(); // 'let'
        let name = self.expect_ident("a variable name")?;
        self.expect(Token::Equal)?;
        let value = self.parse_expression()?;
        self.skip_semi();
        Ok(AstNode::LetBinding { name, value })
    }

    pub fn parse_expression(&mut self) -> Result<Expression, String> {
        if self.depth >= MAX_EXPRESSION_DEPTH {
            let (line, col) = self.current_span();
            return Err(format!(
                "Expression nested deeper than {MAX_EXPRESSION_DEPTH} levels at {line}:{col}"
            ));
        }
        self.depth += 1;
        let result = self.parse_add_sub();
        self.depth -= 1;
        result
    }

    fn parse_add_sub(&mut self) -> Result<Expression, String> {
        let mut left = self.parse_mul_div()?;
        while matches!(self.peek(), Token::Plus | Token::Minus) {
            let op = match self.advance() {
                Token::Plus => BinaryOperator::Add,
                Token::Minus => BinaryOperator::Sub,
                _ => unreachable!(),
            };
            let right = self.parse_mul_div()?;
            left = Expression::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_mul_div(&mut self) -> Result<Expression, String> {
        let mut left = self.parse_unary()?;
        while matches!(self.peek(), Token::Star | Token::Slash) {
            let op = match self.advance() {
                Token::Star => BinaryOperator::Mul,
                Token::Slash => BinaryOperator::Div,
                _ => unreachable!(),
            };
            let right = self.parse_unary()?;
            left = Expression::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expression, String> {
        if self.peek() == &Token::Minus {
            self.advance();
            let operand = self.parse_postfix()?;
            Ok(Expression::UnaryOp {
                op: UnaryOperator::Neg,
                operand: Box::new(operand),
            })
        } else {
            self.parse_postfix()
        }
    }

    fn parse_postfix(&mut self) -> Result<Expression, String> {
        let mut expr = self.parse_primary()?;

        loop {
            if self.peek() == &Token::Dot {
                self.advance(); // '.'
                let (line, col) = self.current_span();
                let method = self.expect_ident("a method name")?;
                // `body.top` reads an exported face; `body.face("top")`
                // calls a method. Both are method calls to the evaluator.
                let (named_args, positional_args) = if self.peek() == &Token::LParen {
                    self.parse_arguments()?
                } else {
                    (Vec::new(), Vec::new())
                };
                expr = Expression::MethodCall {
                    target: Box::new(expr),
                    method,
                    named_args,
                    positional_args,
                    line,
                    col,
                };
            } else if self.peek() == &Token::LBracket {
                let (line, col) = self.current_span();
                self.advance(); // '['
                let index = self.parse_expression()?;
                self.expect(Token::RBracket)?;
                expr = Expression::Index {
                    target: Box::new(expr),
                    index: Box::new(index),
                    line,
                    col,
                };
            } else {
                break;
            }
        }

        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expression, String> {
        match self.peek() {
            Token::Number(n) => {
                let val = *n;
                self.advance();
                Ok(Expression::Number(val))
            }
            Token::True => {
                self.advance();
                Ok(Expression::Bool(true))
            }
            Token::False => {
                self.advance();
                Ok(Expression::Bool(false))
            }
            Token::StringLit(s) => {
                let val = s.clone();
                self.advance();
                Ok(Expression::String(val))
            }
            Token::Ident(name) => {
                let name = name.clone();
                let (line, col) = self.current_span();
                self.advance();
                if self.peek() == &Token::LParen {
                    let (named_args, positional_args) = self.parse_arguments()?;
                    Ok(Expression::FunctionCall {
                        name,
                        named_args,
                        positional_args,
                        line,
                        col,
                    })
                } else {
                    Ok(Expression::Identifier { name, line, col })
                }
            }
            Token::LBracket => {
                self.advance(); // '['
                let mut elements = Vec::new();
                while self.peek() != &Token::RBracket && self.peek() != &Token::Eof {
                    elements.push(self.parse_expression()?);
                    if self.peek() == &Token::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.expect(Token::RBracket)?;
                Ok(Expression::Array(elements))
            }
            Token::LParen => {
                self.advance(); // '('
                let inner = self.parse_expression()?;
                self.expect(Token::RParen)?;
                Ok(inner)
            }
            other => {
                let (line, col) = self.current_span();
                Err(format!("Unexpected token {other:?} at {line}:{col}"))
            }
        }
    }

    fn parse_arguments(&mut self) -> Result<ParsedArgs, String> {
        self.expect(Token::LParen)?;
        let mut named_args = Vec::new();
        let mut positional_args = Vec::new();

        while self.peek() != &Token::RParen && self.peek() != &Token::Eof {
            // Check for named argument: identifier ':'
            if let Token::Ident(name) = self.peek() {
                let name_clone = name.clone();
                if self.peek_at(1) == &Token::Colon {
                    self.advance(); // ident
                    self.advance(); // ':'
                    let val = self.parse_expression()?;
                    named_args.push((name_clone, val));
                } else {
                    positional_args.push(self.parse_expression()?);
                }
            } else {
                positional_args.push(self.parse_expression()?);
            }

            if self.peek() == &Token::Comma {
                self.advance();
            } else {
                break;
            }
        }

        self.expect(Token::RParen)?;
        Ok((named_args, positional_args))
    }
}
