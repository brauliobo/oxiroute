mod detect;
mod symbols;

use clap::ValueEnum;
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

pub(crate) use detect::{Capabilities, ColorChoice};
pub(crate) use symbols::Symbols;

use super::model::Tone;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ThemeName {
    #[default]
    Dark,
    Light,
    Mono,
}

impl ThemeName {
    pub(crate) const fn next(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Mono,
            Self::Mono => Self::Dark,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::Mono => "mono",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Depth {
    None,
    Ansi16,
    Truecolor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Role {
    Normal,
    Dim,
    Good,
    Warn,
    Bad,
    Accent,
    Header,
    Selected,
    Border,
    Title,
    Chip,
    MeterLow,
    MeterMid,
    MeterHigh,
}

#[derive(Clone, Copy, Debug)]
struct Palette {
    accent: Color,
    good: Color,
    warn: Color,
    bad: Color,
    dim: Color,
    border: Color,
}

const DARK_TRUECOLOR: Palette = Palette {
    accent: Color::Rgb(0x5f, 0xaf, 0xff),
    good: Color::Rgb(0x87, 0xd7, 0x87),
    warn: Color::Rgb(0xff, 0xd7, 0x5f),
    bad: Color::Rgb(0xff, 0x5f, 0x5f),
    dim: Color::Rgb(0x80, 0x88, 0x94),
    border: Color::Rgb(0x58, 0x66, 0x7a),
};

const LIGHT_TRUECOLOR: Palette = Palette {
    accent: Color::Rgb(0x00, 0x57, 0xb8),
    good: Color::Rgb(0x1a, 0x7f, 0x37),
    warn: Color::Rgb(0x9a, 0x67, 0x00),
    bad: Color::Rgb(0xcf, 0x22, 0x2e),
    dim: Color::Rgb(0x6e, 0x77, 0x81),
    border: Color::Rgb(0x8c, 0x95, 0xa0),
};

const ANSI: Palette = Palette {
    accent: Color::Cyan,
    good: Color::Green,
    warn: Color::Yellow,
    bad: Color::Red,
    dim: Color::DarkGray,
    border: Color::DarkGray,
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Theme {
    pub(crate) name: ThemeName,
    pub(crate) depth: Depth,
    pub(crate) symbols: Symbols,
}

impl Theme {
    pub(crate) fn new(name: ThemeName, capabilities: Capabilities) -> Self {
        let depth = match (name, capabilities.depth) {
            (ThemeName::Mono, _) | (_, Depth::None) => Depth::None,
            (_, depth) => depth,
        };
        Self {
            name,
            depth,
            symbols: Symbols::pick(capabilities.unicode),
        }
    }

    fn palette(self) -> Palette {
        match (self.depth, self.name) {
            (Depth::Truecolor, ThemeName::Light) => LIGHT_TRUECOLOR,
            (Depth::Truecolor, _) => DARK_TRUECOLOR,
            _ => ANSI,
        }
    }

    pub(crate) fn style(self, role: Role) -> Style {
        if self.depth == Depth::None {
            return Self::monochrome(role);
        }
        let palette = self.palette();
        let colored = |color| Style::new().fg(color);
        match role {
            Role::Normal => Style::new(),
            Role::Dim => colored(palette.dim),
            Role::Good | Role::MeterLow => colored(palette.good),
            Role::Warn | Role::MeterMid => colored(palette.warn),
            Role::Bad | Role::MeterHigh => colored(palette.bad),
            Role::Accent => colored(palette.accent).add_modifier(Modifier::BOLD),
            Role::Header => Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            Role::Selected | Role::Chip => Style::new().add_modifier(Modifier::REVERSED),
            Role::Border => colored(palette.border),
            Role::Title => colored(palette.accent),
        }
    }

    fn monochrome(role: Role) -> Style {
        let modifier = match role {
            Role::Normal | Role::Border | Role::MeterLow | Role::Good => Modifier::empty(),
            Role::Dim => Modifier::DIM,
            Role::Warn | Role::MeterMid | Role::Accent | Role::Title => Modifier::BOLD,
            Role::Bad | Role::MeterHigh | Role::Header => Modifier::BOLD | Modifier::UNDERLINED,
            Role::Selected | Role::Chip => Modifier::REVERSED,
        };
        Style::new().add_modifier(modifier)
    }

    pub(crate) fn tone(self, tone: Tone) -> Style {
        self.style(match tone {
            Tone::Plain => Role::Normal,
            Tone::Good => Role::Good,
            Tone::Warn => Role::Warn,
            Tone::Bad => Role::Bad,
            Tone::Dim => Role::Dim,
        })
    }

    pub(crate) fn meter(self, fraction: f64) -> Style {
        self.style(match fraction {
            share if share >= 0.9 => Role::MeterHigh,
            share if share >= 0.7 => Role::MeterMid,
            _ => Role::MeterLow,
        })
    }
}
