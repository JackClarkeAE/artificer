//! Lexer for tokenizing .art CAD scripts.

#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Param,
    Let,
    For,
    In,
    LBrace,
    RBrace,
    DotDot,
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
            let num: f64 = s
                .parse()
                .map_err(|e| format!("Invalid number at {line}:{start_col}: {e}"))?;
            tokens.push(SpannedToken {
                token: Token::Number(num),
                line,
                col: start_col,
            });
            continue;
        }

        if ch == '"' {
            chars.next();
            col += 1;
            let mut s = String::new();
            while let Some(&c) = chars.peek() {
                if c == '"' {
                    chars.next();
                    col += 1;
                    break;
                } else if c == '\\' {
                    chars.next();
                    col += 1;
                    if let Some(&escaped) = chars.peek() {
                        s.push(escaped);
                        chars.next();
                        col += 1;
                    }
                } else {
                    s.push(c);
                    chars.next();
                    col += 1;
                }
            }
            tokens.push(SpannedToken {
                token: Token::StringLit(s),
                line,
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
