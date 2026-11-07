#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DirectiveContext {
    NginxMain,
    RtmpMain,
    RtmpServer,
    RtmpApplication,
    RtmpRecorder,
    Http,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelayKind {
    Push,
    Pull,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueKind {
    AccessLog,
    AccessRule,
    Bitmask(&'static [&'static str]),
    Block,
    Command,
    Duration,
    DurationOrOff,
    Enum(&'static [&'static str]),
    Flag,
    HlsVariant,
    Integer,
    Listen,
    LogFormat,
    NamedBlock,
    Path,
    RelayTarget(RelayKind),
    Signal,
    Size,
    Strings,
    Url,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeSupport {
    ParsedNotEnforced,
    SourceNoOp,
    SourceBug,
    Deprecated,
    PlatformLimited,
}

/// Runtime status for a directive key or one of its explicitly described forms.
///
/// `runtime_support` on [`DirectiveSpec`] remains the coarse, key-level status
/// used by existing consumers. The form-level status is authoritative whenever
/// a directive has entries in [`DirectiveSpec::runtime_forms`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectiveStatus {
    Enforced,
    Partial,
    DisableOnly,
    ParsedOnly,
    SourceNoOp,
    SourceBug,
    Deprecated,
    PlatformLimited,
}

impl DirectiveStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enforced => "enforced",
            Self::Partial => "partial",
            Self::DisableOnly => "disable_only",
            Self::ParsedOnly => "parsed_only",
            Self::SourceNoOp => "source_no_op",
            Self::SourceBug => "source_bug",
            Self::Deprecated => "deprecated",
            Self::PlatformLimited => "platform_limited",
        }
    }
}

/// Runtime status for one named normalized form of a directive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectiveForm {
    pub form: &'static str,
    pub contexts: &'static [DirectiveContext],
    pub status: DirectiveStatus,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DirectiveStatusCounts {
    pub enforced: usize,
    pub partial: usize,
    pub disable_only: usize,
    pub parsed_only: usize,
    pub source_no_op: usize,
    pub source_bug: usize,
    pub deprecated: usize,
    pub platform_limited: usize,
}

impl DirectiveStatusCounts {
    #[must_use]
    pub const fn total(self) -> usize {
        self.enforced
            + self.partial
            + self.disable_only
            + self.parsed_only
            + self.source_no_op
            + self.source_bug
            + self.deprecated
            + self.platform_limited
    }
}

/// Generated status counts and registry entries for the RTMP compatibility report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectiveCompatibilityReport {
    pub entries: &'static [DirectiveSpec],
    pub directive_status: DirectiveStatusCounts,
    pub form_status: DirectiveStatusCounts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectiveSpec {
    pub name: &'static str,
    pub contexts: &'static [DirectiveContext],
    pub min_args: u8,
    pub max_args: Option<u8>,
    pub value_kind: ValueKind,
    pub default: Option<&'static str>,
    pub repeatable: bool,
    pub runtime_support: RuntimeSupport,
    pub runtime_forms: &'static [DirectiveForm],
}

impl DirectiveSpec {
    #[must_use]
    pub fn compatibility_status(&self) -> DirectiveStatus {
        let Some(first) = self.runtime_forms.first() else {
            return match self.runtime_support {
                RuntimeSupport::ParsedNotEnforced => DirectiveStatus::ParsedOnly,
                RuntimeSupport::SourceNoOp => DirectiveStatus::SourceNoOp,
                RuntimeSupport::SourceBug => DirectiveStatus::SourceBug,
                RuntimeSupport::Deprecated => DirectiveStatus::Deprecated,
                RuntimeSupport::PlatformLimited => DirectiveStatus::PlatformLimited,
            };
        };

        if self
            .runtime_forms
            .iter()
            .all(|form| form.status == first.status)
        {
            first.status
        } else {
            DirectiveStatus::Partial
        }
    }

    #[must_use]
    pub fn runtime_form(&self, form: &str) -> Option<&DirectiveForm> {
        self.runtime_forms
            .iter()
            .find(|runtime_form| runtime_form.form == form)
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum DirectiveError {
    #[error("unknown nginx-rtmp directive `{0}`")]
    UnknownDirective(String),
    #[error("directive `{name}` is not valid in {context:?}")]
    InvalidContext {
        name: &'static str,
        context: DirectiveContext,
    },
    #[error("directive `{name}` expects {expected} arguments, got {actual}")]
    InvalidArity {
        name: &'static str,
        expected: String,
        actual: usize,
    },
    #[error("invalid value for directive `{name}`: {detail}")]
    InvalidValue { name: &'static str, detail: String },
}
