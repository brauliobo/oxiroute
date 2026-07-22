use std::iter::Peekable;

use crate::{
    DirectiveContext, DirectiveError, DirectiveSpec, ValueKind, directive_specs, validate_directive,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxDirective {
    pub name: String,
    pub args: Vec<String>,
    pub children: Option<Vec<Self>>,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum NginxParseError {
    #[error("invalid nginx-rtmp directive: {0}")]
    Directive(#[from] DirectiveError),
    #[error("unknown directive `{name}` in RTMP context at {line}:{column}")]
    UnknownRtmpDirective {
        name: String,
        line: usize,
        column: usize,
    },
    #[error("unexpected token at {line}:{column}: expected {expected}")]
    UnexpectedToken {
        line: usize,
        column: usize,
        expected: &'static str,
    },
    #[error("unterminated quoted string at {line}:{column}")]
    UnterminatedQuote { line: usize, column: usize },
    #[error("unterminated escape at {line}:{column}")]
    UnterminatedEscape { line: usize, column: usize },
    #[error("directive `{name}` at {line}:{column} must {requirement}")]
    InvalidBlockShape {
        name: String,
        line: usize,
        column: usize,
        requirement: &'static str,
    },
}

/// Parses nginx syntax and validates all nginx-rtmp directives in their effective contexts.
///
/// # Errors
///
/// Returns an error for malformed nginx syntax or invalid nginx-rtmp directives.
pub fn parse_nginx_config(source: &str) -> Result<Vec<NginxDirective>, NginxParseError> {
    let (tokens, end_line, end_column) = Lexer::new(source).tokenize()?;
    Parser {
        tokens,
        index: 0,
        end_line,
        end_column,
    }
    .parse_scope(Scope::NginxMain, false)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scope {
    NginxMain,
    RtmpMain,
    RtmpServer,
    RtmpApplication,
    RtmpRecorder,
    Http,
    Other,
}

impl Scope {
    fn directive_context(self) -> Option<DirectiveContext> {
        match self {
            Self::NginxMain => Some(DirectiveContext::NginxMain),
            Self::RtmpMain => Some(DirectiveContext::RtmpMain),
            Self::RtmpServer => Some(DirectiveContext::RtmpServer),
            Self::RtmpApplication => Some(DirectiveContext::RtmpApplication),
            Self::RtmpRecorder => Some(DirectiveContext::RtmpRecorder),
            Self::Http => Some(DirectiveContext::Http),
            Self::Other => None,
        }
    }

    fn is_rtmp(self) -> bool {
        matches!(
            self,
            Self::RtmpMain | Self::RtmpServer | Self::RtmpApplication | Self::RtmpRecorder
        )
    }

    fn child(self, name: &str) -> Self {
        match (self, name) {
            (Self::NginxMain, "rtmp") => Self::RtmpMain,
            (Self::NginxMain, "http") | (Self::Http, _) => Self::Http,
            (Self::RtmpMain, "server") => Self::RtmpServer,
            (Self::RtmpServer, "application") => Self::RtmpApplication,
            (Self::RtmpApplication, "recorder") => Self::RtmpRecorder,
            (Self::Other | Self::NginxMain, _) => Self::Other,
            (scope, _) => scope,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Token {
    kind: TokenKind,
    line: usize,
    column: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TokenKind {
    Word(String),
    Semicolon,
    OpenBrace,
    CloseBrace,
}

struct Lexer<'a> {
    chars: Peekable<std::str::Chars<'a>>,
    line: usize,
    column: usize,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            chars: source.chars().peekable(),
            line: 1,
            column: 1,
        }
    }

    fn tokenize(mut self) -> Result<(Vec<Token>, usize, usize), NginxParseError> {
        let mut tokens = Vec::new();

        while let Some(character) = self.chars.peek().copied() {
            match character {
                character if character.is_whitespace() => {
                    self.next();
                }
                '#' => self.skip_comment(),
                ';' | '{' | '}' => {
                    let line = self.line;
                    let column = self.column;
                    self.next();
                    let kind = match character {
                        ';' => TokenKind::Semicolon,
                        '{' => TokenKind::OpenBrace,
                        '}' => TokenKind::CloseBrace,
                        _ => unreachable!(),
                    };
                    tokens.push(Token { kind, line, column });
                }
                _ => tokens.push(self.word()?),
            }
        }

        Ok((tokens, self.line, self.column))
    }

    fn word(&mut self) -> Result<Token, NginxParseError> {
        let line = self.line;
        let column = self.column;
        let mut value = String::new();
        let mut consumed = false;

        while let Some(character) = self.chars.peek().copied() {
            if character.is_whitespace() || matches!(character, ';' | '{' | '}' | '#') {
                break;
            }

            consumed = true;
            match character {
                '\'' | '"' => {
                    let quote = self.next().expect("peeked quote");
                    loop {
                        match self.next() {
                            Some(character) if character == quote => break,
                            Some('\\') => {
                                let escaped =
                                    self.next().ok_or(NginxParseError::UnterminatedEscape {
                                        line: self.line,
                                        column: self.column,
                                    })?;
                                value.push(escaped);
                            }
                            Some(character) => value.push(character),
                            None => {
                                return Err(NginxParseError::UnterminatedQuote { line, column });
                            }
                        }
                    }
                }
                '\\' => {
                    self.next();
                    let escaped = self.next().ok_or(NginxParseError::UnterminatedEscape {
                        line: self.line,
                        column: self.column,
                    })?;
                    value.push(escaped);
                }
                _ => value.push(self.next().expect("peeked character")),
            }
        }

        if !consumed {
            return Err(NginxParseError::UnexpectedToken {
                line,
                column,
                expected: "a directive token",
            });
        }

        Ok(Token {
            kind: TokenKind::Word(value),
            line,
            column,
        })
    }

    fn skip_comment(&mut self) {
        while let Some(character) = self.next() {
            if character == '\n' {
                break;
            }
        }
    }

    fn next(&mut self) -> Option<char> {
        let character = self.chars.next()?;
        if character == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(character)
    }
}

struct Parser {
    tokens: Vec<Token>,
    index: usize,
    end_line: usize,
    end_column: usize,
}

impl Parser {
    fn parse_scope(
        &mut self,
        scope: Scope,
        expects_close: bool,
    ) -> Result<Vec<NginxDirective>, NginxParseError> {
        let mut directives = Vec::new();

        loop {
            let Some(token) = self.tokens.get(self.index) else {
                if expects_close {
                    return Err(NginxParseError::UnexpectedToken {
                        line: self.end_line,
                        column: self.end_column,
                        expected: "a closing brace",
                    });
                }
                return Ok(directives);
            };

            if token.kind == TokenKind::CloseBrace {
                if !expects_close {
                    return Err(NginxParseError::UnexpectedToken {
                        line: token.line,
                        column: token.column,
                        expected: "a directive name",
                    });
                }
                self.index += 1;
                return Ok(directives);
            }

            directives.push(self.parse_directive(scope)?);
        }
    }

    fn parse_directive(&mut self, scope: Scope) -> Result<NginxDirective, NginxParseError> {
        let token = self.tokens.get(self.index).expect("caller checked token");
        let TokenKind::Word(name) = &token.kind else {
            return Err(NginxParseError::UnexpectedToken {
                line: token.line,
                column: token.column,
                expected: "a directive name",
            });
        };
        let name = name.clone();
        let line = token.line;
        let column = token.column;
        self.index += 1;

        let mut args = Vec::new();
        while let Some(Token {
            kind: TokenKind::Word(value),
            ..
        }) = self.tokens.get(self.index)
        {
            args.push(value.clone());
            self.index += 1;
        }

        let Some(delimiter) = self.tokens.get(self.index) else {
            return Err(NginxParseError::UnexpectedToken {
                line: self.end_line,
                column: self.end_column,
                expected: "a semicolon or opening brace",
            });
        };
        let is_block = match delimiter.kind {
            TokenKind::Semicolon => false,
            TokenKind::OpenBrace => true,
            _ => {
                return Err(NginxParseError::UnexpectedToken {
                    line: delimiter.line,
                    column: delimiter.column,
                    expected: "a semicolon or opening brace",
                });
            }
        };
        self.index += 1;

        if let Some(spec) = Self::resolve_spec(scope, &name, line, column)? {
            let expects_block = matches!(spec.value_kind, ValueKind::Block | ValueKind::NamedBlock);
            if expects_block != is_block {
                return Err(NginxParseError::InvalidBlockShape {
                    name,
                    line,
                    column,
                    requirement: if expects_block {
                        "use an opening brace"
                    } else {
                        "end with a semicolon"
                    },
                });
            }

            let context = scope
                .directive_context()
                .expect("validated scope has context");
            let values: Vec<_> = args.iter().map(String::as_str).collect();
            validate_directive(&name, context, &values)?;
        }

        let children = if is_block {
            Some(self.parse_scope(scope.child(&name), true)?)
        } else {
            None
        };

        Ok(NginxDirective {
            name,
            args,
            children,
            line,
            column,
        })
    }

    fn resolve_spec(
        scope: Scope,
        name: &str,
        line: usize,
        column: usize,
    ) -> Result<Option<&'static DirectiveSpec>, NginxParseError> {
        let registered = directive_specs().iter().find(|spec| spec.name == name);
        let should_validate = match scope {
            Scope::NginxMain => registered.is_some(),
            Scope::Http => matches!(name, "rtmp_stat" | "rtmp_stat_stylesheet" | "rtmp_control"),
            scope if scope.is_rtmp() => {
                if name == "include" {
                    return Ok(None);
                }
                if registered.is_none() {
                    return Err(NginxParseError::UnknownRtmpDirective {
                        name: name.to_owned(),
                        line,
                        column,
                    });
                }
                true
            }
            _ => false,
        };

        if !should_validate {
            return Ok(None);
        }

        let context = scope
            .directive_context()
            .expect("validated scope has context");
        let spec = registered.expect("registered directive selected for validation");
        if !spec.contexts.contains(&context) {
            return Err(DirectiveError::InvalidContext {
                name: spec.name,
                context,
            }
            .into());
        }
        Ok(Some(spec))
    }
}
