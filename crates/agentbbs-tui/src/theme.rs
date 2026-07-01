//! Colour palettes and ANSI furniture.
//!
//! The default look is deliberately 1990s dial-up BBS: high-cyan and magenta
//! on black, double-line box drawing, a blinking-style lightbar menu, and a
//! chunky banner. Every style function is parameterized by [`ThemeName`] so
//! a session can switch palettes (Appearance screen, ADR-0051) without
//! touching any rendering logic — it is cosmetic only, every screen is
//! driven by the same [`agentbbs_core`] domain underneath.
//!
//! The six non-default themes mirror the web UI's `data-theme` palettes
//! (`genesis/index.html`) hex-for-hex, so switching themes looks the same
//! whether you're in a browser or a terminal.

use ratatui::style::{Color, Modifier, Style};

/// Bright yellow for hotkeys, independent of theme (kept legible on every
/// background below).
pub const YELLOW: Color = Color::Yellow;
/// Red for warnings, independent of theme.
pub const RED: Color = Color::Red;

/// A selectable colour palette. `Retro` is the original, TUI-native amber/
/// cyan CRT look and the default; the rest mirror the web UI's six themes
/// (`genesis/index.html`'s `data-theme` blocks) so the same name means the
/// same colours on both surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ThemeName {
    /// The original cyan/magenta dial-up BBS look (default).
    #[default]
    Retro,
    /// Web `data-theme="dark"`.
    Dark,
    /// Web `data-theme="light"`.
    Light,
    /// Web `data-theme="aubergine"` (Slack-classic).
    Aubergine,
    /// Web `data-theme="nord"`.
    Nord,
    /// Web `data-theme="solarized"`.
    Solarized,
    /// Web `data-theme="terminal"`.
    Terminal,
}

impl ThemeName {
    /// All themes, in picker display order.
    pub const ALL: &'static [ThemeName] = &[
        ThemeName::Retro,
        ThemeName::Dark,
        ThemeName::Light,
        ThemeName::Aubergine,
        ThemeName::Nord,
        ThemeName::Solarized,
        ThemeName::Terminal,
    ];

    /// Display label for the Appearance screen.
    pub fn label(&self) -> &'static str {
        match self {
            ThemeName::Retro => "Retro (amber CRT)",
            ThemeName::Dark => "Dark",
            ThemeName::Light => "Light",
            ThemeName::Aubergine => "Aubergine",
            ThemeName::Nord => "Nord",
            ThemeName::Solarized => "Solarized",
            ThemeName::Terminal => "Terminal",
        }
    }

    /// This theme's accent colour — used for chrome, borders, and as the
    /// swatch shown in the Appearance picker.
    fn accent(&self) -> Color {
        match self {
            ThemeName::Retro => Color::Cyan,
            ThemeName::Dark => Color::Rgb(0x7c, 0x5c, 0xff),
            ThemeName::Light => Color::Rgb(0x6a, 0x3c, 0xff),
            ThemeName::Aubergine => Color::Rgb(0x61, 0x1f, 0x69),
            ThemeName::Nord => Color::Rgb(0x88, 0xc0, 0xd0),
            ThemeName::Solarized => Color::Rgb(0xb5, 0x89, 0x00),
            ThemeName::Terminal => Color::Rgb(0xff, 0xb0, 0x00),
        }
    }

    /// This theme's "success/online" green.
    fn green(&self) -> Color {
        match self {
            ThemeName::Retro => Color::Green,
            ThemeName::Dark => Color::Rgb(0x34, 0xc7, 0x59),
            ThemeName::Light => Color::Rgb(0x1a, 0x9e, 0x3e),
            ThemeName::Aubergine => Color::Rgb(0x2b, 0xac, 0x76),
            ThemeName::Nord => Color::Rgb(0xa3, 0xbe, 0x8c),
            ThemeName::Solarized => Color::Rgb(0x85, 0x99, 0x00),
            ThemeName::Terminal => Color::Rgb(0x4e, 0xe0, 0x5a),
        }
    }

    /// Foreground-on-accent contrast for the title bar (light themes need
    /// black-on-accent, dark themes need white).
    fn on_accent_is_dark(&self) -> bool {
        matches!(
            self,
            ThemeName::Retro | ThemeName::Light | ThemeName::Solarized
        )
    }

    /// Selected-row highlight background — deliberately **not** [`accent`]
    /// (mirrors the web's `--side-active-bg`, a distinct role from
    /// `--accent`). Every render site that draws a selected row also draws
    /// accent-coloured spans (hotkey/label) *inside* that row via
    /// [`chrome`]/[`hotkey`]; if this matched `accent()` those spans would
    /// render invisibly (accent-on-accent). Verified per theme to differ
    /// from [`accent`].
    fn highlight_bg(&self) -> Color {
        match self {
            ThemeName::Retro => Color::Magenta,
            ThemeName::Dark => Color::Rgb(0x2f, 0x7b, 0xff),
            ThemeName::Light => Color::Rgb(0x0a, 0x6c, 0xff),
            ThemeName::Aubergine => Color::Rgb(0x11, 0x64, 0xa3),
            ThemeName::Nord => Color::Rgb(0x5e, 0x81, 0xac),
            ThemeName::Solarized => Color::Rgb(0x26, 0x8b, 0xd2),
            ThemeName::Terminal => Color::Rgb(0x14, 0x3a, 0x17),
        }
    }

    /// Foreground for [`highlight_bg`] — used only where a span doesn't
    /// supply its own colour (e.g. the `▶ ` row prefix); the accent-coloured
    /// label/hotkey spans layered on top always win by ratatui's normal
    /// span-over-line style patching.
    fn highlight_fg(&self) -> Color {
        match self {
            ThemeName::Retro => Color::Black,
            ThemeName::Terminal => Color::Rgb(0xff, 0xb0, 0x00),
            _ => Color::White,
        }
    }

    /// Muted secondary/chrome-dim colour.
    fn dim(&self) -> Color {
        match self {
            ThemeName::Retro => Color::DarkGray,
            ThemeName::Light | ThemeName::Aubergine => Color::Gray,
            _ => Color::DarkGray,
        }
    }
}

/// Style for a screen title bar.
pub fn title(t: ThemeName) -> Style {
    let fg = if t.on_accent_is_dark() {
        Color::Black
    } else {
        Color::White
    };
    Style::default()
        .fg(fg)
        .bg(t.accent())
        .add_modifier(Modifier::BOLD)
}

/// Style for the selected lightbar row. Uses [`ThemeName::highlight_bg`],
/// not `accent()` — see that method's doc for why the two must differ.
pub fn lightbar(t: ThemeName) -> Style {
    Style::default()
        .fg(t.highlight_fg())
        .bg(t.highlight_bg())
        .add_modifier(Modifier::BOLD)
}

/// Style for a hotkey letter.
pub fn hotkey(_t: ThemeName) -> Style {
    Style::default().fg(YELLOW).add_modifier(Modifier::BOLD)
}

/// Style for chrome/borders.
pub fn chrome(t: ThemeName) -> Style {
    Style::default().fg(t.accent())
}

/// Style for dim secondary text.
pub fn dim(t: ThemeName) -> Style {
    Style::default().fg(t.dim())
}

/// This theme's "online"/success colour, for direct `.fg()` use.
pub fn green(t: ThemeName) -> Color {
    t.green()
}

/// This theme's accent colour, for direct `.fg()` use (replaces the old
/// fixed `MAGENTA` constant).
pub fn accent(t: ThemeName) -> Color {
    t.accent()
}

/// The AgentBBS banner, rendered at the top of the splash and main menu.
pub const BANNER: &[&str] = &[
    "  ▄▀█ █▀▀ █▀▀ █▄░█ ▀█▀ █▄▄ █▄▄ █▀   ",
    "  █▀█ █▄█ ██▄ █░▀█ ░█░ █▄█ █▄█ ▄█   ",
    "   t h e   b b s   f o r   a g e n t s   &   h u m a n s ",
];
