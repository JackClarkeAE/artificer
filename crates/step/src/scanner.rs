//! The Part 21 tokenizer: bytes to tokens, with comments dropped, strings
//! unescaped, and the line of every token remembered for error messages.

use crate::ParseError;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Token {
    /// `#n`.
    Ref(u64),
    /// A keyword: an entity or section name, or a typed-parameter name.
    Ident(String),
    /// `!NAME`: a user-defined entity name.
    UserIdent(String),
    Str(String),
    /// `.NAME.`
    Enum(String),
    Real(f64),
    Integer(i64),
    Dollar,
    Star,
    LParen,
    RParen,
    Comma,
    Equals,
    Semicolon,
    /// Anything else, kept so an unknown section can be skipped token by
    /// token until its `ENDSEC`.
    Other(char),
    Eof,
}

pub(crate) struct Scanner<'a> {
    bytes: &'a [u8],
    at: usize,
    line: usize,
}

impl<'a> Scanner<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        Self {
            bytes: text.as_bytes(),
            at: 0,
            line: 1,
        }
    }

    /// Skips raw text to just past the next `ENDSEC;`, for the sections
    /// this crate does not read, whose contents (anchors, URLs, signatures)
    /// are not Part 21 tokens.
    pub(crate) fn skip_to_endsec(&mut self) -> Result<(), ParseError> {
        let start = self.line;
        const MARKER: &[u8] = b"ENDSEC;";
        loop {
            if self.bytes[self.at..].starts_with(MARKER) {
                self.at += MARKER.len();
                return Ok(());
            }
            if self.advance().is_none() {
                return Err(ParseError {
                    message: "a section is missing its ENDSEC".to_owned(),
                    line: start,
                });
            }
        }
    }

    pub(crate) fn error(&self, message: impl Into<String>) -> ParseError {
        ParseError {
            message: message.into(),
            line: self.line,
        }
    }

    fn peek_byte(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let byte = self.peek_byte()?;
        self.at += 1;
        if byte == b'\n' {
            self.line += 1;
        }
        Some(byte)
    }

    /// Skips whitespace and `/* */` comments.
    fn skip_blank(&mut self) -> Result<(), ParseError> {
        loop {
            match self.peek_byte() {
                Some(byte) if byte.is_ascii_whitespace() => {
                    self.advance();
                }
                Some(b'/') if self.bytes.get(self.at + 1) == Some(&b'*') => {
                    let start = self.line;
                    self.at += 2;
                    loop {
                        match self.advance() {
                            None => {
                                return Err(ParseError {
                                    message: "unterminated comment".to_owned(),
                                    line: start,
                                });
                            }
                            Some(b'*') if self.peek_byte() == Some(b'/') => {
                                self.at += 1;
                                break;
                            }
                            Some(_) => {}
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    pub(crate) fn next_token(&mut self) -> Result<Token, ParseError> {
        self.skip_blank()?;
        let Some(byte) = self.peek_byte() else {
            return Ok(Token::Eof);
        };
        match byte {
            b'#' => {
                self.advance();
                let start = self.at;
                while self.peek_byte().is_some_and(|byte| byte.is_ascii_digit()) {
                    self.at += 1;
                }
                let digits = std::str::from_utf8(&self.bytes[start..self.at]).unwrap_or("");
                digits
                    .parse()
                    .map(Token::Ref)
                    .map_err(|_| self.error("an entity reference needs a number after `#`"))
            }
            b'\'' => self.string(),
            b'"' => {
                // A binary literal: kept as text, apostrophes and all.
                self.advance();
                let start = self.at;
                while let Some(byte) = self.peek_byte() {
                    if byte == b'"' {
                        break;
                    }
                    self.advance();
                }
                let text = String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned();
                if self.advance().is_none() {
                    return Err(self.error("unterminated binary literal"));
                }
                Ok(Token::Str(text))
            }
            b'.' if self
                .bytes
                .get(self.at + 1)
                .is_some_and(|next| next.is_ascii_alphabetic() || *next == b'_') =>
            {
                self.advance();
                let start = self.at;
                while self
                    .peek_byte()
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    self.at += 1;
                }
                let name = String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned();
                if self.peek_byte() != Some(b'.') {
                    return Err(
                        self.error(format!("enumeration `.{name}` is missing its closing dot"))
                    );
                }
                self.advance();
                Ok(Token::Enum(name))
            }
            b'$' => {
                self.advance();
                Ok(Token::Dollar)
            }
            b'*' => {
                self.advance();
                Ok(Token::Star)
            }
            b'(' => {
                self.advance();
                Ok(Token::LParen)
            }
            b')' => {
                self.advance();
                Ok(Token::RParen)
            }
            b',' => {
                self.advance();
                Ok(Token::Comma)
            }
            b'=' => {
                self.advance();
                Ok(Token::Equals)
            }
            b';' => {
                self.advance();
                Ok(Token::Semicolon)
            }
            b'!' => {
                self.advance();
                let name = self.identifier();
                if name.is_empty() {
                    return Err(self.error("a user-defined entity needs a name after `!`"));
                }
                Ok(Token::UserIdent(name))
            }
            byte if byte.is_ascii_digit() || byte == b'-' || byte == b'+' => self.number(),
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                Ok(Token::Ident(self.identifier()))
            }
            other => {
                self.advance();
                Ok(Token::Other(other as char))
            }
        }
    }

    /// `[A-Za-z_][A-Za-z0-9_-]*`: keywords, and the hyphenated
    /// `ISO-10303-21` and `END-ISO-10303-21`.
    fn identifier(&mut self) -> String {
        let start = self.at;
        while self
            .peek_byte()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            self.at += 1;
        }
        String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned()
    }

    fn number(&mut self) -> Result<Token, ParseError> {
        let start = self.at;
        if matches!(self.peek_byte(), Some(b'-' | b'+')) {
            self.at += 1;
        }
        let mut real = false;
        while let Some(byte) = self.peek_byte() {
            match byte {
                b'0'..=b'9' => self.at += 1,
                b'.' => {
                    real = true;
                    self.at += 1;
                }
                b'E' | b'e' => {
                    real = true;
                    self.at += 1;
                    if matches!(self.peek_byte(), Some(b'-' | b'+')) {
                        self.at += 1;
                    }
                }
                _ => break,
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.at]).unwrap_or("");
        if real {
            // `1.` and `-.5` are not numbers to Rust; `1.0` and `-0.5` are.
            let mut normalised = text.replace('e', "E");
            if normalised.ends_with('.') {
                normalised.push('0');
            }
            if let Some(dot) = normalised.find('.') {
                if dot == 0 || !normalised.as_bytes()[dot - 1].is_ascii_digit() {
                    normalised.insert(dot, '0');
                }
                if normalised
                    .as_bytes()
                    .get(dot + 1)
                    .is_some_and(|next| *next == b'E')
                {
                    normalised.insert(dot + 1, '0');
                }
            }
            normalised
                .parse::<f64>()
                .map(Token::Real)
                .map_err(|_| self.error(format!("`{text}` is not a number")))
        } else {
            text.parse::<i64>()
                .map(Token::Integer)
                .map_err(|_| self.error(format!("`{text}` is not a number")))
        }
    }

    /// `'...'` with `''` as an apostrophe. The `\X2\...\X0\` and `\S\`
    /// encodings of ISO 8859 and Unicode characters are decoded where they
    /// are well formed and kept as written otherwise.
    fn string(&mut self) -> Result<Token, ParseError> {
        let start_line = self.line;
        self.advance();
        let mut raw = Vec::new();
        loop {
            match self.advance() {
                None => {
                    return Err(ParseError {
                        message: "unterminated string".to_owned(),
                        line: start_line,
                    });
                }
                Some(b'\'') => {
                    if self.peek_byte() == Some(b'\'') {
                        self.advance();
                        raw.push(b'\'');
                    } else {
                        break;
                    }
                }
                Some(byte) => raw.push(byte),
            }
        }
        Ok(Token::Str(decode_control_directives(
            &String::from_utf8_lossy(&raw),
        )))
    }
}

/// Decodes `\X2\<hex4>*\X0\`, `\X4\<hex8>*\X0\` and `\X\<hex2>`, the Part
/// 21 spellings of characters outside the basic set, leaving any malformed
/// directive as written.
fn decode_control_directives(text: &str) -> String {
    if !text.contains('\\') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        let directive = &rest[at..];
        if let Some(after) = directive.strip_prefix("\\X2\\") {
            if let Some(end) = after.find("\\X0\\") {
                let hex = &after[..end];
                if let Some(decoded) = decode_hex_units(hex, 4) {
                    out.push_str(&decoded);
                    rest = &after[end + 4..];
                    continue;
                }
            }
        } else if let Some(after) = directive.strip_prefix("\\X4\\") {
            if let Some(end) = after.find("\\X0\\") {
                let hex = &after[..end];
                if let Some(decoded) = decode_hex_units(hex, 8) {
                    out.push_str(&decoded);
                    rest = &after[end + 4..];
                    continue;
                }
            }
        } else if let Some(after) = directive.strip_prefix("\\X\\")
            && after.len() >= 2
            && let Ok(code) = u32::from_str_radix(&after[..2], 16)
            && let Some(character) = char::from_u32(code)
        {
            out.push(character);
            rest = &after[2..];
            continue;
        }
        out.push('\\');
        rest = &directive[1..];
    }
    out.push_str(rest);
    out
}

fn decode_hex_units(hex: &str, width: usize) -> Option<String> {
    if hex.is_empty() || !hex.len().is_multiple_of(width) {
        return None;
    }
    let mut out = String::new();
    for unit in hex.as_bytes().chunks(width) {
        let text = std::str::from_utf8(unit).ok()?;
        let code = u32::from_str_radix(text, 16).ok()?;
        out.push(char::from_u32(code)?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(text: &str) -> Vec<Token> {
        let mut scanner = Scanner::new(text);
        let mut out = Vec::new();
        loop {
            let token = scanner.next_token().expect("scans");
            if token == Token::Eof {
                return out;
            }
            out.push(token);
        }
    }

    #[test]
    fn every_token_kind_scans() {
        assert_eq!(
            tokens("#12=NAME('a''b',.T.,$,*,-1.5E-3,7,(1.,2.));"),
            vec![
                Token::Ref(12),
                Token::Equals,
                Token::Ident("NAME".to_owned()),
                Token::LParen,
                Token::Str("a'b".to_owned()),
                Token::Comma,
                Token::Enum("T".to_owned()),
                Token::Comma,
                Token::Dollar,
                Token::Comma,
                Token::Star,
                Token::Comma,
                Token::Real(-1.5e-3),
                Token::Comma,
                Token::Integer(7),
                Token::Comma,
                Token::LParen,
                Token::Real(1.0),
                Token::Comma,
                Token::Real(2.0),
                Token::RParen,
                Token::RParen,
                Token::Semicolon,
            ]
        );
    }

    #[test]
    fn odd_real_spellings_scan() {
        assert_eq!(tokens("1."), vec![Token::Real(1.0)]);
        assert_eq!(tokens("-.5"), vec![Token::Real(-0.5)]);
        assert_eq!(tokens("1.E-7"), vec![Token::Real(1.0e-7)]);
        assert_eq!(tokens("2.5e3"), vec![Token::Real(2500.0)]);
        assert_eq!(tokens("+3"), vec![Token::Integer(3)]);
    }

    #[test]
    fn comments_and_newlines_are_blank() {
        assert_eq!(
            tokens("A /* x\n y */ (\n1,\n2)"),
            vec![
                Token::Ident("A".to_owned()),
                Token::LParen,
                Token::Integer(1),
                Token::Comma,
                Token::Integer(2),
                Token::RParen
            ]
        );
        let mut scanner = Scanner::new("\n\n#1");
        assert_eq!(scanner.next_token().unwrap(), Token::Ref(1));
        assert_eq!(scanner.line, 3);
        let mut scanner = Scanner::new("<a.b>=#1;\nENDSEC;\n#2");
        scanner.skip_to_endsec().unwrap();
        assert_eq!(scanner.next_token().unwrap(), Token::Ref(2));
    }

    #[test]
    fn control_directives_decode() {
        assert_eq!(
            decode_control_directives("Espa\\X2\\00E7\\X0\\ador"),
            "Espaçador"
        );
        assert_eq!(
            decode_control_directives("\\X\\E9t\\X4\\0001F600\\X0\\"),
            "ét😀"
        );
        assert_eq!(decode_control_directives("a\\bad"), "a\\bad");
    }
}
