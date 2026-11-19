use clap::ValueEnum;

use super::Depth;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Capabilities {
    pub(crate) depth: Depth,
    pub(crate) unicode: bool,
}

impl Capabilities {
    pub(crate) fn detect(
        color: ColorChoice,
        ascii: bool,
        variable: &dyn Fn(&str) -> Option<String>,
    ) -> Self {
        let set = |name: &str| variable(name).is_some_and(|value| !value.is_empty());
        let term = variable("TERM").unwrap_or_default();
        let depth = match color {
            ColorChoice::Never => Depth::None,
            _ if color == ColorChoice::Auto && (set("NO_COLOR") || term == "dumb") => Depth::None,
            _ if matches!(
                variable("COLORTERM").as_deref(),
                Some("truecolor" | "24bit")
            ) =>
            {
                Depth::Truecolor
            }
            _ => Depth::Ansi16,
        };
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .into_iter()
            .find_map(|name| variable(name).filter(|value| !value.is_empty()))
            .unwrap_or_default()
            .to_ascii_uppercase();
        let unicode = !ascii
            && !matches!(term.as_str(), "linux" | "dumb")
            && (locale.contains("UTF-8") || locale.contains("UTF8"));
        Self { depth, unicode }
    }
}
