#[must_use]
pub fn parse(source: &SourceFile) -> Report<Document> {
    parse_with_limits(source, ParserLimits::default())
}

#[must_use]
pub fn parse_with_limits(source: &SourceFile, limits: ParserLimits) -> Report<Document> {
    let (tokens, mut diagnostics) = lex(source, limits.source_bytes, limits.tokens).into_parts();
    let stop_offset = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code() == E_SOURCE_LIMIT)
        .filter_map(Diagnostic::primary_span)
        .map(|span| span.range().start())
        .min()
        .unwrap_or(source.len());
    let mut parser = Parser {
        tokens: &tokens,
        source: source.id(),
        source_len: source.len(),
        stop_offset,
        index: 0,
        statement_count: 0,
        statement_limit_reached: false,
        limits,
        diagnostics: Vec::new(),
    };
    let declarations = parser.parse_document();
    diagnostics.extend(parser.diagnostics);
    Report::new(
        Document {
            declarations,
            span: source.full_span(),
        },
        diagnostics,
    )
}

struct Parser<'a> {
    tokens: &'a [Token],
    source: SourceId,
    source_len: usize,
    stop_offset: usize,
    index: usize,
    statement_count: usize,
    statement_limit_reached: bool,
    limits: ParserLimits,
    diagnostics: Vec<Diagnostic>,
}

impl Parser<'_> {
    fn parse_document(&mut self) -> Vec<Declaration> {
        let mut declarations = Vec::new();
        while self.peek().is_some() {
            declarations.push(self.parse_declaration());
        }
        declarations
    }

    fn parse_declaration(&mut self) -> Declaration {
        let start = self.current_start();
        let Some(keyword) = self.take_value() else {
            let span = self.take_one_span();
            self.syntax("expected a VCL declaration", span);
            return Declaration::Unsupported {
                keyword: None,
                span,
            };
        };

        match keyword.bytes.as_slice() {
            b"vcl" => {
                let value = self.require_value("expected a VCL version after `vcl`");
                let end = self.require_semicolon();
                Declaration::Version {
                    value,
                    span: self.span(start, end),
                }
            }
            b"include" => {
                let glob = if self.take_if(TokenKindTag::Plus).is_some() {
                    let marker = self.require_word("expected `glob` after `include +`");
                    if marker.bytes != b"glob" {
                        self.syntax("expected `glob` after `include +`", marker.span);
                    }
                    true
                } else {
                    false
                };
                let path = self.require_string("expected a quoted VCL include path");
                let end = self.require_semicolon();
                Declaration::Include(IncludeDeclaration {
                    glob,
                    path,
                    span: self.span(start, end),
                })
            }
            b"import" => self.parse_import(start),
            b"acl" => self.parse_acl(start),
            b"probe" => self.parse_probe(start),
            b"backend" => self.parse_backend(start),
            b"director" => self.parse_director(start),
            b"sub" => self.parse_subroutine(start),
            _ => {
                let end = self.skip_declaration();
                Declaration::Unsupported {
                    keyword: Some(keyword),
                    span: self.span(start, end),
                }
            }
        }
    }

    fn parse_import(&mut self, start: usize) -> Declaration {
        let module = self.require_word("expected a VMOD name after `import`");
        let alias = if self.at_word(b"as") {
            self.index += 1;
            Some(self.require_word("expected a VMOD alias after `as`"))
        } else {
            None
        };
        let from = if self.at_word(b"from") {
            self.index += 1;
            Some(self.require_string("expected a quoted VMOD path after `from`"))
        } else {
            None
        };
        let end = self.require_semicolon();
        Declaration::Import(ImportDeclaration {
            module,
            alias,
            from,
            span: self.span(start, end),
        })
    }

    fn parse_acl(&mut self, start: usize) -> Declaration {
        let name = self.require_word("expected an ACL name");
        let open = self.require_open_brace("expected `{` after ACL name");
        let mut entries = Vec::new();
        while let Some(token) = self.peek().cloned() {
            if matches!(token.kind, TokenKind::CloseBrace) {
                self.index += 1;
                return Declaration::Acl(AclDeclaration {
                    name,
                    entries,
                    span: self.span(start, token.span.range().end()),
                });
            }
            let entry_start = token.span.range().start();
            let negated = self.take_if(TokenKindTag::Not).is_some();
            let optional = self.take_if(TokenKindTag::OpenParen).is_some();
            let value = self.require_string("expected a quoted ACL entry");
            let mask = if self.take_if(TokenKindTag::Slash).is_some() {
                Some(self.require_value("expected an ACL prefix length after `/`"))
            } else {
                None
            };
            if optional {
                self.require_close_paren();
            }
            let end = self.require_semicolon();
            entries.push(AclEntry {
                negated,
                optional,
                value,
                mask,
                span: self.span(entry_start, end),
            });
        }
        self.unclosed(open, "ACL block");
        Declaration::Acl(AclDeclaration {
            name,
            entries,
            span: self.span(start, self.stop_offset),
        })
    }

    fn parse_probe(&mut self, start: usize) -> Declaration {
        let name = self.require_word("expected a probe name");
        let open = self.require_open_brace("expected `{` after probe name");
        let (properties, end) = self.parse_property_block(open, 1);
        Declaration::Probe(ProbeDeclaration {
            name,
            properties,
            span: self.span(start, end),
        })
    }

    fn parse_backend(&mut self, start: usize) -> Declaration {
        let name = self.require_word("expected a backend name");
        self.take_if(TokenKindTag::Assign);
        if self.at_word(b"none") {
            self.index += 1;
            let end = self.require_semicolon();
            return Declaration::Backend(BackendDeclaration {
                name,
                kind: BackendDeclarationKind::None,
                properties: Vec::new(),
                span: self.span(start, end),
            });
        }
        let open = self.require_open_brace("expected `{` after backend name");
        let (properties, end) = self.parse_property_block(open, 1);
        Declaration::Backend(BackendDeclaration {
            name,
            kind: BackendDeclarationKind::Endpoint,
            properties,
            span: self.span(start, end),
        })
    }

    fn parse_director(&mut self, start: usize) -> Declaration {
        let name = self.require_word("expected a director name");
        let policy = self.require_word("expected a director policy");
        let open = self.require_open_brace("expected `{` after director policy");
        let mut entries = Vec::new();
        let mut properties = Vec::new();

        while let Some(token) = self.peek().cloned() {
            match token.kind {
                TokenKind::CloseBrace => {
                    let end = token.span.range().end();
                    self.index += 1;
                    return Declaration::Director(DirectorDeclaration {
                        name,
                        policy,
                        entries,
                        properties,
                        span: self.span(start, end),
                    });
                }
                TokenKind::OpenBrace => {
                    let entry_start = token.span.range().start();
                    let opening = token.span;
                    self.index += 1;
                    let (entry_properties, end) = self.parse_property_block(opening, 2);
                    entries.push(DirectorEntry {
                        properties: entry_properties,
                        span: self.span(entry_start, end),
                    });
                }
                TokenKind::Word(_) => {
                    properties.push(self.parse_assignment_statement());
                }
                _ => {
                    let span = self.take_one_span();
                    self.syntax("expected a director property or member block", span);
                }
            }
        }

        self.unclosed(open, "director block");
        Declaration::Director(DirectorDeclaration {
            name,
            policy,
            entries,
            properties,
            span: self.span(start, self.stop_offset),
        })
    }

    fn parse_subroutine(&mut self, start: usize) -> Declaration {
        let name = self.require_word("expected a subroutine name");
        let open = self.require_open_brace("expected `{` after subroutine name");
        let (statements, end) = self.parse_statement_block(open, 1);
        Declaration::Subroutine(SubroutineDeclaration {
            name,
            statements,
            span: self.span(start, end),
        })
    }

    fn parse_property_block(&mut self, opening: Span, depth: usize) -> (Vec<Assignment>, usize) {
        if depth > self.limits.structural_depth {
            self.depth_limit(opening);
            return (Vec::new(), self.skip_block(opening));
        }
        let mut properties = Vec::new();
        while let Some(token) = self.peek().cloned() {
            if matches!(token.kind, TokenKind::CloseBrace) {
                let end = token.span.range().end();
                self.index += 1;
                return (properties, end);
            }
            if matches!(token.kind, TokenKind::Word(_)) {
                properties.push(self.parse_assignment_statement());
            } else {
                let span = self.take_one_span();
                self.syntax("expected a property assignment", span);
            }
        }
        self.unclosed(opening, "property block");
        (properties, self.stop_offset)
    }

    fn parse_statement_block(&mut self, opening: Span, depth: usize) -> (Vec<Statement>, usize) {
        if depth > self.limits.structural_depth {
            self.depth_limit(opening);
            return (Vec::new(), self.skip_block(opening));
        }
        if self.statement_limit_reached {
            return (Vec::new(), self.skip_block(opening));
        }
        let mut statements = Vec::new();
        while let Some(token) = self.peek().cloned() {
            if matches!(token.kind, TokenKind::CloseBrace) {
                let end = token.span.range().end();
                self.index += 1;
                return (statements, end);
            }
            if self.statement_count == self.limits.statements {
                self.statement_limit_reached = true;
                self.diagnostics.push(
                    Diagnostic::new(
                        E_SOURCE_LIMIT,
                        Severity::Error,
                        DiagnosticStage::Parse,
                        format!(
                            "VCL statement count exceeds the maximum of {} per source",
                            self.limits.statements
                        ),
                    )
                    .with_primary_span(token.span),
                );
                return (statements, self.skip_block(opening));
            }
            self.statement_count += 1;
            statements.push(self.parse_statement(depth));
        }
        self.unclosed(opening, "statement block");
        (statements, self.stop_offset)
    }

    fn parse_statement(&mut self, depth: usize) -> Statement {
        let start = self.current_start();
        if self.at_word(b"if") {
            return self.parse_if(depth, start);
        }
        if self.at_word(b"set") {
            self.index += 1;
            let assignment = self.parse_assignment_statement();
            return Statement {
                span: self.span(start, assignment.span.range().end()),
                kind: StatementKind::Set(assignment),
            };
        }
        if self.at_word(b"unset") {
            self.index += 1;
            let target = self.require_word("expected a field after `unset`");
            let end = self.require_semicolon();
            return Statement {
                kind: StatementKind::Unset(target),
                span: self.span(start, end),
            };
        }
        if self.at_word(b"return") {
            self.index += 1;
            let expression = if self.take_if(TokenKindTag::OpenParen).is_some() {
                let expression = self.parse_expression(0);
                self.require_close_paren();
                expression
            } else {
                self.parse_expression(0)
            };
            let end = self.require_semicolon();
            return Statement {
                kind: StatementKind::Return(expression),
                span: self.span(start, end),
            };
        }
        if self.at_word(b"call") {
            self.index += 1;
            let target = self.require_word("expected a subroutine name after `call`");
            let end = self.require_semicolon();
            return Statement {
                kind: StatementKind::Call(target),
                span: self.span(start, end),
            };
        }
        if self.at_word(b"new") {
            self.index += 1;
            let name = self.require_word("expected an object name after `new`");
            if self.take_if(TokenKindTag::Assign).is_none() {
                self.syntax("expected `=` in object declaration", self.current_span());
            }
            let constructor = self.parse_expression(0);
            let end = self.require_semicolon();
            return Statement {
                kind: StatementKind::New(NewObjectStatement { name, constructor }),
                span: self.span(start, end),
            };
        }
        if matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::InlineC)
        ) {
            let span = self.take_one_span();
            return Statement {
                kind: StatementKind::InlineC,
                span,
            };
        }

        let expression = self.parse_expression(0);
        let end = self.require_semicolon();
        let kind = if matches!(expression.kind, ExpressionKind::Invalid) {
            StatementKind::Invalid
        } else {
            StatementKind::Expression(expression)
        };
        Statement {
            kind,
            span: self.span(start, end),
        }
    }

    fn parse_if(&mut self, depth: usize, start: usize) -> Statement {
        self.index += 1;
        let mut branches = Vec::new();
        let mut otherwise = Vec::new();
        let mut statement_end;
        loop {
            let branch_start = self.current_start();
            self.require_open_paren();
            let condition = self.parse_expression(0);
            self.require_close_paren();
            let open = self.require_open_brace("expected `{` after VCL condition");
            let (statements, end) = self.parse_statement_block(open, depth + 1);
            statement_end = end;
            branches.push(ConditionalBranch {
                condition,
                statements,
                span: self.span(branch_start, end),
            });

            if self.at_word(b"elseif") {
                self.index += 1;
                continue;
            }

            if self.at_word(b"else") {
                self.index += 1;
                if self.at_word(b"if") {
                    self.index += 1;
                    continue;
                }
                let open = self.require_open_brace("expected `{` after `else`");
                (otherwise, statement_end) = self.parse_statement_block(open, depth + 1);
            }
            break;
        }
        Statement {
            kind: StatementKind::If(IfStatement {
                branches,
                otherwise,
            }),
            span: self.span(start, statement_end.min(self.source_len)),
        }
    }

    fn parse_assignment_statement(&mut self) -> Assignment {
        let start = self.current_start();
        let target = self.require_word("expected an assignment target");
        let operator = match self.peek().map(|token| &token.kind) {
            Some(TokenKind::Assign) => AssignmentOperator::Set,
            Some(TokenKind::AddAssign) => AssignmentOperator::Add,
            Some(TokenKind::SubtractAssign) => AssignmentOperator::Subtract,
            Some(TokenKind::MultiplyAssign) => AssignmentOperator::Multiply,
            Some(TokenKind::DivideAssign) => AssignmentOperator::Divide,
            _ => {
                self.syntax("expected an assignment operator", self.current_span());
                AssignmentOperator::Set
            }
        };
        if matches!(
            self.peek().map(|token| &token.kind),
            Some(
                TokenKind::Assign
                    | TokenKind::AddAssign
                    | TokenKind::SubtractAssign
                    | TokenKind::MultiplyAssign
                    | TokenKind::DivideAssign
            )
        ) {
            self.index += 1;
        }
        let value = self.parse_expression(0);
        let end = if matches!(value.kind, ExpressionKind::Object(_)) {
            self.take_if(TokenKindTag::Semicolon)
                .map_or(value.span.range().end(), |span| span.range().end())
        } else {
            self.require_semicolon()
        };
        Assignment {
            target,
            operator,
            value,
            span: self.span(start, end),
        }
    }

    fn parse_expression(&mut self, min_binding_power: u8) -> Expression {
        let mut left = self.parse_prefix();
        loop {
            if matches!(
                self.peek().map(|token| &token.kind),
                Some(TokenKind::OpenParen)
            ) {
                let start = left.span.range().start();
                self.index += 1;
                let mut arguments = Vec::new();
                if !matches!(
                    self.peek().map(|token| &token.kind),
                    Some(TokenKind::CloseParen)
                ) {
                    loop {
                        arguments.push(self.parse_expression(0));
                        if self.take_if(TokenKindTag::Comma).is_none() {
                            break;
                        }
                    }
                }
                let end = self.require_close_paren();
                left = Expression {
                    kind: ExpressionKind::Call {
                        function: Box::new(left),
                        arguments,
                    },
                    span: self.span(start, end),
                };
                continue;
            }

            if matches!(
                self.peek().map(|token| &token.kind),
                Some(TokenKind::String(_))
            ) {
                let right = self.parse_prefix();
                let span = self.span(left.span.range().start(), right.span.range().end());
                left = Expression {
                    kind: ExpressionKind::Binary {
                        left: Box::new(left),
                        operator: BinaryOperator::Concatenate,
                        right: Box::new(right),
                    },
                    span,
                };
                continue;
            }

            let Some((operator, left_power, right_power)) = self.binary_operator() else {
                break;
            };
            if left_power < min_binding_power {
                break;
            }
            self.index += 1;
            let right = self.parse_expression(right_power);
            let span = self.span(left.span.range().start(), right.span.range().end());
            left = Expression {
                kind: ExpressionKind::Binary {
                    left: Box::new(left),
                    operator,
                    right: Box::new(right),
                },
                span,
            };
        }
        left
    }

    fn parse_prefix(&mut self) -> Expression {
        let Some(token) = self.peek().cloned() else {
            let span = self.end_span();
            self.syntax("expected a VCL expression", span);
            return Expression {
                kind: ExpressionKind::Invalid,
                span,
            };
        };
        match token.kind {
            TokenKind::Word(bytes) => {
                self.index += 1;
                let value = Value {
                    bytes,
                    span: token.span,
                };
                Expression {
                    kind: ExpressionKind::Name(value),
                    span: token.span,
                }
            }
            TokenKind::String(bytes) => {
                self.index += 1;
                let value = Value {
                    bytes,
                    span: token.span,
                };
                Expression {
                    kind: ExpressionKind::Literal(Literal::String(value)),
                    span: token.span,
                }
            }
            TokenKind::Number(bytes) => {
                self.index += 1;
                let value = Value {
                    bytes,
                    span: token.span,
                };
                Expression {
                    kind: ExpressionKind::Literal(Literal::Number(value)),
                    span: token.span,
                }
            }
            TokenKind::Not | TokenKind::Plus | TokenKind::Minus => {
                self.index += 1;
                let operator = match token.kind {
                    TokenKind::Not => UnaryOperator::Not,
                    TokenKind::Plus => UnaryOperator::Positive,
                    TokenKind::Minus => UnaryOperator::Negative,
                    _ => unreachable!(),
                };
                let operand = self.parse_expression(13);
                Expression {
                    span: self.span(token.span.range().start(), operand.span.range().end()),
                    kind: ExpressionKind::Unary {
                        operator,
                        operand: Box::new(operand),
                    },
                }
            }
            TokenKind::OpenParen => {
                self.index += 1;
                let mut expression = self.parse_expression(0);
                let end = self.require_close_paren();
                expression.span = self.span(token.span.range().start(), end);
                expression
            }
            TokenKind::OpenBrace => {
                self.index += 1;
                let (properties, end) = self.parse_property_block(token.span, 1);
                Expression {
                    kind: ExpressionKind::Object(properties),
                    span: self.span(token.span.range().start(), end),
                }
            }
            _ => {
                self.index += 1;
                self.syntax("expected a VCL expression", token.span);
                Expression {
                    kind: ExpressionKind::Invalid,
                    span: token.span,
                }
            }
        }
    }

    fn binary_operator(&self) -> Option<(BinaryOperator, u8, u8)> {
        Some(match self.peek()?.kind {
            TokenKind::Or => (BinaryOperator::Or, 1, 2),
            TokenKind::And => (BinaryOperator::And, 3, 4),
            TokenKind::Equal => (BinaryOperator::Equal, 5, 6),
            TokenKind::NotEqual => (BinaryOperator::NotEqual, 5, 6),
            TokenKind::Match => (BinaryOperator::Match, 5, 6),
            TokenKind::NotMatch => (BinaryOperator::NotMatch, 5, 6),
            TokenKind::Less => (BinaryOperator::Less, 5, 6),
            TokenKind::LessEqual => (BinaryOperator::LessEqual, 5, 6),
            TokenKind::Greater => (BinaryOperator::Greater, 5, 6),
            TokenKind::GreaterEqual => (BinaryOperator::GreaterEqual, 5, 6),
            TokenKind::Plus => (BinaryOperator::Add, 7, 8),
            TokenKind::Minus => (BinaryOperator::Subtract, 7, 8),
            TokenKind::Star => (BinaryOperator::Multiply, 9, 10),
            TokenKind::Slash => (BinaryOperator::Divide, 9, 10),
            _ => return None,
        })
    }

    fn skip_declaration(&mut self) -> usize {
        let mut depth = 0_usize;
        while let Some(token) = self.peek() {
            let end = token.span.range().end();
            match token.kind {
                TokenKind::OpenBrace => depth += 1,
                TokenKind::CloseBrace => {
                    self.index += 1;
                    if depth <= 1 {
                        return end;
                    }
                    depth -= 1;
                    continue;
                }
                TokenKind::Semicolon if depth == 0 => {
                    self.index += 1;
                    return end;
                }
                _ => {}
            }
            self.index += 1;
        }
        self.stop_offset
    }

    fn skip_block(&mut self, opening: Span) -> usize {
        let mut depth = 1_usize;
        while let Some(token) = self.peek().cloned() {
            self.index += 1;
            match token.kind {
                TokenKind::OpenBrace => depth += 1,
                TokenKind::CloseBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return token.span.range().end();
                    }
                }
                _ => {}
            }
        }
        self.unclosed(opening, "block");
        self.stop_offset
    }

    fn require_value(&mut self, message: &'static str) -> Value {
        if let Some(value) = self.take_value() {
            value
        } else {
            let span = self.current_span();
            self.syntax(message, span);
            Value {
                bytes: Vec::new(),
                span,
            }
        }
    }

    fn require_word(&mut self, message: &'static str) -> Value {
        if matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::Word(_))
        ) {
            return self.take_value().expect("word token was checked");
        }
        let span = self.current_span();
        self.syntax(message, span);
        Value {
            bytes: Vec::new(),
            span,
        }
    }

    fn require_string(&mut self, message: &'static str) -> Value {
        if matches!(
            self.peek().map(|token| &token.kind),
            Some(TokenKind::String(_))
        ) {
            return self.take_value().expect("string token was checked");
        }
        let span = self.current_span();
        self.syntax(message, span);
        Value {
            bytes: Vec::new(),
            span,
        }
    }

    fn take_value(&mut self) -> Option<Value> {
        let token = self.peek()?.clone();
        let (TokenKind::Word(bytes) | TokenKind::String(bytes) | TokenKind::Number(bytes)) =
            token.kind
        else {
            return None;
        };
        self.index += 1;
        Some(Value {
            bytes,
            span: token.span,
        })
    }

    fn require_semicolon(&mut self) -> usize {
        if let Some(span) = self.take_if(TokenKindTag::Semicolon) {
            span.range().end()
        } else {
            let span = self.current_span();
            self.syntax("expected `;`", span);
            self.recover_to_semicolon()
        }
    }

    fn require_open_brace(&mut self, message: &'static str) -> Span {
        if let Some(span) = self.take_if(TokenKindTag::OpenBrace) {
            span
        } else {
            let span = self.current_span();
            self.syntax(message, span);
            span
        }
    }

    fn require_open_paren(&mut self) {
        if self.take_if(TokenKindTag::OpenParen).is_none() {
            self.syntax("expected `(`", self.current_span());
        }
    }

    fn require_close_paren(&mut self) -> usize {
        if let Some(span) = self.take_if(TokenKindTag::CloseParen) {
            span.range().end()
        } else {
            let span = self.current_span();
            self.syntax("expected `)`", span);
            span.range().end()
        }
    }

    fn recover_to_semicolon(&mut self) -> usize {
        while let Some(token) = self.peek() {
            if matches!(token.kind, TokenKind::Semicolon) {
                let end = token.span.range().end();
                self.index += 1;
                return end;
            }
            if matches!(token.kind, TokenKind::CloseBrace) {
                return token.span.range().start();
            }
            self.index += 1;
        }
        self.stop_offset
    }

    fn take_if(&mut self, tag: TokenKindTag) -> Option<Span> {
        let token = self.peek()?;
        if tag.matches(&token.kind) {
            let span = token.span;
            self.index += 1;
            Some(span)
        } else {
            None
        }
    }

    fn at_word(&self, expected: &[u8]) -> bool {
        matches!(self.peek().map(|token| &token.kind), Some(TokenKind::Word(bytes)) if bytes == expected)
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.index)
    }

    fn current_start(&self) -> usize {
        self.peek()
            .map_or(self.stop_offset, |token| token.span.range().start())
    }

    fn current_span(&self) -> Span {
        self.peek()
            .map_or_else(|| self.end_span(), |token| token.span)
    }

    fn take_one_span(&mut self) -> Span {
        let span = self.current_span();
        if self.peek().is_some() {
            self.index += 1;
        }
        span
    }

    fn unclosed(&mut self, opening: Span, label: &'static str) {
        self.diagnostics.push(
            Diagnostic::new(
                E_VCL_SYNTAX,
                Severity::Error,
                DiagnosticStage::Parse,
                format!("unexpected end of source; expected `}}` for {label}"),
            )
            .with_primary_span(self.end_span())
            .with_related_span(opening, "block opened here"),
        );
    }

    fn depth_limit(&mut self, opening: Span) {
        self.diagnostics.push(
            Diagnostic::new(
                E_SOURCE_LIMIT,
                Severity::Error,
                DiagnosticStage::Parse,
                format!(
                    "VCL structural depth exceeds the maximum of {}",
                    self.limits.structural_depth
                ),
            )
            .with_primary_span(opening),
        );
    }

    fn syntax(&mut self, message: impl Into<String>, span: Span) {
        self.diagnostics.push(
            Diagnostic::new(
                E_VCL_SYNTAX,
                Severity::Error,
                DiagnosticStage::Parse,
                message,
            )
            .with_primary_span(span),
        );
    }

    fn end_span(&self) -> Span {
        self.span(self.stop_offset, self.stop_offset)
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.source, ByteRange::new(start, end))
    }
}

#[derive(Clone, Copy)]
enum TokenKindTag {
    OpenBrace,
    OpenParen,
    CloseParen,
    Semicolon,
    Comma,
    Assign,
    Plus,
    Not,
    Slash,
}

impl TokenKindTag {
    fn matches(self, kind: &TokenKind) -> bool {
        matches!(
            (self, kind),
            (Self::OpenBrace, TokenKind::OpenBrace)
                | (Self::OpenParen, TokenKind::OpenParen)
                | (Self::CloseParen, TokenKind::CloseParen)
                | (Self::Semicolon, TokenKind::Semicolon)
                | (Self::Comma, TokenKind::Comma)
                | (Self::Assign, TokenKind::Assign)
                | (Self::Plus, TokenKind::Plus)
                | (Self::Not, TokenKind::Not)
                | (Self::Slash, TokenKind::Slash)
        )
    }
}
