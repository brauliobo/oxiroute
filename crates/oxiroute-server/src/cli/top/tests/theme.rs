use ratatui::style::{Color, Modifier};

use super::harness::{app, draw, running, text};
use crate::cli::top::{
    model::Tone,
    prefs::Prefs,
    theme::{Capabilities, ColorChoice, Depth, Role, Symbols, Theme, ThemeName},
    views::View,
};

fn detect(color: ColorChoice, ascii: bool, variables: &[(&str, &str)]) -> Capabilities {
    let variables = variables
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect::<Vec<_>>();
    Capabilities::detect(color, ascii, &move |name| {
        variables
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.clone())
    })
}

const UTF8: (&str, &str) = ("LANG", "en_US.UTF-8");

#[test]
fn truecolor_is_used_only_when_the_terminal_says_so() {
    let truecolor = detect(
        ColorChoice::Auto,
        false,
        &[UTF8, ("TERM", "xterm"), ("COLORTERM", "truecolor")],
    );
    assert_eq!(truecolor.depth, Depth::Truecolor);
    let basic = detect(
        ColorChoice::Auto,
        false,
        &[UTF8, ("TERM", "xterm-256color")],
    );
    assert_eq!(basic.depth, Depth::Ansi16);
}

#[test]
fn no_color_and_dumb_terminals_turn_colors_off_unless_forced() {
    let off = |variables: &[(&str, &str)], color| detect(color, false, variables).depth;
    assert_eq!(off(&[("NO_COLOR", "1")], ColorChoice::Auto), Depth::None);
    assert_eq!(off(&[("TERM", "dumb")], ColorChoice::Auto), Depth::None);
    assert_eq!(
        off(&[("NO_COLOR", "1")], ColorChoice::Always),
        Depth::Ansi16
    );
    assert_eq!(off(&[], ColorChoice::Never), Depth::None);
    assert_eq!(off(&[("NO_COLOR", "")], ColorChoice::Auto), Depth::Ansi16);
}

#[test]
fn unicode_needs_a_utf8_locale_and_a_capable_terminal() {
    let unicode =
        |variables: &[(&str, &str)], ascii| detect(ColorChoice::Auto, ascii, variables).unicode;
    assert!(unicode(&[UTF8], false));
    assert!(unicode(&[("LC_ALL", "C.utf8")], false));
    assert!(!unicode(&[UTF8], true));
    assert!(!unicode(&[("LANG", "C")], false));
    assert!(!unicode(&[], false));
    assert!(!unicode(&[UTF8, ("TERM", "linux")], false));
    assert!(!unicode(&[UTF8, ("TERM", "dumb")], false));
}

#[test]
fn the_mono_theme_and_no_color_use_modifiers_instead_of_colors() {
    for theme in [
        Theme::new(
            ThemeName::Mono,
            Capabilities {
                depth: Depth::Truecolor,
                unicode: true,
            },
        ),
        Theme::new(
            ThemeName::Dark,
            Capabilities {
                depth: Depth::None,
                unicode: true,
            },
        ),
    ] {
        assert_eq!(theme.depth, Depth::None);
        for role in [
            Role::Good,
            Role::Warn,
            Role::Bad,
            Role::Accent,
            Role::Border,
            Role::Dim,
        ] {
            assert_eq!(theme.style(role).fg, None, "{role:?}");
        }
        assert!(
            theme
                .style(Role::Bad)
                .add_modifier
                .contains(Modifier::BOLD | Modifier::UNDERLINED)
        );
        assert!(
            theme
                .style(Role::Selected)
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert_ne!(theme.tone(Tone::Bad), theme.tone(Tone::Plain));
    }
}

#[test]
fn the_color_themes_differ_and_fall_back_to_sixteen_colors() {
    let capabilities = |depth| Capabilities {
        depth,
        unicode: true,
    };
    let dark = Theme::new(ThemeName::Dark, capabilities(Depth::Truecolor)).style(Role::Good);
    let light = Theme::new(ThemeName::Light, capabilities(Depth::Truecolor)).style(Role::Good);
    let basic = Theme::new(ThemeName::Dark, capabilities(Depth::Ansi16)).style(Role::Good);
    assert!(matches!(dark.fg, Some(Color::Rgb(..))) && matches!(light.fg, Some(Color::Rgb(..))));
    assert_ne!(dark.fg, light.fg);
    assert_eq!(basic.fg, Some(Color::Green));
}

#[test]
fn meters_turn_from_green_to_red_as_they_fill() {
    let theme = Theme::new(
        ThemeName::Dark,
        Capabilities {
            depth: Depth::Ansi16,
            unicode: true,
        },
    );
    assert_eq!(theme.meter(0.2).fg, Some(Color::Green));
    assert_eq!(theme.meter(0.75).fg, Some(Color::Yellow));
    assert_eq!(theme.meter(0.95).fg, Some(Color::Red));
}

#[test]
fn the_ascii_fallback_draws_only_ascii_characters() {
    let capabilities = detect(ColorChoice::Auto, true, &[UTF8, ("TERM", "xterm")]);
    assert_eq!(Symbols::pick(capabilities.unicode), Symbols::ASCII);
    let mut app = app(View::Upstreams, Prefs::default(), capabilities);
    app.sample(Ok(super::harness::first_sample()));
    app.sample(Ok(super::harness::second_sample()));
    let screen = text(&draw(&mut app, 120, 30));
    assert!(
        screen.is_ascii(),
        "{}",
        screen.chars().filter(|c| !c.is_ascii()).collect::<String>()
    );
    assert!(screen.contains("+--") && screen.contains('|'));
    assert!(screen.contains("-- api") || screen.contains("+ api") || screen.contains("- api"));
}

#[test]
fn the_unicode_look_uses_rounded_borders() {
    let mut app = running(View::Listeners);
    let screen = text(&draw(&mut app, 120, 30));
    assert!(screen.contains('╭') && screen.contains('╯'));
}

#[test]
fn without_color_no_cell_carries_a_color() {
    let mut app = running(View::Upstreams);
    let buffer = draw(&mut app, 120, 30);
    assert!(
        buffer
            .content()
            .iter()
            .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset)
    );
}

#[test]
fn with_color_failures_are_painted() {
    let mut app = app(
        View::Upstreams,
        Prefs::default(),
        Capabilities {
            depth: Depth::Ansi16,
            unicode: true,
        },
    );
    app.sample(Ok(super::harness::first_sample()));
    app.sample(Ok(super::harness::second_sample()));
    let buffer = draw(&mut app, 120, 30);
    assert!(buffer.content().iter().any(|cell| cell.fg == Color::Red));
    assert!(buffer.content().iter().any(|cell| cell.fg == Color::Green));
}
