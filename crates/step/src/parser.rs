//! The Part 21 exchange-structure grammar over the scanner's tokens:
//! sections, entity statements, simple and complex instances, and argument
//! lists.

use std::collections::BTreeMap;

use crate::scanner::{Scanner, Token};
use crate::{Entity, Graph, Header, Instance, ParseError, Value};

/// How deep argument lists may nest before the parse is refused.
///
/// The argument parser recurses once per `(`, so without a ceiling a file of
/// nothing but open parentheses would overflow the stack, which aborts the
/// process rather than returning an error. Real STEP arguments nest a
/// handful of levels; 64 is far past anything a writer emits.
const MAX_NESTING: u32 = 64;

struct Parser<'a> {
    scanner: Scanner<'a>,
    lookahead: Option<Token>,
}

impl Parser<'_> {
    fn peek(&mut self) -> Result<&Token, ParseError> {
        if self.lookahead.is_none() {
            self.lookahead = Some(self.scanner.next_token()?);
        }
        Ok(self.lookahead.as_ref().expect("filled above"))
    }

    fn next(&mut self) -> Result<Token, ParseError> {
        match self.lookahead.take() {
            Some(token) => Ok(token),
            None => self.scanner.next_token(),
        }
    }

    fn error(&self, message: impl Into<String>) -> ParseError {
        self.scanner.error(message)
    }

    fn expect(&mut self, wanted: &Token, what: &str) -> Result<(), ParseError> {
        let token = self.next()?;
        if &token == wanted {
            Ok(())
        } else {
            Err(self.error(format!("expected {what}, found {}", describe(&token))))
        }
    }

    fn is_ident(&mut self, name: &str) -> Result<bool, ParseError> {
        Ok(matches!(self.peek()?, Token::Ident(found) if found == name))
    }

    /// Skips to just past the next `ENDSEC;`, for sections this crate does
    /// not read.
    fn skip_section(&mut self) -> Result<(), ParseError> {
        self.lookahead = None;
        self.scanner.skip_to_endsec()
    }

    fn header(&mut self) -> Result<Header, ParseError> {
        let mut header = Header::default();
        loop {
            match self.next()? {
                Token::Ident(name) if name == "ENDSEC" => {
                    self.expect(&Token::Semicolon, "`;` after ENDSEC")?;
                    break;
                }
                Token::Ident(name) => {
                    self.expect(&Token::LParen, "`(` after a header entity name")?;
                    let args = self.arguments(0)?;
                    self.expect(&Token::Semicolon, "`;` after a header entity")?;
                    header.entities.push(Instance { kind: name, args });
                }
                Token::Eof => return Err(self.error("the HEADER section is missing its ENDSEC")),
                other => {
                    return Err(self.error(format!(
                        "expected a header entity, found {}",
                        describe(&other)
                    )));
                }
            }
        }
        let strings = |value: &Value| -> Vec<String> {
            value
                .as_list()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        let string = |value: &Value| value.as_str().unwrap_or("").to_owned();
        for instance in &header.entities {
            match instance.kind.as_str() {
                "FILE_DESCRIPTION" => {
                    header.description = strings(instance.arg(0));
                    header.implementation_level = string(instance.arg(1));
                }
                "FILE_NAME" => {
                    header.name = string(instance.arg(0));
                    header.time_stamp = string(instance.arg(1));
                    header.author = strings(instance.arg(2));
                    header.organization = strings(instance.arg(3));
                    header.preprocessor_version = string(instance.arg(4));
                    header.originating_system = string(instance.arg(5));
                    header.authorization = string(instance.arg(6));
                }
                "FILE_SCHEMA" => header.schema = strings(instance.arg(0)),
                _ => {}
            }
        }
        Ok(header)
    }

    fn data(&mut self, entities: &mut BTreeMap<u64, Entity>) -> Result<(), ParseError> {
        // `DATA;` or `DATA('name', ('schema'));`.
        if self.peek()? == &Token::LParen {
            self.next()?;
            self.arguments(0)?;
        }
        self.expect(&Token::Semicolon, "`;` after DATA")?;
        loop {
            match self.next()? {
                Token::Ident(name) if name == "ENDSEC" => {
                    self.expect(&Token::Semicolon, "`;` after ENDSEC")?;
                    return Ok(());
                }
                Token::Ref(id) => {
                    self.expect(&Token::Equals, "`=` after an entity number")?;
                    let entity = self.entity(id)?;
                    self.expect(&Token::Semicolon, "`;` after an entity")?;
                    if entities.insert(id, entity).is_some() {
                        return Err(self.error(format!("entity #{id} is defined twice")));
                    }
                }
                Token::Eof => return Err(self.error("the DATA section is missing its ENDSEC")),
                other => {
                    return Err(self.error(format!(
                        "expected an entity statement, found {}",
                        describe(&other)
                    )));
                }
            }
        }
    }

    /// `KIND(args)`, `!KIND(args)`, or `(KIND(args)KIND(args)...)`.
    fn entity(&mut self, id: u64) -> Result<Entity, ParseError> {
        match self.next()? {
            Token::Ident(kind) => {
                self.expect(&Token::LParen, "`(` after an entity name")?;
                let args = self.arguments(0)?;
                Ok(Entity {
                    id,
                    user_defined: false,
                    instances: vec![Instance { kind, args }],
                })
            }
            Token::UserIdent(kind) => {
                self.expect(&Token::LParen, "`(` after an entity name")?;
                let args = self.arguments(0)?;
                Ok(Entity {
                    id,
                    user_defined: true,
                    instances: vec![Instance { kind, args }],
                })
            }
            Token::LParen => {
                let mut instances = Vec::new();
                loop {
                    match self.next()? {
                        Token::RParen => break,
                        Token::Ident(kind) => {
                            self.expect(&Token::LParen, "`(` after a supertype name")?;
                            let args = self.arguments(0)?;
                            instances.push(Instance { kind, args });
                        }
                        other => {
                            return Err(self.error(format!(
                                "expected a supertype of complex entity #{id}, found {}",
                                describe(&other)
                            )));
                        }
                    }
                }
                if instances.is_empty() {
                    return Err(self.error(format!("complex entity #{id} lists no types")));
                }
                Ok(Entity {
                    id,
                    user_defined: false,
                    instances,
                })
            }
            other => Err(self.error(format!(
                "expected an entity after `#{id}=`, found {}",
                describe(&other)
            ))),
        }
    }

    /// The values up to and including the closing `)`; the opening `(` has
    /// been consumed.
    fn arguments(&mut self, depth: u32) -> Result<Vec<Value>, ParseError> {
        if depth > MAX_NESTING {
            return Err(self.error(format!(
                "argument lists nest deeper than {MAX_NESTING} levels"
            )));
        }
        let mut values = Vec::new();
        if self.peek()? == &Token::RParen {
            self.next()?;
            return Ok(values);
        }
        loop {
            values.push(self.value(depth)?);
            match self.next()? {
                Token::Comma => {}
                Token::RParen => return Ok(values),
                other => {
                    return Err(self.error(format!(
                        "expected `,` or `)` in an argument list, found {}",
                        describe(&other)
                    )));
                }
            }
        }
    }

    fn value(&mut self, depth: u32) -> Result<Value, ParseError> {
        Ok(match self.next()? {
            Token::Ref(id) => Value::Ref(id),
            Token::Real(value) => Value::Real(value),
            Token::Integer(value) => Value::Integer(value),
            Token::Str(text) => Value::Str(text),
            Token::Enum(name) => Value::Enum(name),
            Token::Dollar => Value::Null,
            Token::Star => Value::Derived,
            Token::LParen => Value::List(self.arguments(depth + 1)?),
            Token::Ident(name) => {
                // A typed parameter: `LENGTH_MEASURE(1.E-6)`.
                self.expect(&Token::LParen, &format!("`(` after the type name {name}"))?;
                let inner = self.value(depth + 1)?;
                self.expect(
                    &Token::RParen,
                    &format!("`)` closing the typed parameter {name}"),
                )?;
                Value::Typed(name, Box::new(inner))
            }
            other => {
                return Err(self.error(format!("expected an argument, found {}", describe(&other))));
            }
        })
    }
}

fn describe(token: &Token) -> String {
    match token {
        Token::Ref(id) => format!("`#{id}`"),
        Token::Ident(name) => format!("`{name}`"),
        Token::UserIdent(name) => format!("`!{name}`"),
        Token::Str(text) => format!("the string '{text}'"),
        Token::Enum(name) => format!("`.{name}.`"),
        Token::Real(value) => format!("the number {value}"),
        Token::Integer(value) => format!("the number {value}"),
        Token::Dollar => "`$`".to_owned(),
        Token::Star => "`*`".to_owned(),
        Token::LParen => "`(`".to_owned(),
        Token::RParen => "`)`".to_owned(),
        Token::Comma => "`,`".to_owned(),
        Token::Equals => "`=`".to_owned(),
        Token::Semicolon => "`;`".to_owned(),
        Token::Other(character) => format!("`{character}`"),
        Token::Eof => "the end of the file".to_owned(),
    }
}

pub(crate) fn parse(text: &str) -> Result<(Header, Graph), ParseError> {
    let mut parser = Parser {
        scanner: Scanner::new(text),
        lookahead: None,
    };
    if !parser.is_ident("ISO-10303-21")? {
        return Err(parser.error("a Part 21 file starts with `ISO-10303-21;`"));
    }
    parser.next()?;
    parser.expect(&Token::Semicolon, "`;` after ISO-10303-21")?;
    let mut header = Header::default();
    let mut entities = BTreeMap::new();
    let mut seen_header = false;
    loop {
        match parser.next()? {
            Token::Ident(name) if name == "END-ISO-10303-21" => {
                // A trailing `;` is conventional; its absence is not an error.
                break;
            }
            Token::Ident(name) if name == "HEADER" => {
                parser.expect(&Token::Semicolon, "`;` after HEADER")?;
                header = parser.header()?;
                seen_header = true;
            }
            Token::Ident(name) if name == "DATA" => {
                parser.data(&mut entities)?;
            }
            Token::Ident(name) if matches!(name.as_str(), "ANCHOR" | "REFERENCE" | "SIGNATURE") => {
                parser.expect(&Token::Semicolon, &format!("`;` after {name}"))?;
                parser.skip_section()?;
            }
            Token::Eof => {
                if seen_header || !entities.is_empty() {
                    // A file cut off after its last section still holds what
                    // it holds; the missing trailer is tolerated.
                    break;
                }
                return Err(parser.error("the file has no HEADER or DATA section"));
            }
            other => {
                return Err(parser.error(format!("expected a section, found {}", describe(&other))));
            }
        }
    }
    Ok((header, Graph { entities }))
}
