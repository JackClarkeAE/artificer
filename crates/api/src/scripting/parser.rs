//! Recursive-descent parser for the .art language.

use crate::scripting::ast::{AstNode, BinaryOperator, Expression, UnaryOperator};
use crate::scripting::lexer::{SpannedToken, Token};

pub type ParsedArgs = (Vec<(String, Expression)>, Vec<Expression>);

pub struct Parser {
    tokens: Vec<SpannedToken>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<SpannedToken>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        if self.pos < self.tokens.len() {
            &self.tokens[self.pos].token
        } else {
            &Token::Eof
        }
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
            _ => {
                let expr = self.parse_expression()?;
                if self.peek() == &Token::Semi {
                    self.advance();
                }
                Ok(AstNode::Statement(expr))
            }
        }
    }

    fn parse_param_decl(&mut self) -> Result<AstNode, String> {
        self.advance(); // 'param'
        let (line, col) = self.current_span();
        let name = match self.advance() {
            Token::Ident(s) => s,
            other => {
                return Err(format!(
                    "Expected parameter identifier at {line}:{col}, got {other:?}"
                ));
            }
        };

        let mut param_type = "f64".to_owned();
        if self.peek() == &Token::Colon {
            self.advance();
            match self.advance() {
                Token::Ident(t) => param_type = t,
                other => return Err(format!("Expected parameter type, got {other:?}")),
            }
        }

        self.expect(Token::Equal)?;
        let default_value = self.parse_expression()?;
        if self.peek() == &Token::Semi {
            self.advance();
        }

        Ok(AstNode::ParamDecl {
            name,
            param_type,
            default_value,
        })
    }

    fn parse_let_binding(&mut self) -> Result<AstNode, String> {
        self.advance(); // 'let'
        let (line, col) = self.current_span();
        let name = match self.advance() {
            Token::Ident(s) => s,
            other => {
                return Err(format!(
                    "Expected variable name at {line}:{col}, got {other:?}"
                ));
            }
        };

        self.expect(Token::Equal)?;
        let value = self.parse_expression()?;
        if self.peek() == &Token::Semi {
            self.advance();
        }

        Ok(AstNode::LetBinding { name, value })
    }

    pub fn parse_expression(&mut self) -> Result<Expression, String> {
        self.parse_add_sub()
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
                let method = match self.advance() {
                    Token::Ident(s) => s,
                    other => {
                        return Err(format!(
                            "Expected method name at {line}:{col}, got {other:?}"
                        ));
                    }
                };

                let (named_args, positional_args) = self.parse_arguments()?;
                expr = Expression::MethodCall {
                    target: Box::new(expr),
                    method,
                    named_args,
                    positional_args,
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
            Token::StringLit(s) => {
                let val = s.clone();
                self.advance();
                Ok(Expression::String(val))
            }
            Token::Ident(name) => {
                let name = name.clone();
                self.advance();
                if self.peek() == &Token::LParen {
                    let (named_args, positional_args) = self.parse_arguments()?;
                    Ok(Expression::FunctionCall {
                        name,
                        named_args,
                        positional_args,
                    })
                } else {
                    Ok(Expression::Identifier(name))
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
                if self.tokens.get(self.pos + 1).map(|t| &t.token) == Some(&Token::Colon) {
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
