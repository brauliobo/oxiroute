macro_rules! directive {
    ($name:literal, $contexts:expr, $min:literal, $max:expr, $kind:expr, $default:expr, $repeatable:literal) => {
        DirectiveSpec {
            name: $name,
            contexts: $contexts,
            min_args: $min,
            max_args: $max,
            value_kind: $kind,
            default: $default,
            repeatable: $repeatable,
            runtime_support: ParsedNotEnforced,
            runtime_forms: &[],
        }
    };
    ($name:literal, $contexts:expr, $min:literal, $max:expr, $kind:expr, $default:expr, $repeatable:literal, $support:expr) => {
        DirectiveSpec {
            name: $name,
            contexts: $contexts,
            min_args: $min,
            max_args: $max,
            value_kind: $kind,
            default: $default,
            repeatable: $repeatable,
            runtime_support: $support,
            runtime_forms: &[],
        }
    };
    ($name:literal, $contexts:expr, $min:literal, $max:expr, $kind:expr, $default:expr, $repeatable:literal; $forms:expr) => {
        DirectiveSpec {
            name: $name,
            contexts: $contexts,
            min_args: $min,
            max_args: $max,
            value_kind: $kind,
            default: $default,
            repeatable: $repeatable,
            runtime_support: ParsedNotEnforced,
            runtime_forms: $forms,
        }
    };
    ($name:literal, $contexts:expr, $min:literal, $max:expr, $kind:expr, $default:expr, $repeatable:literal, $support:expr; $forms:expr) => {
        DirectiveSpec {
            name: $name,
            contexts: $contexts,
            min_args: $min,
            max_args: $max,
            value_kind: $kind,
            default: $default,
            repeatable: $repeatable,
            runtime_support: $support,
            runtime_forms: $forms,
        }
    };
}

pub(super) use directive;
