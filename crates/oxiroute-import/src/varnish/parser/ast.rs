#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParserLimits {
    pub source_bytes: usize,
    pub tokens: usize,
    pub statements: usize,
    pub structural_depth: usize,
}

impl Default for ParserLimits {
    fn default() -> Self {
        Self {
            source_bytes: MAX_SOURCE_BYTES,
            tokens: MAX_TOKENS_PER_SOURCE,
            statements: MAX_DIRECTIVES_PER_SOURCE,
            structural_depth: MAX_STRUCTURAL_DEPTH,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Document {
    pub declarations: Vec<Declaration>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Declaration {
    Version { value: Value, span: Span },
    Include(IncludeDeclaration),
    Import(ImportDeclaration),
    Acl(AclDeclaration),
    Probe(ProbeDeclaration),
    Backend(BackendDeclaration),
    Director(DirectorDeclaration),
    Subroutine(SubroutineDeclaration),
    Unsupported { keyword: Option<Value>, span: Span },
}

impl Declaration {
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::Version { span, .. } | Self::Unsupported { span, .. } => *span,
            Self::Include(declaration) => declaration.span,
            Self::Import(declaration) => declaration.span,
            Self::Acl(declaration) => declaration.span,
            Self::Probe(declaration) => declaration.span,
            Self::Backend(declaration) => declaration.span,
            Self::Director(declaration) => declaration.span,
            Self::Subroutine(declaration) => declaration.span,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncludeDeclaration {
    pub glob: bool,
    pub path: Value,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportDeclaration {
    pub module: Value,
    pub alias: Option<Value>,
    pub from: Option<Value>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AclDeclaration {
    pub name: Value,
    pub entries: Vec<AclEntry>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AclEntry {
    pub negated: bool,
    pub optional: bool,
    pub value: Value,
    pub mask: Option<Value>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbeDeclaration {
    pub name: Value,
    pub properties: Vec<Assignment>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendDeclaration {
    pub name: Value,
    pub kind: BackendDeclarationKind,
    pub properties: Vec<Assignment>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendDeclarationKind {
    Endpoint,
    None,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectorDeclaration {
    pub name: Value,
    pub policy: Value,
    pub entries: Vec<DirectorEntry>,
    pub properties: Vec<Assignment>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectorEntry {
    pub properties: Vec<Assignment>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubroutineDeclaration {
    pub name: Value,
    pub statements: Vec<Statement>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Statement {
    pub kind: StatementKind,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StatementKind {
    If(IfStatement),
    Set(Assignment),
    Unset(Value),
    Return(Expression),
    Call(Value),
    New(NewObjectStatement),
    Expression(Expression),
    InlineC,
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewObjectStatement {
    pub name: Value,
    pub constructor: Expression,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IfStatement {
    pub branches: Vec<ConditionalBranch>,
    pub otherwise: Vec<Statement>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConditionalBranch {
    pub condition: Expression,
    pub statements: Vec<Statement>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Assignment {
    pub target: Value,
    pub operator: AssignmentOperator,
    pub value: Expression,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssignmentOperator {
    Set,
    Add,
    Subtract,
    Multiply,
    Divide,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Value {
    pub bytes: Vec<u8>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Expression {
    pub kind: ExpressionKind,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExpressionKind {
    Name(Value),
    Literal(Literal),
    Unary {
        operator: UnaryOperator,
        operand: Box<Expression>,
    },
    Binary {
        left: Box<Expression>,
        operator: BinaryOperator,
        right: Box<Expression>,
    },
    Call {
        function: Box<Expression>,
        arguments: Vec<Expression>,
    },
    Object(Vec<Assignment>),
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Literal {
    String(Value),
    Number(Value),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnaryOperator {
    Not,
    Positive,
    Negative,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOperator {
    Equal,
    NotEqual,
    Match,
    NotMatch,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    And,
    Or,
    Add,
    Subtract,
    Multiply,
    Divide,
    Concatenate,
}
