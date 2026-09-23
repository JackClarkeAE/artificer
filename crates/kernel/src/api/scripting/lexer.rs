//! Lexer for tokenizing .art CAD scripts.

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Param,
    Let,
    For,
    In,
    Fn,
    Return,
    Use,
    With,
    True,
    False,
    LBrace,
    RBrace,
    DotDot,
    Arrow,
    Ident(String),
    Number(f64),
    StringLit(String),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Colon,
    Semi,
    Comma,
    Equal,
    Plus,
    Minus,
    Star,
    Slash,
    Dot,
    Eof,
}

/// A token as an error message names it: the source spelling in backticks,
/// so a message reads "expected `)` but found `e12`", and "end of file"
/// for the end.
impl fmt::Display for Token {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let spelling = match self {
            Self::Param => "param",
            Self::Let => "let",
            Self::For => "for",
            Self::In => "in",
            Self::Fn => "fn",
            Self::Return => "return",
            Self::Use => "use",
            Self::With => "with",
            Self::True => "true",
            Self::False => "false",
            Self::LBrace => "{",
            Self::RBrace => "}",
            Self::DotDot => "..",
            Self::Arrow => "->",
            Self::LParen => "(",
            Self::RParen => ")",
            Self::LBracket => "[",
            Self::RBracket => "]",
            Self::Colon => ":",
            Self::Semi => ";",
            Self::Comma => ",",
            Self::Equal => "=",
            Self::Plus => "+",
            Self::Minus => "-",
            Self::Star => "*",
            Self::Slash => "/",
            Self::Dot => ".",
            Self::Ident(name) => return write!(formatter, "`{name}`"),
            Self::Number(number) => return write!(formatter, "`{number}`"),
            Self::StringLit(text) => return write!(formatter, "`\"{text}\"`"),
            Self::Eof => return formatter.write_str("end of file"),
        };
        write!(formatter, "`{spelling}`")
    }
}

#[derive(Clone, Debug)]
pub struct SpannedToken {
    pub token: Token,
    pub line: usize,
    pub col: usize,
}

pub fn tokenize(source: &str) -> Result<Vec<SpannedToken>, String> {
    let mut tokens = Vec::new();
    let mut chars = source.chars().peekable();
    let mut line = 1;
    let mut col = 1;

    while let Some(&ch) = chars.peek() {
        if ch == '\n' {
            chars.next();
            line += 1;
            col = 1;
            continue;
        }
        if ch.is_whitespace() {
            chars.next();
            col += 1;
            continue;
        }

        // Line comment: //
        if ch == '/' {
            chars.next();
            if let Some(&'/') = chars.peek() {
                chars.next();
                while let Some(&c) = chars.peek() {
                    if c == '\n' {
                        break;
                    }
                    chars.next();
                }
                continue;
            } else {
                tokens.push(SpannedToken {
                    token: Token::Slash,
                    line,
                    col,
                });
                col += 1;
                continue;
            }
        }

        let start_col = col;

        if ch.is_ascii_alphabetic() || ch == '_' {
            let mut s = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_alphanumeric() || c == '_' {
                    s.push(c);
                    chars.next();
                    col += 1;
                } else {
                    break;
                }
            }
            let token = match s.as_str() {
                "param" => Token::Param,
                "let" => Token::Let,
                "for" => Token::For,
                "in" => Token::In,
                "fn" => Token::Fn,
                "return" => Token::Return,
                "use" => Token::Use,
                "with" => Token::With,
                "true" => Token::True,
                "false" => Token::False,
                _ => Token::Ident(s),
            };
            tokens.push(SpannedToken {
                token,
                line,
                col: start_col,
            });
            continue;
        }

        if ch.is_ascii_digit() {
            let mut s = String::new();
            let mut has_dot = false;
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() {
                    s.push(c);
                    chars.next();
                    col += 1;
                } else if c == '.' && !has_dot && chars.clone().nth(1) != Some('.') {
                    // A lone `.` continues the number; `..` after a number
                    // is the range of a `for` loop and ends it.
                    s.push(c);
                    has_dot = true;
                    chars.next();
                    col += 1;
                } else {
                    break;
                }
            }
            // An exponent: `1e-3`, `1e12`, `2.5E+4`. The `e` belongs to the
            // number only when digits follow it, directly or after one sign;
            // otherwise it starts the next token as it always did.
            if let Some(&marker) = chars.peek()
                && matches!(marker, 'e' | 'E')
            {
                let mut ahead = chars.clone();
                ahead.next();
                let sign = ahead.next_if(|c| *c == '+' || *c == '-');
                if ahead.peek().is_some_and(char::is_ascii_digit) {
                    s.push(marker);
                    chars.next();
                    col += 1;
                    if let Some(sign) = sign {
                        s.push(sign);
                        chars.next();
                        col += 1;
                    }
                    while let Some(&c) = chars.peek() {
                        if c.is_ascii_digit() {
                            s.push(c);
                            chars.next();
                            col += 1;
                        } else {
                            break;
                        }
                    }
                }
            }
            let num: f64 = s
                .parse()
                .map_err(|e| format!("Invalid number at {line}:{start_col}: {e}"))?;
            // `1e400` parses as infinity. Every number a script holds is
            // finite, so a literal past the largest float is refused here
            // rather than carried into a dimension.
            if !num.is_finite() {
                return Err(format!(
                    "The number {s} is too large to be a finite number at {line}:{start_col}"
                ));
            }
            tokens.push(SpannedToken {
                token: Token::Number(num),
                line,
                col: start_col,
            });
            continue;
        }

        if ch == '"' {
            let start_line = line;
            chars.next();
            col += 1;
            let mut s = String::new();
            let mut closed = false;
            // A newline inside a string is part of it, and the line after
            // it is still a new line for every position reported past it.
            let advance = |line: &mut usize, col: &mut usize, c: char| {
                if c == '\n' {
                    *line += 1;
                    *col = 1;
                } else {
                    *col += 1;
                }
            };
            while let Some(c) = chars.next() {
                advance(&mut line, &mut col, c);
                if c == '"' {
                    closed = true;
                    break;
                } else if c == '\\' {
                    if let Some(escaped) = chars.next() {
                        advance(&mut line, &mut col, escaped);
                        s.push(escaped);
                    }
                } else {
                    s.push(c);
                }
            }
            // A string that runs to the end of the file swallowed the rest
            // of the script; reading on as though it had closed would
            // report whatever came next, or nothing at all.
            if !closed {
                return Err(format!(
                    "A string never closes: the end of the file at {line}:{col} comes before its closing `\"`; the string starts at {start_line}:{start_col}"
                ));
            }
            tokens.push(SpannedToken {
                token: Token::StringLit(s),
                line: start_line,
                col: start_col,
            });
            continue;
        }

        chars.next();
        col += 1;

        // `..` is one token: the range of a `for` loop.
        if ch == '.' && chars.peek() == Some(&'.') {
            chars.next();
            col += 1;
            tokens.push(SpannedToken {
                token: Token::DotDot,
                line,
                col: start_col,
            });
            continue;
        }

        // `->` is one token: a function's return type.
        if ch == '-' && chars.peek() == Some(&'>') {
            chars.next();
            col += 1;
            tokens.push(SpannedToken {
                token: Token::Arrow,
                line,
                col: start_col,
            });
            continue;
        }

        let token = match ch {
            '{' => Token::LBrace,
            '}' => Token::RBrace,
            '(' => Token::LParen,
            ')' => Token::RParen,
            '[' => Token::LBracket,
            ']' => Token::RBracket,
            ':' => Token::Colon,
            ';' => Token::Semi,
            ',' => Token::Comma,
            '=' => Token::Equal,
            '+' => Token::Plus,
            '-' => Token::Minus,
            '*' => Token::Star,
            '.' => Token::Dot,
            _ => return Err(format!("Unexpected character '{ch}' at {line}:{start_col}")),
        };
        tokens.push(SpannedToken {
            token,
            line,
            col: start_col,
        });
    }

    tokens.push(SpannedToken {
        token: Token::Eof,
        line,
        col,
    });
    Ok(tokens)
}
