//! The one place colors, spacing, radii, sizes and fonts are defined. Views only use these.

use iced::border::Radius;
use iced::font::Weight;
use iced::widget::{button, checkbox, container, pick_list, progress_bar, text_editor, text_input};
use iced::{Background, Border, Color, Font, Shadow, Theme};

/// Spacing scale (8 px grid).
pub const XS: f32 = 4.0;
pub const SM: f32 = 8.0;
pub const MD: f32 = 12.0;
pub const LG: f32 = 16.0;
pub const XL: f32 = 24.0;
pub const XXL: f32 = 32.0;

pub const RADIUS_CONTROL: f32 = 6.0;
pub const RADIUS_PANEL: f32 = 10.0;

pub const TEXT_BODY: f32 = 13.0;
pub const TEXT_META: f32 = 12.0;
pub const TEXT_TITLE: f32 = 15.0;

pub const ROW_HEIGHT: f32 = 44.0;
pub const ROW_HEIGHT_COMPACT: f32 = 32.0;
/// Zoom steps offered in Settings.
pub const UI_SCALES: [f32; 5] = [0.9, 1.0, 1.1, 1.25, 1.5];
/// Card width in Browse's grid.
pub const CARD_WIDTH: f32 = 250.0;
/// Trailing width for rows that show hover actions.
pub const ACTIONS_HOVER: f32 = 132.0;
/// Default table column widths; users can drag them wider or narrower.
pub const COL_NAME: f32 = 280.0;
pub const COL_VERSION: f32 = 120.0;
pub const COL_SOURCE: f32 = 110.0;
pub const COL_STATUS: f32 = 140.0;
pub const COL_AUTHOR: f32 = 140.0;
pub const COL_COUNT: f32 = 100.0;
pub const COL_DATE: f32 = 110.0;
pub const COL_DATETIME: f32 = 150.0;
pub const COL_CHANGE: f32 = 480.0;
pub const COL_MIN: f32 = 60.0;
/// Row action areas at the right edge of tables.
pub const ACTIONS_NARROW: f32 = 72.0;
/// The checkbox column of selectable tables.
pub const COL_CHECK: f32 = 28.0;
pub const ICON_SIZE: f32 = 14.0;
pub const TOAST_WIDTH: f32 = 360.0;
pub const SHORTCUTS_WIDTH: f32 = 420.0;
pub const ACTIONS_WIDE: f32 = 150.0;
/// Grab area between header cells for resizing.
pub const RESIZE_HANDLE: f32 = 8.0;
pub const HEADER_HEIGHT: f32 = 28.0;
pub const SIDEBAR_WIDTH: f32 = 200.0;
pub const STATUS_BAR_HEIGHT: f32 = 28.0;
pub const DETAIL_WIDTH: f32 = 340.0;
/// Scrollbar thickness, and the strip kept clear for it beside scrolled content (and beside the
/// table headers above it, so columns line up).
pub const SCROLLBAR: f32 = 8.0;
pub const SCROLL_GUTTER: f32 = SCROLLBAR + XS;
/// Fixed label column in settings rows.
pub const LABEL_WIDTH: f32 = 100.0;
/// First-run card.
pub const CARD_MAX_WIDTH: f32 = 680.0;
pub const BORDER_WIDTH: f32 = 1.0;

pub const WINDOW_DEFAULT: (f32, f32) = (1100.0, 720.0);
pub const WINDOW_MIN: (f32, f32) = (860.0, 560.0);

pub const INTER_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
pub const INTER_SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf");
pub const FONT: Font = Font::with_name("Inter");
pub const FONT_STRONG: Font = Font {
    weight: Weight::Semibold,
    ..FONT
};
/// Versions and paths only.
pub const MONO: Font = Font::MONOSPACE;
/// Lucide icons (ISC, see assets/fonts/LICENSE-lucide.txt), cut down to the glyphs in [`Icon`].
pub const LUCIDE: &[u8] = include_bytes!("../../../assets/fonts/lucide.ttf");
pub const ICONS: Font = Font::with_name("lucide");

/// Every icon the app draws. To add one, add its codepoint here and re-subset the font with
/// `pyftsubset lucide.ttf --unicodes=...` from the lucide-static package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    ArrowLeft,
    Check,
    ChevronDown,
    ChevronUp,
    Compass,
    Copy,
    Download,
    Ellipsis,
    ExternalLink,
    FolderOpen,
    HeartPulse,
    Keyboard,
    Layers,
    Package,
    Pin,
    PinOff,
    Power,
    PowerOff,
    QrCode,
    Refresh,
    Search,
    Settings,
    Trash,
    TriangleAlert,
    Undo,
    UpdateArrow,
    X,
    BrushCleaning,
    FileArchive,
    History,
    LayoutGrid,
    Link,
    List,
    Sparkles,
    Users,
}

impl Icon {
    pub fn glyph(self) -> char {
        match self {
            Self::ArrowLeft => '\u{e048}',
            Self::Check => '\u{e06c}',
            Self::ChevronDown => '\u{e06d}',
            Self::ChevronUp => '\u{e070}',
            Self::Compass => '\u{e09b}',
            Self::Copy => '\u{e09e}',
            Self::Download => '\u{e0b2}',
            Self::Ellipsis => '\u{e0b6}',
            Self::ExternalLink => '\u{e0b9}',
            Self::FolderOpen => '\u{e247}',
            Self::HeartPulse => '\u{e36e}',
            Self::Keyboard => '\u{e284}',
            Self::Layers => '\u{e529}',
            Self::Package => '\u{e129}',
            Self::Pin => '\u{e259}',
            Self::PinOff => '\u{e2b6}',
            Self::Power => '\u{e140}',
            Self::PowerOff => '\u{e209}',
            Self::QrCode => '\u{e1df}',
            Self::Refresh => '\u{e145}',
            Self::Search => '\u{e151}',
            Self::Settings => '\u{e154}',
            Self::Trash => '\u{e18e}',
            Self::TriangleAlert => '\u{e193}',
            Self::Undo => '\u{e2a1}',
            Self::UpdateArrow => '\u{e07b}',
            Self::BrushCleaning => '\u{e666}',
            Self::FileArchive => '\u{e30d}',
            Self::History => '\u{e1f5}',
            Self::LayoutGrid => '\u{e0ff}',
            Self::Link => '\u{e102}',
            Self::List => '\u{e106}',
            Self::Sparkles => '\u{e412}',
            Self::Users => '\u{e1a4}',
            Self::X => '\u{e1b2}',
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    pub bg: Color,
    pub surface: Color,
    pub surface_hover: Color,
    pub border: Color,
    pub text: Color,
    pub text_muted: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    /// Extra hues for telling categories apart (sources, history kinds); never for status.
    pub violet: Color,
    pub teal: Color,
    pub orange: Color,
    pub pink: Color,
    pub dark: bool,
    /// Table row height, from the density setting.
    pub row: f32,
}

const fn hex(rgb: u32) -> Color {
    Color::from_rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const DARK: Tokens = Tokens {
    bg: hex(0x0F1115),
    surface: hex(0x161A20),
    surface_hover: hex(0x1D222A),
    border: hex(0x262C35),
    text: hex(0xE6E8EB),
    text_muted: hex(0x8B93A1),
    accent: hex(0x7C9CFF),
    success: hex(0x4CC38A),
    warning: hex(0xE5B454),
    danger: hex(0xE5606B),
    violet: hex(0xB392F0),
    teal: hex(0x4FC1C9),
    orange: hex(0xF0915A),
    pink: hex(0xE879B9),
    dark: true,
    row: ROW_HEIGHT,
};

pub const LIGHT: Tokens = Tokens {
    bg: hex(0xF7F8FA),
    surface: hex(0xFFFFFF),
    surface_hover: hex(0xF0F2F5),
    border: hex(0xE2E5EA),
    text: hex(0x14171C),
    text_muted: hex(0x5E6672),
    accent: hex(0x3B60E8),
    success: hex(0x177C4D),
    warning: hex(0x8C6516),
    danger: hex(0xC43844),
    violet: hex(0x7A3FD1),
    teal: hex(0x0B7680),
    orange: hex(0xB04E12),
    pink: hex(0xAD2A76),
    dark: false,
    row: ROW_HEIGHT,
};

impl Tokens {
    /// A source's badge color; `None` is a hand-installed folder.
    pub fn source(&self, source: Option<kodo_core::addon::SourceId>) -> Color {
        use kodo_core::addon::SourceId as S;
        match source {
            Some(S::Github) => self.violet,
            Some(S::Wowi) => self.teal,
            Some(S::Curseforge) => self.orange,
            Some(S::Wago) => self.pink,
            None => self.text_muted,
        }
    }

    /// Pick the palette for a configured theme, given whether the OS prefers dark.
    pub fn for_theme(
        theme: kodo_core::config::Theme,
        system_dark: bool,
        density: kodo_core::config::Density,
    ) -> Self {
        use kodo_core::config::Theme as T;
        let base = match theme {
            T::Dark => DARK,
            T::Light => LIGHT,
            T::System if system_dark => DARK,
            T::System => LIGHT,
        };
        Self {
            row: match density {
                kodo_core::config::Density::Comfortable => ROW_HEIGHT,
                kodo_core::config::Density::Compact => ROW_HEIGHT_COMPACT,
            },
            ..base
        }
    }

    /// iced's built-in theme seeded from the tokens, so unstyled widgets (scrollbars, menus)
    /// still match.
    pub fn iced(&self) -> Theme {
        Theme::custom(
            if self.dark { "kodo dark" } else { "kodo light" },
            iced::theme::Palette {
                background: self.bg,
                text: self.text,
                primary: self.accent,
                success: self.success,
                warning: self.warning,
                danger: self.danger,
            },
        )
    }

    /// Text color on top of the accent.
    fn on_accent(&self) -> Color {
        if self.dark { self.bg } else { self.surface }
    }
}

fn border(color: Color, radius: f32) -> Border {
    Border {
        color,
        width: BORDER_WIDTH,
        radius: Radius::from(radius),
    }
}

fn no_border(radius: f32) -> Border {
    Border {
        color: Color::TRANSPARENT,
        width: 0.0,
        radius: Radius::from(radius),
    }
}

pub fn window(t: Tokens) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(t.text),
        background: Some(Background::Color(t.bg)),
        ..container::Style::default()
    }
}

/// Sidebar, list, status bar: a surface with a 1 px border.
pub fn surface(t: Tokens) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(t.text),
        background: Some(Background::Color(t.surface)),
        border: border(t.border, 0.0),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Side panels and cards.
pub fn panel(t: Tokens) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(t.text),
        background: Some(Background::Color(t.surface)),
        border: border(t.border, RADIUS_PANEL),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// The keyboard cursor's row.
pub fn cursor_row(t: Tokens) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(t.text),
        background: Some(Background::Color(t.surface_hover)),
        border: border(t.accent, RADIUS_CONTROL),
        ..container::Style::default()
    }
}

/// A notification; errors get a danger border.
pub fn toast(t: Tokens, error: bool) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(t.text),
        background: Some(Background::Color(t.surface)),
        border: border(if error { t.danger } else { t.border }, RADIUS_PANEL),
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
            offset: iced::Vector::new(0.0, 4.0),
            blur_radius: 16.0,
        },
        snap: true,
    }
}

/// 1 px divider line.
pub fn divider(t: Tokens) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(t.border)),
        ..container::Style::default()
    }
}

/// Pill background: its color at this opacity over the row. Low enough that every pill color
/// keeps 4.5:1 against it in both palettes.
pub const PILL_TINT: f32 = 0.08;

/// Outlined, faintly tinted status or source pill in the given role color.
pub fn pill(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(color),
        background: Some(Background::Color(Color {
            a: PILL_TINT,
            ..color
        })),
        border: border(color, RADIUS_CONTROL),
        ..container::Style::default()
    }
}

/// Filled count badge (Updates in the sidebar).
pub fn badge(t: Tokens) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(t.on_accent()),
        background: Some(Background::Color(t.accent)),
        border: no_border(RADIUS_CONTROL),
        ..container::Style::default()
    }
}

pub fn primary_button(t: Tokens) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let background = match status {
            button::Status::Disabled => t.border,
            button::Status::Hovered | button::Status::Pressed => Color {
                a: 0.85,
                ..t.accent
            },
            button::Status::Active => t.accent,
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: if status == button::Status::Disabled {
                t.text_muted
            } else {
                t.on_accent()
            },
            border: no_border(RADIUS_CONTROL),
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

/// Outlined secondary button.
pub fn secondary_button(t: Tokens) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered | button::Status::Pressed => t.surface_hover,
            _ => t.surface,
        })),
        text_color: if status == button::Status::Disabled {
            t.text_muted
        } else {
            t.text
        },
        border: border(t.border, RADIUS_CONTROL),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Destructive action (remove).
pub fn danger_button(t: Tokens) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered | button::Status::Pressed => t.surface_hover,
            _ => t.surface,
        })),
        text_color: t.danger,
        border: border(t.danger, RADIUS_CONTROL),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Borderless button: sidebar items (highlighted when `active`), list rows, links.
pub fn ghost_button(t: Tokens, active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let background = match status {
            _ if active => Some(Background::Color(t.surface_hover)),
            button::Status::Hovered | button::Status::Pressed => {
                Some(Background::Color(t.surface_hover))
            }
            _ => None,
        };
        button::Style {
            background,
            text_color: if active { t.accent } else { t.text },
            border: no_border(RADIUS_CONTROL),
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

/// A filter chip: outlined, filled with the accent when chosen.
pub fn chip(t: Tokens, on: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(match (on, hovered) {
                (true, _) => Color {
                    a: 0.16,
                    ..t.accent
                },
                (false, true) => t.surface_hover,
                (false, false) => t.surface,
            })),
            text_color: if on { t.accent } else { t.text },
            border: border(if on { t.accent } else { t.border }, RADIUS_PANEL),
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

/// Text-only link in the accent color.
pub fn link_button(t: Tokens) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| button::Style {
        background: None,
        text_color: match status {
            button::Status::Hovered | button::Status::Pressed => t.text,
            _ => t.accent,
        },
        border: no_border(0.0),
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn input(t: Tokens) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
    move |_, status| text_input::Style {
        background: Background::Color(t.surface),
        border: border(
            match status {
                text_input::Status::Focused { .. } => t.accent,
                _ => t.border,
            },
            RADIUS_CONTROL,
        ),
        icon: t.text_muted,
        placeholder: t.text_muted,
        value: t.text,
        selection: Color {
            a: 0.35,
            ..t.accent
        },
    }
}

/// Multi-line text box, styled like [`input`].
pub fn editor(t: Tokens) -> impl Fn(&Theme, text_editor::Status) -> text_editor::Style {
    move |_, status| text_editor::Style {
        background: Background::Color(t.surface),
        border: border(
            match status {
                text_editor::Status::Focused { .. } => t.accent,
                _ => t.border,
            },
            RADIUS_CONTROL,
        ),
        placeholder: t.text_muted,
        value: t.text,
        selection: Color {
            a: 0.35,
            ..t.accent
        },
    }
}

/// Download progress inside a row.
pub fn progress(t: Tokens) -> impl Fn(&Theme) -> progress_bar::Style {
    move |_| progress_bar::Style {
        background: Background::Color(t.surface_hover),
        bar: Background::Color(t.accent),
        border: border(t.border, RADIUS_CONTROL),
    }
}

pub fn select(t: Tokens) -> impl Fn(&Theme, pick_list::Status) -> pick_list::Style {
    move |_, status| pick_list::Style {
        text_color: t.text,
        placeholder_color: t.text_muted,
        handle_color: t.text_muted,
        background: Background::Color(match status {
            pick_list::Status::Hovered => t.surface_hover,
            _ => t.surface,
        }),
        border: border(
            match status {
                pick_list::Status::Opened { .. } => t.accent,
                _ => t.border,
            },
            RADIUS_CONTROL,
        ),
    }
}

pub fn check(t: Tokens) -> impl Fn(&Theme, checkbox::Status) -> checkbox::Style {
    move |_, status| {
        let checked = match status {
            checkbox::Status::Active { is_checked }
            | checkbox::Status::Hovered { is_checked }
            | checkbox::Status::Disabled { is_checked } => is_checked,
        };
        checkbox::Style {
            background: Background::Color(if checked { t.accent } else { t.surface }),
            icon_color: t.on_accent(),
            border: border(if checked { t.accent } else { t.border }, RADIUS_CONTROL),
            text_color: Some(t.text),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::theme::{DARK, LIGHT, Tokens};

    fn all(t: &Tokens) -> [iced::Color; 14] {
        [
            t.bg,
            t.surface,
            t.surface_hover,
            t.border,
            t.text,
            t.text_muted,
            t.accent,
            t.success,
            t.warning,
            t.danger,
            t.violet,
            t.teal,
            t.orange,
            t.pink,
        ]
    }

    #[test]
    fn palettes_define_distinct_opaque_tokens() {
        for t in [DARK, LIGHT] {
            assert!(all(&t).iter().all(|c| c.a == 1.0));
        }
        assert!(all(&DARK).iter().zip(all(&LIGHT)).all(|(d, l)| *d != l));
        assert_eq!(DARK.bg, iced::Color::from_rgb8(0x0F, 0x11, 0x15));
        assert_eq!(LIGHT.accent, iced::Color::from_rgb8(0x3B, 0x60, 0xE8));
    }

    #[test]
    fn system_theme_follows_os() {
        use kodo_core::config::{Density, Theme};
        let d = Density::Comfortable;
        assert_eq!(Tokens::for_theme(Theme::System, true, d), DARK);
        assert_eq!(Tokens::for_theme(Theme::System, false, d), LIGHT);
        assert_eq!(Tokens::for_theme(Theme::Light, true, d), LIGHT);
        let compact = Tokens::for_theme(Theme::Dark, false, Density::Compact);
        assert!(compact.row < DARK.row);
    }

    #[test]
    fn fonts_are_bundled() {
        assert!(crate::theme::INTER_REGULAR.len() > 100_000);
        assert!(crate::theme::INTER_SEMIBOLD.len() > 100_000);
    }
}
