use ratatui::symbols::{bar, border};

const ASCII_BORDER: border::Set<'static> = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

const ASCII_BARS: bar::Set<'static> = bar::Set {
    full: "#",
    seven_eighths: "%",
    three_quarters: "*",
    five_eighths: "+",
    half: "=",
    three_eighths: "-",
    one_quarter: ":",
    one_eighth: ".",
    empty: " ",
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Symbols {
    pub(crate) border: border::Set<'static>,
    pub(crate) bars: &'static bar::Set<'static>,
    pub(crate) fill: &'static str,
    pub(crate) vacant: &'static str,
    pub(crate) ascending: &'static str,
    pub(crate) descending: &'static str,
    pub(crate) marker: &'static str,
    pub(crate) open: &'static str,
    pub(crate) closed: &'static str,
    pub(crate) leaf: &'static str,
    pub(crate) ellipsis: &'static str,
    pub(crate) live: &'static str,
    pub(crate) rule: &'static str,
    pub(crate) separator: &'static str,
}

static UNICODE_BARS: bar::Set<'static> = bar::NINE_LEVELS;
static PLAIN_BARS: bar::Set<'static> = ASCII_BARS;

impl Symbols {
    pub(crate) const UNICODE: Self = Self {
        border: border::ROUNDED,
        bars: &UNICODE_BARS,
        fill: "█",
        vacant: "░",
        ascending: "▲",
        descending: "▼",
        marker: "▶",
        open: "▾",
        closed: "▸",
        leaf: " ",
        ellipsis: "…",
        live: "●",
        rule: "─",
        separator: "│",
    };

    pub(crate) const ASCII: Self = Self {
        border: ASCII_BORDER,
        bars: &PLAIN_BARS,
        fill: "#",
        vacant: "-",
        ascending: "^",
        descending: "v",
        marker: ">",
        open: "-",
        closed: "+",
        leaf: " ",
        ellipsis: "~",
        live: "*",
        rule: "-",
        separator: "|",
    };

    pub(crate) const fn pick(unicode: bool) -> Self {
        if unicode { Self::UNICODE } else { Self::ASCII }
    }
}
