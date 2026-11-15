//! Pure view functions over [`App`]. Every color, spacing and size comes from `theme`.

use std::collections::HashMap;
use std::fmt;

use iced::widget::text::Wrapping;
use iced::widget::{
    Row, Space, button, checkbox, column, container, image, markdown, mouse_area, pick_list,
    progress_bar, qr_code, row, scrollable, stack, text, text_editor, text_input, tooltip,
};
use iced::{Alignment, Color, Element, Length, Padding, border};
use kodo_core::addon::{AddonRef, Channel, RemoteAddon, Sort, SourceId};
use kodo_core::config::{Density, Secret, Theme};
use kodo_core::engine::{CharacterAddons, Engine, UpdateCheck};
use kodo_core::health::Issue;
use kodo_core::history::{Event, Kind};
use kodo_core::local::LocalAddon;
use kodo_core::lockfile::{LockEntry, rfc3339};
use kodo_core::profile::Profile;

use crate::app::{
    AddonPage, App, CharacterFilter, JobKind, LIST_ID, Message, Notes, RowKey, SEARCH_ID, Screen,
};
use crate::table::{self, Col, Key, Spec, TableId, spec};
use crate::theme::{self, Icon, Tokens};

type El<'a> = Element<'a, Message>;

const CHANGELOG_SNIPPET: usize = 240;
/// QR module size; a full profile code is about 100 modules across.
const QR_CELL: f32 = 4.0;

pub fn view(app: &App) -> El<'_> {
    let t = app.tokens();
    let body: El<'_> = match app.screen {
        Screen::FirstRun => first_run(app, t),
        _ => {
            let mut main = row![sidebar(app, t), main_area(app, t)].height(Length::Fill);
            if let Some(panel) = detail_panel(app, t) {
                main = main.push(panel);
            }
            let mut layers = stack![main].width(Length::Fill).height(Length::Fill);
            if app.show_keys {
                layers = layers.push(shortcuts(t));
            }
            if !app.toasts.is_empty() {
                layers = layers.push(toasts(app, t));
            }
            layers.into()
        }
    };
    let mut col = column![body];
    if app.error_open {
        col = col.push(error_panel(app, t));
    }
    col = col.push(status_bar(app, t));
    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::window(t))
        .into()
}

// ---- text helpers -------------------------------------------------------------------------

fn body<'a>(s: impl text::IntoFragment<'a>, t: Tokens) -> text::Text<'a> {
    text(s)
        .size(theme::TEXT_BODY)
        .font(theme::FONT)
        .color(t.text)
}

fn meta<'a>(s: impl text::IntoFragment<'a>, t: Tokens) -> text::Text<'a> {
    text(s)
        .size(theme::TEXT_META)
        .font(theme::FONT)
        .color(t.text_muted)
}

fn strong<'a>(s: impl text::IntoFragment<'a>, t: Tokens) -> text::Text<'a> {
    text(s)
        .size(theme::TEXT_BODY)
        .font(theme::FONT_STRONG)
        .color(t.text)
}

fn title<'a>(s: impl text::IntoFragment<'a>, t: Tokens) -> text::Text<'a> {
    text(s)
        .size(theme::TEXT_TITLE)
        .font(theme::FONT_STRONG)
        .color(t.text)
}

fn mono<'a>(s: impl text::IntoFragment<'a>, t: Tokens) -> text::Text<'a> {
    text(s)
        .size(theme::TEXT_META)
        .font(theme::MONO)
        .color(t.text_muted)
}

fn single_line(txt: text::Text<'_>) -> text::Text<'_> {
    txt.wrapping(Wrapping::None)
}

fn pill<'a>(label: impl text::IntoFragment<'a>, color: Color) -> El<'a> {
    container(text(label).size(theme::TEXT_META).font(theme::FONT))
        .padding(Padding::from([0.0, theme::SM]))
        .style(theme::pill(color))
        .into()
}

/// A source's badge in its own color; `None` for a hand-installed folder.
fn source_pill<'a>(source: Option<SourceId>, t: Tokens) -> El<'a> {
    pill(source.map_or("Local", SourceId::label), t.source(source))
}

fn icon<'a>(i: Icon) -> text::Text<'a> {
    text(i.glyph())
        .size(theme::ICON_SIZE)
        .font(theme::ICONS)
        .line_height(1.0)
}

/// Button content: an optional icon before the label.
fn labeled<'a>(
    i: Option<Icon>,
    label: impl text::IntoFragment<'a>,
    font: iced::Font,
) -> Row<'a, Message> {
    let mut r = row![].spacing(theme::XS).align_y(Alignment::Center);
    if let Some(i) = i {
        r = r.push(icon(i));
    }
    r.push(text(label).size(theme::TEXT_BODY).font(font))
}

fn primary<'a>(label: &'a str, msg: Option<Message>, t: Tokens) -> El<'a> {
    primary_i(None, label, msg, t)
}

fn primary_i<'a>(i: Option<Icon>, label: &'a str, msg: Option<Message>, t: Tokens) -> El<'a> {
    button(labeled(i, label, theme::FONT_STRONG))
        .padding(Padding::from([theme::XS, theme::MD]))
        .style(theme::primary_button(t))
        .on_press_maybe(msg)
        .into()
}

fn action<'a>(label: impl text::IntoFragment<'a>, msg: Option<Message>, t: Tokens) -> El<'a> {
    action_i(None, label, msg, t)
}

fn action_i<'a>(
    i: Option<Icon>,
    label: impl text::IntoFragment<'a>,
    msg: Option<Message>,
    t: Tokens,
) -> El<'a> {
    button(labeled(i, label, theme::FONT))
        .padding(Padding::from([theme::XS, theme::MD]))
        .style(theme::secondary_button(t))
        .on_press_maybe(msg)
        .into()
}

fn danger<'a>(label: &'a str, msg: Message, t: Tokens) -> El<'a> {
    button(labeled(Some(Icon::Trash), label, theme::FONT))
        .padding(Padding::from([theme::XS, theme::MD]))
        .style(theme::danger_button(t))
        .on_press(msg)
        .into()
}

/// A borderless icon-only button with a tooltip.
fn icon_button<'a>(i: Icon, tip: &'a str, msg: Message, t: Tokens) -> El<'a> {
    tooltip(
        button(icon(i).color(t.text_muted))
            .padding(theme::XS)
            .style(theme::ghost_button(t, false))
            .on_press(msg),
        container(meta(tip, t))
            .padding(Padding::from([theme::XS, theme::SM]))
            .style(theme::panel(t)),
        tooltip::Position::Top,
    )
    .into()
}

/// Row `i` of the main list: findable by the cursor's scroll, and outlined when it's on it.
fn at_cursor<'a>(app: &App, i: usize, el: impl Into<El<'a>>, t: Tokens) -> El<'a> {
    let row = container(el).id(crate::app::row_id(i));
    if app.cursor == Some(i) {
        row.style(theme::cursor_row(t)).into()
    } else {
        row.into()
    }
}

fn link<'a>(label: impl text::IntoFragment<'a>, msg: Message, t: Tokens) -> El<'a> {
    button(text(label).size(theme::TEXT_META).font(theme::FONT))
        .padding(0)
        .style(theme::link_button(t))
        .on_press(msg)
        .into()
}

fn divider<'a>(t: Tokens) -> El<'a> {
    container(Space::new())
        .width(Length::Fill)
        .height(theme::BORDER_WIDTH)
        .style(theme::divider(t))
        .into()
}

fn section<'a>(heading: impl text::IntoFragment<'a>, content: El<'a>, t: Tokens) -> El<'a> {
    container(column![title(heading, t), content].spacing(theme::MD))
        .padding(theme::LG)
        .width(Length::Fill)
        .style(theme::panel(t))
        .into()
}

// ---- choice wrappers for pick lists -------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChannelChoice(Channel);

impl fmt::Display for ChannelChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ThemeChoice(Theme);

impl fmt::Display for ThemeChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            Theme::Dark => "Dark",
            Theme::Light => "Light",
            Theme::System => "System",
        })
    }
}

// ---- chrome -------------------------------------------------------------------------------

fn sidebar(app: &App, t: Tokens) -> El<'_> {
    let names: Vec<String> = app.clients.iter().map(|c| c.name.clone()).collect();
    let picker: El<'_> = match app.client() {
        Some(c) if names.len() > 1 => pick_list(names, Some(c.name.clone()), Message::SelectClient)
            .text_size(theme::TEXT_BODY)
            .font(theme::FONT_STRONG)
            .padding(theme::SM)
            .width(Length::Fill)
            .style(theme::select(t))
            .into(),
        Some(c) => container(strong(c.name.as_str(), t))
            .padding(theme::SM)
            .into(),
        None => container(meta("No client", t)).padding(theme::SM).into(),
    };
    let active: El<'_> = match &app.loaded.profiles.active {
        Some(p) => container(single_line(meta(format!("Profile: {p}"), t)))
            .padding(Padding::from([0.0, theme::SM]))
            .into(),
        None => Space::new().into(),
    };
    let count = app.update_count();
    let kodo_update: El<'_> = match &app.kodo_update {
        Some(r) => container(link(
            format!("kodo {} is out", r.version),
            Message::OpenUrl(r.url.clone()),
            t,
        ))
        .padding(Padding::from([0.0, theme::MD]))
        .into(),
        None => Space::new().into(),
    };
    let nav = |i: Icon, label: &'static str, screen: Screen, badge: Option<usize>| -> El<'_> {
        let here = app.screen == screen;
        let mut content = row![
            icon(i).color(if here { t.accent } else { t.text_muted }),
            text(label).size(theme::TEXT_BODY).font(theme::FONT)
        ]
        .align_y(Alignment::Center)
        .spacing(theme::SM);
        if let Some(n) = badge.filter(|n| *n > 0) {
            content = content.push(Space::new().width(Length::Fill)).push(
                container(
                    text(n.to_string())
                        .size(theme::TEXT_META)
                        .font(theme::FONT_STRONG),
                )
                .padding(Padding::from([0.0, theme::SM]))
                .style(theme::badge(t)),
            );
        }
        button(content)
            .width(Length::Fill)
            .padding(Padding::from([theme::SM, theme::MD]))
            .style(theme::ghost_button(t, here))
            .on_press(Message::Nav(screen))
            .into()
    };
    container(
        column![
            picker,
            active,
            divider(t),
            nav(Icon::Package, "Installed", Screen::Installed, None),
            nav(Icon::Compass, "Browse", Screen::Browse, None),
            nav(Icon::UpdateArrow, "Updates", Screen::Updates, Some(count)),
            nav(Icon::Layers, "Profiles", Screen::Profiles, None),
            nav(Icon::Users, "Characters", Screen::Characters, None),
            nav(
                Icon::HeartPulse,
                "Health",
                Screen::Health,
                Some(app.loaded.issues.len())
            ),
            nav(Icon::History, "History", Screen::History, None),
            Space::new().height(Length::Fill),
            kodo_update,
            nav(Icon::Settings, "Settings", Screen::Settings, None),
        ]
        .spacing(theme::XS)
        .padding(theme::SM),
    )
    .width(theme::SIDEBAR_WIDTH)
    .height(Length::Fill)
    .style(theme::surface(t))
    .into()
}

fn search_field<'a>(
    placeholder: &'a str,
    value: &'a str,
    on_input: fn(String) -> Message,
    t: Tokens,
) -> El<'a> {
    text_input(placeholder, value)
        .id(SEARCH_ID)
        .on_input(on_input)
        .size(theme::TEXT_BODY)
        .font(theme::FONT)
        .padding(theme::SM)
        .width(Length::Fill)
        .style(theme::input(t))
        .into()
}

/// A vertical scroll whose bar sits in its own gutter instead of over the content.
fn scroll<'a>(content: impl Into<El<'a>>) -> iced::widget::Scrollable<'a, Message> {
    scrollable(container(content).padding(iced::padding::right(theme::SCROLL_GUTTER))).direction(
        scrollable::Direction::Vertical(
            scrollable::Scrollbar::new()
                .width(theme::SCROLLBAR)
                .scroller_width(theme::SCROLLBAR),
        ),
    )
}

fn main_area(app: &App, t: Tokens) -> El<'_> {
    let (toolbar, list) = match app.screen {
        Screen::Installed => installed(app, t),
        Screen::Browse => browse(app, t),
        Screen::Updates => updates(app, t),
        Screen::Profiles => profiles(app, t),
        Screen::Characters => characters(app, t),
        Screen::Health => health(app, t),
        Screen::History => history(app, t),
        Screen::Settings | Screen::FirstRun => {
            return container(scroll(settings(app, t)).height(Length::Fill))
                .padding(theme::LG)
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        }
    };
    column![
        container(toolbar)
            .padding(Padding::from(theme::MD).right(theme::MD + theme::SCROLL_GUTTER))
            .width(Length::Fill),
        container(scroll(list).id(LIST_ID).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Padding::from([0.0, theme::MD])),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn status_bar(app: &App, t: Tokens) -> El<'_> {
    let left: El<'_> = match app.jobs.active.iter().next() {
        Some((id, (kind, name, _))) => {
            let verb = match kind {
                JobKind::Install => "Installing",
                JobKind::Update => "Updating",
            };
            let pct = app
                .jobs
                .snapshot
                .get(id)
                .and_then(|(done, total)| total.filter(|t| *t > 0).map(|t| done * 100 / t))
                .map(|p| format!(" · {p}%"))
                .unwrap_or_default();
            meta(
                format!(
                    "{verb} {} of {} · {name}{pct}",
                    (app.jobs.done + 1).min(app.jobs.total),
                    app.jobs.total
                ),
                t,
            )
            .into()
        }
        None if app.checking => meta("Checking for updates…", t).into(),
        None if app.loading => meta("Loading…", t).into(),
        None => button(
            row![icon(Icon::Keyboard), meta("? for shortcuts", t)]
                .spacing(theme::XS)
                .align_y(Alignment::Center),
        )
        .padding(0)
        .style(theme::link_button(t))
        .on_press(Message::ToggleShortcuts)
        .into(),
    };
    let mut bar = row![left, Space::new().width(Length::Fill)]
        .align_y(Alignment::Center)
        .spacing(theme::MD)
        .height(theme::STATUS_BAR_HEIGHT)
        .padding(Padding::from([0.0, theme::MD]));
    if app.game_running {
        let label = match app.queued.len() {
            0 => "WoW is running".to_owned(),
            n => format!("WoW is running · {n} waiting"),
        };
        bar = bar.push(pill(label, t.warning));
    }
    container(bar)
        .width(Length::Fill)
        .style(theme::surface(t))
        .into()
}

fn error_panel(app: &App, t: Tokens) -> El<'_> {
    let Some(err) = &app.last_error else {
        return Space::new().into();
    };
    let mut actions = row![].spacing(theme::SM);
    if let Some((label, retry)) = &err.retry {
        actions = actions.push(action(*label, Some((**retry).clone()), t));
    }
    if let Some(page) = &err.page {
        actions = actions.push(action(
            "Open download page",
            Some(Message::OpenUrl(page.clone())),
            t,
        ));
    }
    actions = actions.push(action("Dismiss", Some(Message::DismissError), t));
    container(column![body(err.message.as_str(), t).color(t.danger), actions].spacing(theme::MD))
        .padding(theme::LG)
        .width(Length::Fill)
        .style(theme::panel(t))
        .into()
}

/// Notifications, newest at the bottom, in the bottom-right corner.
fn toasts(app: &App, t: Tokens) -> El<'_> {
    let list = column(app.toasts.iter().map(|toast| {
        let mut r = row![
            body(toast.text.as_str(), t)
                .color(if toast.error { t.danger } else { t.text })
                .width(Length::Fill)
        ]
        .spacing(theme::SM)
        .align_y(Alignment::Center);
        if let Some((label, msg)) = &toast.action {
            let i = match *label {
                "Undo" => Some(Icon::Undo),
                "Retry" => Some(Icon::Refresh),
                _ => None,
            };
            r = r.push(action_i(i, *label, Some((**msg).clone()), t));
        }
        r = r.push(icon_button(
            Icon::X,
            "Dismiss",
            Message::DismissToast(toast.id),
            t,
        ));
        container(r)
            .padding(theme::MD)
            .width(theme::TOAST_WIDTH)
            .style(theme::toast(t, toast.error))
            .into()
    }))
    .spacing(theme::SM);
    container(list)
        .padding(theme::LG)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_right(Length::Fill)
        .align_bottom(Length::Fill)
        .into()
}

const SHORTCUTS: [(&str, &str); 12] = [
    (
        "Ctrl+1 … 8",
        "Installed, Browse, Updates, Profiles, Characters, Health, History, Settings",
    ),
    ("Ctrl+F", "Search or filter"),
    ("↑ ↓ PgUp PgDn Home End", "Move through the list"),
    ("Enter", "Open details, release notes or the addon page"),
    ("Space", "Tick the row for bulk actions"),
    ("Ctrl+A", "Tick every row"),
    ("U", "Update the row"),
    ("Delete", "Remove the row"),
    ("Ctrl+R", "Check for updates"),
    ("F5", "Rescan the AddOns folder"),
    ("Esc", "Close panels, or clear ticks"),
    ("?", "Show or hide this list"),
];

fn shortcuts<'a>(t: Tokens) -> El<'a> {
    let mut list = column![
        row![
            title("Keyboard shortcuts", t).width(Length::Fill),
            icon_button(Icon::X, "Close", Message::ToggleShortcuts, t),
        ]
        .align_y(Alignment::Center)
    ]
    .spacing(theme::SM);
    for (keys, what) in SHORTCUTS {
        list = list.push(
            row![
                mono(keys, t).width(Length::FillPortion(2)),
                body(what, t).width(Length::FillPortion(3))
            ]
            .spacing(theme::MD)
            .align_y(Alignment::Center),
        );
    }
    container(
        container(list)
            .padding(theme::LG)
            .width(theme::SHORTCUTS_WIDTH)
            .style(theme::toast(t, false)),
    )
    .center(Length::Fill)
    .into()
}

// ---- Installed ----------------------------------------------------------------------------

/// Status pill for an installed entry, in priority order.
fn entry_status(app: &App, e: &LockEntry, t: Tokens) -> (&'static str, Color) {
    let check = app.checks.get(&e.id);
    if app.loaded.is_disabled(e) {
        ("disabled", t.text_muted)
    } else if !app.loaded.is_present(e) {
        ("missing", t.danger)
    } else if e.pinned {
        ("pinned", t.text_muted)
    } else if check.is_some_and(UpdateCheck::has_update) {
        ("update", t.accent)
    } else if app.loaded.toc_out_of_date(e) {
        ("out-of-date TOC", t.warning)
    } else if check.is_some_and(|c| c.error.is_some()) {
        ("check failed", t.warning)
    } else if check.is_some() {
        ("up to date", t.success)
    } else if app.checking {
        ("checking", t.text_muted)
    } else {
        ("not checked", t.text_muted)
    }
}

/// Health problems touching any of these folders.
fn issues_for<'a>(app: &'a App, folders: &[String]) -> Vec<&'a Issue> {
    app.loaded
        .issues
        .iter()
        .filter(|i| {
            i.folders()
                .iter()
                .any(|f| folders.iter().any(|g| g.eq_ignore_ascii_case(f)))
        })
        .collect()
}

/// A warning marker for rows with health problems; it explains on hover and opens Health.
fn health_marker<'a>(app: &'a App, folders: &[String], t: Tokens) -> Option<El<'a>> {
    let issues = issues_for(app, folders);
    if issues.is_empty() {
        return None;
    }
    let kinds: Vec<&str> = issues.iter().map(|i| issue_kind(i)).collect();
    Some(
        tooltip(
            button(icon(Icon::TriangleAlert).color(t.warning))
                .padding(0)
                .style(theme::link_button(t))
                .on_press(Message::Nav(Screen::Health)),
            container(meta(format!("{}; see Health", kinds.join(", ")), t))
                .padding(Padding::from([theme::XS, theme::SM]))
                .style(theme::panel(t)),
            tooltip::Position::Top,
        )
        .into(),
    )
}

/// A name cell: title (with a health marker) and an optional second line.
fn name_cell<'a>(
    app: &'a App,
    name: &'a str,
    sub: Option<String>,
    folders: &[String],
    t: Tokens,
) -> El<'a> {
    let mut head = row![single_line(strong(name, t))]
        .spacing(theme::XS)
        .align_y(Alignment::Center);
    if let Some(m) = health_marker(app, folders, t) {
        head = head.push(m);
    }
    let mut col = column![head];
    if let Some(sub) = sub {
        col = col.push(single_line(meta(sub, t)));
    }
    col.into()
}

/// Tick box for bulk actions.
fn pick_box<'a>(app: &App, id: &AddonRef, t: Tokens) -> El<'a> {
    let id = id.clone();
    checkbox(app.picked.contains(&id))
        .style(theme::check(t))
        .on_toggle(move |_| Message::Pick(id.clone()))
        .into()
}

fn matches_filter(filter: &str, haystacks: &[&str]) -> bool {
    let f = filter.trim().to_lowercase();
    f.is_empty() || haystacks.iter().any(|h| h.to_lowercase().contains(&f))
}

const INSTALLED_COLS: [Spec; 6] = [
    spec(Col::Select, "", false),
    spec(Col::Name, "Name", true),
    spec(Col::Installed, "Installed", true),
    spec(Col::Latest, "Latest", true),
    spec(Col::Source, "Source", true),
    spec(Col::Status, "Status", true),
];

/// Toolbar plus the table's sticky header and filter rows; for tickable tables, the bulk bar
/// and select-all state.
fn with_header<'a>(
    app: &'a App,
    toolbar: impl Into<El<'a>>,
    id: TableId,
    specs: &[Spec],
    widths: &[f32],
    trailing: f32,
    t: Tokens,
) -> El<'a> {
    let view = app.tables.get(id);
    let select = specs.iter().any(|s| s.col == Col::Select).then(|| {
        let ids: Vec<AddonRef> = row_keys(app)
            .into_iter()
            .filter_map(|k| match k {
                RowKey::Addon(id) => Some(id),
                _ => None,
            })
            .collect();
        let all = !ids.is_empty() && ids.iter().all(|id| app.picked.contains(id));
        (all, Message::PickAll(ids))
    });
    let tickable = select.is_some();
    let mut col = column![toolbar.into()].spacing(theme::SM);
    if tickable && !app.picked.is_empty() {
        col = col.push(bulk_bar(app, t));
    }
    col = col.push(table::header(id, specs, widths, view, trailing, select, t));
    if specs.iter().any(|s| s.filter) {
        col = col.push(table::filters(id, specs, widths, view, trailing, t));
    }
    col.into()
}

/// Actions for the ticked rows.
fn bulk_bar(app: &App, t: Tokens) -> El<'_> {
    let n = app.picked.len();
    let updatable = app
        .picked
        .iter()
        .filter(|id| app.checks.get(id).is_some_and(UpdateCheck::has_update))
        .count();
    let update_label = format!("Update {updatable}");
    container(
        row![
            strong(format!("{n} selected"), t).width(Length::Fill),
            button(labeled(
                Some(Icon::UpdateArrow),
                update_label,
                theme::FONT_STRONG
            ))
            .padding(Padding::from([theme::XS, theme::MD]))
            .style(theme::primary_button(t))
            .on_press_maybe((updatable > 0).then_some(Message::BulkUpdate)),
            action_i(
                Some(Icon::PowerOff),
                "Disable",
                Some(Message::BulkDisable),
                t
            ),
            danger("Remove", Message::BulkRemove, t),
            action_i(Some(Icon::X), "Clear", Some(Message::ClearPicks), t),
        ]
        .spacing(theme::SM)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([theme::XS, theme::MD]))
    .style(theme::panel(t))
    .into()
}

/// Rows the keyboard cursor moves over on the current screen, in display order.
pub fn row_keys(app: &App) -> Vec<RowKey> {
    let t = app.tokens();
    match app.screen {
        Screen::Installed => installed_items(app, t)
            .into_iter()
            .map(|item| match item {
                Item::Managed(e) => RowKey::Addon(e.id.clone()),
                Item::Local(_) => RowKey::Local,
            })
            .collect(),
        Screen::Updates => pending_updates(app)
            .into_iter()
            .map(|c| RowKey::Addon(c.entry.id.clone()))
            .collect(),
        Screen::Browse if app.page.is_none() => browse_hits(app)
            .into_iter()
            .map(|h| RowKey::Hit(h.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

fn widths(app: &App, id: TableId, specs: &[Spec]) -> Vec<f32> {
    specs.iter().map(|s| app.col_width(id, s.col)).collect()
}

/// An Installed row: an addon kodo manages, or a folder it doesn't.
enum Item<'a> {
    Managed(&'a LockEntry),
    Local(&'a LocalAddon),
}

fn local_name(l: &LocalAddon) -> &str {
    l.toc
        .as_ref()
        .and_then(|t| t.title.as_deref())
        .unwrap_or(&l.folder)
}

fn latest_version<'a>(app: &'a App, e: &LockEntry) -> Option<&'a str> {
    app.checks
        .get(&e.id)
        .and_then(|c| c.latest.as_ref())
        .map(|r| r.version.as_str())
}

fn installed_key(app: &App, item: &Item<'_>, col: Col, t: Tokens) -> Key {
    let s = match item {
        Item::Managed(e) => match col {
            Col::Name => e.display_name().to_owned(),
            Col::Installed => e.version.clone(),
            Col::Latest => latest_version(app, e).unwrap_or_default().to_owned(),
            Col::Source => e.source.label().to_owned(),
            Col::Status => entry_status(app, e, t).0.to_owned(),
            _ => String::new(),
        },
        Item::Local(l) => match col {
            Col::Name => local_name(l).to_owned(),
            Col::Installed => l
                .toc
                .as_ref()
                .and_then(|t| t.version.clone())
                .unwrap_or_default(),
            Col::Latest => app
                .loaded
                .suggestions
                .get(&l.folder)
                .map(|id| format!("looks like {id}"))
                .unwrap_or_default(),
            Col::Source => "local".to_owned(),
            Col::Status => local_status(l).to_owned(),
            _ => String::new(),
        },
    };
    Key::text(&s)
}

fn local_status(l: &LocalAddon) -> &'static str {
    if l.out_of_date {
        "out-of-date TOC"
    } else {
        "local"
    }
}

/// Installed rows after the filter and sort, as drawn.
fn installed_items(app: &App, t: Tokens) -> Vec<Item<'_>> {
    let mut items: Vec<Item<'_>> = app
        .loaded
        .entries
        .iter()
        .filter(|e| matches_filter(&app.filter, &[e.display_name(), &e.id.to_string()]))
        .map(Item::Managed)
        .chain(
            app.loaded
                .local
                .iter()
                .filter(|l| matches_filter(&app.filter, &[&l.folder, local_name(l)]))
                .map(Item::Local),
        )
        .collect();
    app.tables
        .get(TableId::Installed)
        .apply(&mut items, |item, col| installed_key(app, item, col, t));
    items
}

fn installed(app: &App, t: Tokens) -> (El<'_>, El<'_>) {
    let n = app.update_count();
    let check_label = if app.checking {
        "Checking…"
    } else {
        "Check for updates"
    };
    let toolbar = row![
        search_field("Filter installed addons", &app.filter, Message::Filter, t),
        action_i(
            Some(Icon::Refresh),
            check_label,
            (!app.checking).then_some(Message::CheckUpdates),
            t
        ),
        primary_i(
            Some(Icon::UpdateArrow),
            "Update all",
            (n > 0).then_some(Message::UpdateAll),
            t
        ),
    ]
    .spacing(theme::SM)
    .align_y(Alignment::Center);
    let id = TableId::Installed;
    let w = widths(app, id, &INSTALLED_COLS);

    let mut list = column![].spacing(theme::XS);
    for (i, item) in installed_items(app, t).into_iter().enumerate() {
        match item {
            Item::Managed(e) => {
                list = list.push(at_cursor(app, i, entry_row(app, e, &w, t), t));
                if app.menu.as_ref() == Some(&e.id) {
                    list = list.push(row_menu(app, e, t));
                }
            }
            Item::Local(l) => list = list.push(at_cursor(app, i, local_row(app, l, &w, t), t)),
        }
    }
    if app.loaded.entries.is_empty() && app.loaded.local.is_empty() && !app.loading {
        list = list.push(
            container(meta(
                if app.clients.is_empty() {
                    "No client yet. Add your WoW folder in Settings."
                } else {
                    "No addons installed. Find some in Browse."
                },
                t,
            ))
            .padding(theme::XL),
        );
    }
    (
        with_header(
            app,
            toolbar,
            id,
            &INSTALLED_COLS,
            &w,
            theme::ACTIONS_HOVER,
            t,
        ),
        list.into(),
    )
}

fn local_row<'a>(app: &'a App, l: &'a LocalAddon, w: &[f32], t: Tokens) -> El<'a> {
    let version = l
        .toc
        .as_ref()
        .and_then(|t| t.version.clone())
        .unwrap_or_default();
    let status = if l.out_of_date {
        pill(local_status(l), t.warning)
    } else {
        pill(local_status(l), t.text_muted)
    };
    let suggestion = app.loaded.suggestions.get(&l.folder);
    let hint: El<'_> = match suggestion {
        Some(id) => single_line(meta(format!("looks like {id}"), t)).into(),
        None => meta("—", t).into(),
    };
    // Linking replaces the hand-installed copy with the source's build (old copy backed up).
    let link_button: El<'_> = match suggestion {
        Some(id) => action("Link", Some(Message::Install(id.clone(), true)), t),
        None => Space::new().into(),
    };
    container(table::cells(
        vec![
            Space::new().into(),
            name_cell(
                app,
                local_name(l),
                stale_hint(app, std::slice::from_ref(&l.folder)),
                std::slice::from_ref(&l.folder),
                t,
            ),
            single_line(mono(version, t)).into(),
            hint,
            source_pill(None, t),
            status,
        ],
        w,
        link_button,
        theme::ACTIONS_HOVER,
        t.row,
    ))
    .padding(Padding::from([0.0, theme::MD]))
    .into()
}

fn entry_row<'a>(app: &'a App, e: &'a LockEntry, w: &[f32], t: Tokens) -> El<'a> {
    let latest = latest_version(app, e).unwrap_or("—");
    let (status, color) = entry_status(app, e, t);
    let selected = app.detail.as_ref() == Some(&e.id);
    let folders = (e.folders.len() > 1).then(|| e.folders.join(", "));
    let sub = match (folders, stale_hint(app, &e.folders)) {
        (Some(f), Some(s)) => Some(format!("{f} · {s}")),
        (f, s) => f.or(s),
    };
    let status_cell = match app.jobs.progress(&e.id) {
        Some((kind, pct)) => job_progress(kind, pct, t),
        None => pill(status, color),
    };
    let content = table::cells(
        vec![
            pick_box(app, &e.id, t),
            name_cell(app, e.display_name(), sub, &e.folders, t),
            single_line(mono(e.version.as_str(), t)).into(),
            single_line(mono(latest, t)).into(),
            source_pill(Some(e.source), t),
            status_cell,
        ],
        w,
        quick_actions(app, e, t),
        theme::ACTIONS_HOVER,
        t.row,
    );
    mouse_area(
        button(content)
            .width(Length::Fill)
            .padding(Padding::from([0.0, theme::MD]))
            .style(theme::ghost_button(t, selected))
            .on_press(Message::OpenDetail(e.id.clone())),
    )
    .on_right_press(Message::ToggleMenu(e.id.clone()))
    .on_enter(Message::Hover(Some(e.id.clone())))
    .on_exit(Message::Hover(None))
    .into()
}

/// Icon actions on the hovered row, then the ⋯ menu.
fn quick_actions<'a>(app: &'a App, e: &'a LockEntry, t: Tokens) -> El<'a> {
    let mut r = row![].spacing(theme::XS).align_y(Alignment::Center);
    if app.hovered.as_ref() == Some(&e.id) && app.jobs.progress(&e.id).is_none() {
        if app.checks.get(&e.id).is_some_and(UpdateCheck::has_update) {
            r = r.push(icon_button(
                Icon::UpdateArrow,
                "Update",
                Message::Update(e.id.clone()),
                t,
            ));
        }
        let (i, tip) = if e.pinned {
            (Icon::PinOff, "Unpin")
        } else {
            (Icon::Pin, "Pin (skip updates)")
        };
        r = r
            .push(icon_button(
                i,
                tip,
                Message::Pin(e.id.clone(), !e.pinned),
                t,
            ))
            .push(icon_button(
                Icon::Trash,
                "Remove",
                Message::Remove(e.id.clone(), false),
                t,
            ));
    }
    r.push(icon_button(
        Icon::Ellipsis,
        "More actions",
        Message::ToggleMenu(e.id.clone()),
        t,
    ))
    .into()
}

/// A download or install in progress, in place of the row's status.
fn job_progress<'a>(kind: JobKind, pct: Option<u64>, t: Tokens) -> El<'a> {
    let verb = match kind {
        JobKind::Install => "Installing",
        JobKind::Update => "Updating",
    };
    let label = pct.map_or_else(|| format!("{verb}…"), |p| format!("{verb} {p}%"));
    column![
        meta(label, t),
        progress_bar(0.0..=100.0, pct.unwrap_or(0) as f32)
            .girth(theme::XS)
            .style(theme::progress(t)),
    ]
    .spacing(theme::XS)
    .into()
}

/// Addons whose settings are a month older than the newest any addon has were not loaded in
/// recent play. Measuring from the newest (not from today) keeps a break from the game from
/// flagging everything. Addons with no saved settings get no hint.
const STALE_AFTER: u64 = 30 * 24 * 60 * 60;

fn stale_hint(app: &App, folders: &[String]) -> Option<String> {
    let last = &app.loaded.last_loaded;
    let newest = last.values().max()?;
    let mine = folders
        .iter()
        .filter_map(|f| last.get(&f.to_lowercase()))
        .max()?;
    (newest.saturating_sub(*mine) > STALE_AFTER).then(|| {
        let date = rfc3339(*mine);
        format!("not loaded since {}", date.get(..10).unwrap_or(&date))
    })
}

/// The row menu (⋯ button or right-click): the same actions as the detail panel.
fn row_menu<'a>(app: &'a App, e: &'a LockEntry, t: Tokens) -> El<'a> {
    container(entry_actions(app, e, t))
        .padding(theme::SM)
        .width(Length::Fill)
        .style(theme::panel(t))
        .into()
}

fn entry_actions<'a>(app: &'a App, e: &'a LockEntry, t: Tokens) -> El<'a> {
    let has_update = app.checks.get(&e.id).is_some_and(UpdateCheck::has_update);
    let has_backup = app.loaded.backups.get(&e.id).is_some_and(|b| !b.is_empty());
    let mut r = row![].spacing(theme::SM).align_y(Alignment::Center);
    if has_update {
        r = r.push(primary_i(
            Some(Icon::UpdateArrow),
            "Update",
            Some(Message::Update(e.id.clone())),
            t,
        ));
    }
    r = r.push(action_i(
        Some(if e.pinned { Icon::PinOff } else { Icon::Pin }),
        if e.pinned { "Unpin" } else { "Pin" },
        Some(Message::Pin(e.id.clone(), !e.pinned)),
        t,
    ));
    r = r.push(action_i(
        Some(Icon::Undo),
        "Roll back",
        has_backup.then(|| Message::Rollback(e.id.clone())),
        t,
    ));
    let (i, toggle, msg) = if app.loaded.is_disabled(e) {
        (Icon::Power, "Enable", Message::Enable(e.id.to_string()))
    } else {
        (
            Icon::PowerOff,
            "Disable",
            Message::Disable(e.id.to_string()),
        )
    };
    r = r.push(action_i(Some(i), toggle, Some(msg), t));
    if let Some(url) = page_url(app, &e.id) {
        r = r.push(action_i(
            Some(Icon::ExternalLink),
            "Open page",
            Some(Message::OpenUrl(url)),
            t,
        ));
    }
    r.push(Space::new().width(Length::Fill))
        .push(danger("Remove", Message::Remove(e.id.clone(), false), t))
        .wrap()
        .into()
}

fn page_url(app: &App, id: &AddonRef) -> Option<String> {
    app.checks
        .get(id)
        .and_then(|c| c.latest.as_ref())
        .and_then(|r| r.page_url.clone())
        .or_else(|| match id.source {
            SourceId::Github => Some(format!("https://github.com/{}", id.id)),
            SourceId::Wowi => Some(format!(
                "https://www.wowinterface.com/downloads/info{}",
                id.id
            )),
            SourceId::Curseforge | SourceId::Wago => None,
        })
}

fn snippet(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= CHANGELOG_SNIPPET {
        return flat;
    }
    let cut: String = flat.chars().take(CHANGELOG_SNIPPET).collect();
    format!("{cut}…")
}

fn detail_panel(app: &App, t: Tokens) -> Option<El<'_>> {
    let id = app.detail.as_ref()?;
    let e = app.entry(id)?;
    let check = app.checks.get(id);
    let latest = check.and_then(|c| c.latest.as_ref());
    let mut col = column![
        row![
            single_line(title(e.display_name(), t)).width(Length::Fill),
            link("Close", Message::ClosePanels, t),
        ]
        .align_y(Alignment::Center),
        mono(e.id.to_string(), t),
        divider(t),
        row![
            meta("Installed", t).width(Length::Fill),
            mono(e.version.as_str(), t)
        ],
        row![
            meta("Latest", t).width(Length::Fill),
            mono(latest.map_or("—", |r| r.version.as_str()), t)
        ],
    ]
    .spacing(theme::SM);
    if let Some(err) = check.and_then(|c| c.error.as_deref()) {
        col = col.push(meta(err, t).color(t.warning));
    }
    if let Some(notes) = latest.and_then(|r| r.changelog.as_deref()) {
        col = col.push(meta(snippet(notes), t));
    }
    col = col.push(divider(t)).push(
        row![
            meta("Channel", t).width(Length::Fill),
            pick_list(
                [Channel::Stable, Channel::Beta, Channel::Alpha].map(ChannelChoice),
                Some(ChannelChoice(e.channel)),
                {
                    let id = id.clone();
                    move |c: ChannelChoice| Message::SetChannel(id.clone(), c.0)
                },
            )
            .text_size(theme::TEXT_BODY)
            .padding(Padding::from([theme::XS, theme::SM]))
            .style(theme::select(t)),
        ]
        .align_y(Alignment::Center),
    );
    col = col.push(
        checkbox(e.pinned)
            .label("Pinned (skip updates)")
            .text_size(theme::TEXT_BODY)
            .style(theme::check(t))
            .on_toggle({
                let id = id.clone();
                move |b| Message::Pin(id.clone(), b)
            }),
    );
    col = col.push(divider(t)).push(meta("Folders", t));
    for f in &e.folders {
        col = col.push(mono(f.as_str(), t));
    }
    let backups = app.loaded.backups.get(id).cloned().unwrap_or_default();
    col = col
        .push(divider(t))
        .push(meta("Versions to roll back to", t));
    if backups.is_empty() {
        col = col.push(meta("none yet", t));
    }
    for v in backups {
        col = col.push(mono(v, t));
    }
    let mut links = row![].spacing(theme::MD);
    if let Some(url) = page_url(app, id) {
        links = links.push(link("Project page", Message::OpenUrl(url), t));
    }
    if let Some(c) = app.client() {
        links = links.push(link(
            "AddOns folder",
            Message::OpenPath(c.addons.clone()),
            t,
        ));
    }
    col = col
        .push(divider(t))
        .push(links)
        .push(entry_actions(app, e, t));
    Some(
        container(scroll(col.padding(theme::LG)).height(Length::Fill))
            .width(theme::DETAIL_WIDTH)
            .height(Length::Fill)
            .style(theme::panel(t))
            .into(),
    )
}

// ---- Browse -------------------------------------------------------------------------------

fn browse(app: &App, t: Tokens) -> (El<'_>, El<'_>) {
    if let Some(page) = &app.page {
        return addon_page(app, page, t);
    }
    let searching = !app.search.trim().is_empty();
    let top = row![
        container(search_field(
            "Search, or browse the catalogue",
            &app.search,
            Message::Search,
            t
        ))
        .width(Length::FillPortion(3)),
        text_input(
            "Paste a GitHub or WoWInterface link, or a .zip path",
            &app.link_input
        )
        .on_input(Message::LinkInput)
        .on_submit(Message::LinkSubmit)
        .size(theme::TEXT_BODY)
        .font(theme::FONT)
        .padding(theme::SM)
        .width(Length::FillPortion(2))
        .style(theme::input(t)),
        icon_button(
            Icon::Link,
            "Install the pasted link",
            Message::LinkSubmit,
            t
        ),
        icon_button(
            Icon::FileArchive,
            "Install a .zip from disk (or drop one on the window)",
            Message::PickZip,
            t
        ),
    ]
    .spacing(theme::SM)
    .align_y(Alignment::Center);

    // Server-side order: Relevance falls back to popularity without search text.
    let mut sorts = vec![];
    if searching {
        sorts.push((Sort::Relevance, "Best match"));
    }
    sorts.extend([
        (Sort::Downloads, "Popular"),
        (Sort::Updated, "Recently updated"),
        (Sort::Name, "A–Z"),
    ]);
    let current = match app.sort {
        Sort::Relevance if !searching => Sort::Downloads,
        s => s,
    };
    let mut chips = row![].spacing(theme::XS).align_y(Alignment::Center);
    for (sort, label) in sorts {
        chips = chips.push(chip(label, current == sort, Message::BrowseSort(sort), t));
    }
    chips = chips.push(Space::new().width(theme::MD));
    chips = chips.push(chip(
        "All sources",
        app.source_filter.is_none(),
        Message::SourceFilter(None),
        t,
    ));
    for src in app.engine.iter().flat_map(Engine::source_ids) {
        chips = chips.push(chip(
            src.label(),
            app.source_filter == Some(src),
            Message::SourceFilter(Some(src)),
            t,
        ));
    }
    let flavor = app.client().map_or("Forever", |c| c.flavor.label());
    chips = chips
        .push(Space::new().width(theme::MD))
        .push(chip(
            format!("{flavor} builds only"),
            app.builds_only,
            Message::BuildsOnly(!app.builds_only),
            t,
        ))
        .push(Space::new().width(Length::Fill))
        .push(icon_toggle(
            Icon::List,
            "List",
            !app.browse_cards,
            Message::BrowseCards(false),
            t,
        ))
        .push(icon_toggle(
            Icon::LayoutGrid,
            "Cards",
            app.browse_cards,
            Message::BrowseCards(true),
            t,
        ));
    let toolbar: El<'_> = column![top, chips.wrap()].spacing(theme::SM).into();

    let id = TableId::Browse;
    let w = widths(app, id, &BROWSE_COLS);
    let mut list = column![].spacing(theme::XS);
    if !searching {
        list = list.push(
            container(meta(
                format!(
                    "Browsing the GitHub catalogue for {flavor}. Type to search WoWInterface and CurseForge too."
                ),
                t,
            ))
            .padding(Padding::from([theme::XS, theme::MD])),
        );
    }
    for e in &app.search_errors {
        list = list.push(meta(e.as_str(), t).color(t.warning));
    }
    let hits = browse_hits(app);
    if app.engine.is_none() {
        list = list.push(container(meta("Starting sources…", t)).padding(theme::XL));
    } else if app.searching && app.results.is_empty() {
        list = list.push(container(meta("Searching…", t)).padding(theme::XL));
    } else if hits.is_empty() {
        list = list.push(container(meta("No results.", t)).padding(theme::XL));
    }
    if app.browse_cards {
        let cards = hits
            .into_iter()
            .enumerate()
            .map(|(i, hit)| at_cursor(app, i, hit_card(app, hit, t), t));
        list = list.push(Row::with_children(cards).spacing(theme::SM).wrap());
    } else {
        for (i, hit) in hits.into_iter().enumerate() {
            list = list.push(at_cursor(app, i, hit_row(app, hit, &w, t), t));
        }
    }
    // A full page means there may be more.
    if app.results.len() >= app.search_limit {
        let label = if app.searching {
            "Loading…"
        } else {
            "Show more"
        };
        list = list.push(
            container(action(
                label,
                (!app.searching).then_some(Message::ShowMore),
                t,
            ))
            .center_x(Length::Fill)
            .padding(theme::MD),
        );
    }
    let header = if app.browse_cards {
        toolbar
    } else {
        with_header(app, toolbar, id, &BROWSE_COLS, &w, theme::ACTIONS_WIDE, t)
    };
    (header, list.into())
}

fn hit_row<'a>(app: &'a App, hit: &'a RemoteAddon, w: &[f32], t: Tokens) -> El<'a> {
    let mut info = column![single_line(strong(hit.name.as_str(), t))];
    if let Some(s) = &hit.summary {
        info = info.push(single_line(meta(s.as_str(), t)));
    }
    let mut actions = row![].spacing(theme::MD).align_y(Alignment::Center);
    if let Some(url) = &hit.page_url {
        actions = actions.push(link("Page", Message::OpenUrl(url.clone()), t));
    }
    actions = actions.push(install_button(app, &hit.addon, t));
    container(table::cells(
        vec![
            button(info)
                .padding(0)
                .width(Length::Fill)
                .style(theme::ghost_button(t, false))
                .on_press(Message::OpenPage(hit.clone()))
                .into(),
            single_line(meta(hit.author.clone().unwrap_or_default(), t)).into(),
            mono(hit.downloads.map(human_count).unwrap_or_default(), t).into(),
            mono(hit.updated.as_deref().unwrap_or("—"), t).into(),
            source_pill(Some(hit.addon.source), t),
        ],
        w,
        actions.into(),
        theme::ACTIONS_WIDE,
        t.row,
    ))
    .padding(Padding::from([0.0, theme::MD]))
    .into()
}

fn hit_card<'a>(app: &'a App, hit: &'a RemoteAddon, t: Tokens) -> El<'a> {
    let mut facts = row![source_pill(Some(hit.addon.source), t)]
        .spacing(theme::SM)
        .align_y(Alignment::Center);
    if let Some(d) = hit.downloads {
        facts = facts.push(meta(human_count(d), t));
    }
    if let Some(u) = &hit.updated {
        facts = facts.push(meta(u.as_str(), t));
    }
    let mut body_col = column![single_line(strong(hit.name.as_str(), t))].spacing(theme::XS);
    if let Some(a) = &hit.author {
        body_col = body_col.push(single_line(meta(format!("by {a}"), t)));
    }
    body_col = body_col.push(
        meta(hit.summary.as_deref().map(snippet).unwrap_or_default(), t)
            .height(Length::Fixed(theme::XXL + theme::SM)),
    );
    let card = column![
        button(body_col)
            .padding(0)
            .width(Length::Fill)
            .style(theme::ghost_button(t, false))
            .on_press(Message::OpenPage(hit.clone())),
        facts,
        install_button(app, &hit.addon, t),
    ]
    .spacing(theme::SM);
    container(card)
        .padding(theme::MD)
        .width(theme::CARD_WIDTH)
        .style(theme::panel(t))
        .into()
}

/// A filter or sort choice in Browse's chip row.
fn chip<'a>(label: impl text::IntoFragment<'a>, on: bool, msg: Message, t: Tokens) -> El<'a> {
    button(text(label).size(theme::TEXT_META).font(theme::FONT))
        .padding(Padding::from([theme::XS, theme::SM]))
        .style(theme::chip(t, on))
        .on_press(msg)
        .into()
}

/// An icon button that shows whether it's the current choice.
fn icon_toggle<'a>(i: Icon, tip: &'a str, on: bool, msg: Message, t: Tokens) -> El<'a> {
    tooltip(
        button(icon(i).color(if on { t.accent } else { t.text_muted }))
            .padding(theme::XS)
            .style(theme::ghost_button(t, on))
            .on_press(msg),
        container(meta(tip, t))
            .padding(Padding::from([theme::XS, theme::SM]))
            .style(theme::panel(t)),
        tooltip::Position::Top,
    )
    .into()
}

/// Browse results in table order.
fn browse_hits(app: &App) -> Vec<&RemoteAddon> {
    // Only drop hits the catalogue says build for other game versions; unknown stays.
    let tracks = app.client().map_or(&[][..], |c| c.flavor.catalog_tracks());
    let mut hits: Vec<&RemoteAddon> = app
        .results
        .iter()
        .filter(|h| {
            !app.builds_only
                || h.tracks.is_empty()
                || tracks.is_empty()
                || h.tracks.iter().any(|tr| tracks.contains(&tr.as_str()))
        })
        .collect();
    app.tables
        .get(TableId::Browse)
        .apply(&mut hits, |h, col| match col {
            Col::Downloads => Key::Num(h.downloads.unwrap_or(0)),
            Col::Author => Key::text(h.author.as_deref().unwrap_or_default()),
            Col::Updated => Key::text(h.updated.as_deref().unwrap_or_default()),
            Col::Source => Key::text(h.addon.source.label()),
            _ => Key::text(&h.name),
        });
    hits
}

const BROWSE_COLS: [Spec; 5] = [
    spec(Col::Name, "Addon", false),
    spec(Col::Author, "Author", false),
    spec(Col::Downloads, "Downloads", false),
    spec(Col::Updated, "Updated", false),
    spec(Col::Source, "Source", false),
];

fn install_button<'a>(app: &'a App, id: &AddonRef, t: Tokens) -> El<'a> {
    if let Some((kind, pct)) = app.jobs.progress(id) {
        return job_progress(kind, pct, t);
    }
    if app.loaded.entries.iter().any(|e| &e.id == id) {
        meta("Installed", t).into()
    } else {
        primary_i(
            Some(Icon::Download),
            "Install",
            Some(Message::Install(id.clone(), false)),
            t,
        )
    }
}

/// A Browse result's own page: catalogue facts, the latest release for this client, and the
/// README.
fn addon_page<'a>(app: &'a App, page: &'a AddonPage, t: Tokens) -> (El<'a>, El<'a>) {
    let hit = &page.hit;
    let back = button(
        row![
            icon(Icon::ArrowLeft),
            meta("Back to results", t).color(t.accent)
        ]
        .spacing(theme::XS)
        .align_y(Alignment::Center),
    )
    .padding(0)
    .style(theme::link_button(t))
    .on_press(Message::ClosePage);
    let mut toolbar = row![back].spacing(theme::MD).align_y(Alignment::Center);
    toolbar = toolbar.push(Space::new().width(Length::Fill));
    if let Some(url) = &hit.page_url {
        toolbar = toolbar.push(link("Open project page", Message::OpenUrl(url.clone()), t));
    }

    let mut facts = row![source_pill(Some(hit.addon.source), t)]
        .spacing(theme::MD)
        .align_y(Alignment::Center);
    if let Some(a) = &hit.author {
        facts = facts.push(meta(format!("by {a}"), t));
    }
    if let Some(d) = hit.downloads {
        facts = facts.push(meta(format!("{} downloads", human_count(d)), t));
    }
    if let Some(u) = &hit.updated {
        facts = facts.push(meta(format!("updated {u}"), t));
    }
    let mut head = column![
        row![
            title(hit.name.as_str(), t).width(Length::Fill),
            install_button(app, &hit.addon, t)
        ]
        .align_y(Alignment::Center),
        facts,
    ]
    .spacing(theme::SM);
    if !hit.tracks.is_empty() {
        let mut tracks = row![meta("Builds for", t)]
            .spacing(theme::SM)
            .align_y(Alignment::Center);
        for tr in &hit.tracks {
            tracks = tracks.push(pill(tr.as_str(), t.accent));
        }
        head = head.push(tracks);
    }
    if let Some(s) = &hit.summary {
        head = head.push(body(s.as_str(), t));
    }

    let release: El<'_> = match &page.release {
        None => meta("Looking up the latest release…", t).into(),
        Some(Err(e)) => meta(e.as_str(), t).color(t.warning).into(),
        Some(Ok(r)) => {
            let mut c = column![
                row![
                    mono(r.version.as_str(), t),
                    meta(
                        r.published_at
                            .as_deref()
                            .map(|d| d.chars().take(10).collect::<String>())
                            .unwrap_or_default(),
                        t
                    ),
                    meta(r.size.map(human_bytes).unwrap_or_default(), t),
                ]
                .spacing(theme::MD)
                .align_y(Alignment::Center)
            ]
            .spacing(theme::SM);
            if r.download_url.is_none() {
                c = c.push(
                    meta(
                        "The author doesn't allow downloads by apps; use the project page.",
                        t,
                    )
                    .color(t.warning),
                );
            }
            if !page.notes.is_empty() {
                c = c.push(md(&page.notes, &app.images, t));
            }
            c.into()
        }
    };
    let about: El<'_> = match &page.readme {
        None => meta("Loading…", t).into(),
        Some(Err(e)) => meta(e.as_str(), t).color(t.warning).into(),
        Some(Ok(None)) => meta("No README.", t).into(),
        Some(Ok(Some(items))) => md(items, &app.images, t),
    };
    let content = column![
        head,
        section("Latest release", release, t),
        section("About", about, t),
    ]
    .spacing(theme::LG)
    .padding(Padding::from([theme::SM, 0.0]));
    (toolbar.into(), content.into())
}

/// Render parsed Markdown in kodo's fonts and colors. Links open in the browser; images
/// (mostly README badges) are left out.
/// For Markdown that never shows images (release notes).
fn no_images() -> &'static HashMap<String, image::Handle> {
    static EMPTY: std::sync::OnceLock<HashMap<String, image::Handle>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(HashMap::new)
}

fn md<'a>(
    items: &'a [markdown::Item],
    images: &'a HashMap<String, image::Handle>,
    t: Tokens,
) -> El<'a> {
    let style = markdown::Style {
        font: theme::FONT,
        inline_code_highlight: markdown::Highlight {
            background: t.surface_hover.into(),
            border: border::rounded(theme::RADIUS_CONTROL),
        },
        inline_code_padding: Padding::from([0.0, theme::XS]),
        inline_code_color: t.text,
        inline_code_font: theme::MONO,
        code_block_font: theme::MONO,
        link_color: t.accent,
    };
    markdown::view_with(
        items,
        markdown::Settings::with_text_size(theme::TEXT_BODY, style),
        &Viewer { images },
    )
}

/// Links open in the browser; images show once downloaded, scaled down to fit.
struct Viewer<'a> {
    images: &'a HashMap<String, image::Handle>,
}

impl<'a> markdown::Viewer<'a, Message> for Viewer<'a> {
    fn on_link_click(url: markdown::Uri) -> Message {
        Message::OpenUrl(url)
    }

    fn image(
        &self,
        _settings: markdown::Settings,
        url: &'a markdown::Uri,
        _title: &'a str,
        _alt: &markdown::Text,
    ) -> El<'a> {
        match self.images.get(url) {
            Some(h) => image(h.clone())
                .content_fit(iced::ContentFit::ScaleDown)
                .width(Length::Fill)
                .into(),
            None => Space::new().into(),
        }
    }
}

// ---- Profiles -----------------------------------------------------------------------------

fn profiles(app: &App, t: Tokens) -> (El<'_>, El<'_>) {
    let named = !app.profile_name.trim().is_empty();
    let picked = app.loaded.entries.len() - app.profile_skip.len().min(app.loaded.entries.len());
    let toolbar = row![
        text_input("Name a new profile", &app.profile_name)
            .on_input(Message::ProfileName)
            .on_submit_maybe(named.then_some(Message::SaveProfile))
            .size(theme::TEXT_BODY)
            .font(theme::FONT)
            .padding(theme::SM)
            .width(Length::Fill)
            .style(theme::input(t)),
        primary(
            "Save as profile",
            (named && !app.switching).then_some(Message::SaveProfile),
            t
        ),
        action_i(
            Some(Icon::Copy),
            "Copy code",
            (picked > 0).then_some(Message::CopyCode(None)),
            t
        ),
        action_i(
            Some(Icon::QrCode),
            "QR",
            (picked > 0).then_some(Message::ShowQr(None)),
            t
        ),
    ]
    .spacing(theme::SM)
    .align_y(Alignment::Center);

    let set = &app.loaded.profiles;
    let mut list = column![].spacing(theme::SM);
    if app.switching {
        list = list.push(meta("Switching profiles…", t));
    }
    list = list.push(meta(
        "The active profile is always what's installed. Switching puts its addons away and brings the other profile's back, downloading any it doesn't have.",
        t,
    ));
    for p in &set.profiles {
        list = list.push(profile_row(app, p, set.is_active(&p.name), t));
    }
    let profiles_card = section("Profiles", list.into(), t);
    let qr_card = app.qr.as_ref().map(|(label, data)| {
        let code: El<'_> = match data {
            Some(data) => container(qr_code(data).cell_size(QR_CELL).style(|_| qr_code::Style {
                cell: Color::BLACK,
                background: Color::WHITE,
            }))
            .into(),
            None => meta(
                "This code is too long for a QR code; use Copy code instead.",
                t,
            )
            .color(t.warning)
            .into(),
        };
        section(
            "Scan to get the code",
            column![
                row![
                    strong(label.as_str(), t).width(Length::Fill),
                    link("Close", Message::CloseQr, t)
                ]
                .align_y(Alignment::Center),
                code,
            ]
            .spacing(theme::MD)
            .into(),
            t,
        )
    });

    let mut picks = column![meta(
        format!(
            "{picked} of {} installed addons go into a new profile or code. Hand-installed (Local) folders are never part of one.",
            app.loaded.entries.len()
        ),
        t
    )]
    .spacing(theme::XS);
    for e in &app.loaded.entries {
        picks = picks.push(
            checkbox(!app.profile_skip.contains(&e.id))
                .label(e.display_name())
                .text_size(theme::TEXT_BODY)
                .style(theme::check(t))
                .on_toggle({
                    let id = e.id.clone();
                    move |_| Message::ProfileToggle(id.clone())
                }),
        );
    }
    let picks_card = section("What goes in", picks.into(), t);

    let decoded =
        (!app.import_code.trim().is_empty()).then(|| Profile::from_code(&app.import_code));
    let mut import = column![
        row![
            text_input("Paste a kodo1-… code from a friend", &app.import_code)
                .on_input(Message::ImportCode)
                .size(theme::TEXT_BODY)
                .font(theme::MONO)
                .padding(theme::SM)
                .width(Length::Fill)
                .style(theme::input(t)),
            primary(
                "Import",
                matches!(decoded, Some(Ok(_))).then_some(Message::ImportProfile),
                t
            ),
        ]
        .spacing(theme::SM)
        .align_y(Alignment::Center)
    ]
    .spacing(theme::SM);
    match &decoded {
        Some(Ok((p, flavor))) => {
            import = import.push(strong(
                format!(
                    "{} · {} addons · {}",
                    p.name,
                    p.addons.len(),
                    flavor.label()
                ),
                t,
            ));
            if app.client().is_some_and(|c| &c.flavor != flavor) {
                import = import.push(
                    meta(
                        "Made for a different game client; some addons may have no build for this one.",
                        t,
                    )
                    .color(t.warning),
                );
            }
            let names: Vec<String> = p.addons.iter().map(|a| a.display_name()).collect();
            import = import.push(meta(names.join(", "), t));
        }
        Some(Err(e)) => import = import.push(meta(e.to_string(), t).color(t.warning)),
        None => {}
    }
    let import_card = section("Import a code", import.into(), t);
    let list_card = section("Import a list", import_list(app, t), t);

    let mut cards = column![].spacing(theme::LG);
    if let Some(plan) = &app.switch_plan {
        cards = cards.push(switch_preview(app, plan, t));
    }
    cards = cards.push(profiles_card);
    if let Some(qr) = qr_card {
        cards = cards.push(qr);
    }
    (
        toolbar.into(),
        cards
            .push(picks_card)
            .push(import_card)
            .push(list_card)
            .padding(Padding::from([theme::SM, 0.0]))
            .into(),
    )
}

/// Paste a list or WowUp export, match it, tick what to install.
fn import_list(app: &App, t: Tokens) -> El<'_> {
    let empty = app.import_list.text().trim().is_empty();
    let find_label = if app.resolving {
        "Finding…"
    } else {
        "Find matches"
    };
    let mut col = column![
        meta(
            "One addon per line (a name, a GitHub or WoWInterface link, or github:owner/repo), or a WowUp export string.",
            t
        ),
        text_editor(&app.import_list)
            .placeholder("Details\nhttps://github.com/Cidan/BetterBags\n…")
            .on_action(Message::ImportEdit)
            .height(Length::Fixed(theme::XXL * 3.0))
            .size(theme::TEXT_BODY)
            .font(theme::MONO)
            .padding(theme::SM)
            .style(theme::editor(t)),
        row![primary_i(
            Some(Icon::Search),
            find_label,
            (!empty && !app.resolving).then_some(Message::ResolveImport),
            t
        )],
    ]
    .spacing(theme::SM);
    let Some(matches) = &app.import_matches else {
        return col.into();
    };
    for (i, m) in matches.iter().enumerate() {
        let (note, color) = match (&m.addon, m.exact, m.installed) {
            (Some(_), _, true) => ("installed", t.text_muted),
            (Some(_), true, false) => ("match", t.success),
            (Some(_), false, false) => ("near miss", t.warning),
            (None, ..) => ("not found", t.danger),
        };
        let label = if m.name == m.wanted.name {
            m.name.clone()
        } else {
            format!("{} → {}", m.wanted.name, m.name)
        };
        let mut tick = checkbox(!app.import_skip.contains(&i))
            .label(label)
            .text_size(theme::TEXT_BODY)
            .style(theme::check(t));
        if m.addon.is_some() && !m.installed {
            tick = tick.on_toggle(move |_| Message::ImportToggle(i));
        }
        let id = m
            .addon
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        col = col.push(
            row![
                container(tick).width(Length::Fill),
                mono(id, t),
                pill(note, color)
            ]
            .spacing(theme::MD)
            .align_y(Alignment::Center),
        );
    }
    let n = matches.len() - app.import_skip.len().min(matches.len());
    let label = format!("Install {n}");
    col.push(
        button(labeled(Some(Icon::Download), label, theme::FONT_STRONG))
            .padding(Padding::from([theme::XS, theme::MD]))
            .style(theme::primary_button(t))
            .on_press_maybe((n > 0).then_some(Message::InstallImport)),
    )
    .into()
}

/// What a profile switch will do, before it does it.
fn switch_preview<'a>(
    app: &'a App,
    plan: &'a Result<kodo_core::engine::SwitchPlan, String>,
    t: Tokens,
) -> El<'a> {
    let plan = match plan {
        Ok(p) => p,
        Err(e) => {
            return section(
                "Can't switch",
                column![
                    meta(e.as_str(), t).color(t.warning),
                    action("Cancel", Some(Message::CancelSwitch), t)
                ]
                .spacing(theme::MD)
                .into(),
                t,
            );
        }
    };
    let line = |label: &'a str, names: &'a [String], color: Color| -> El<'a> {
        let list = if names.is_empty() {
            "nothing".to_owned()
        } else {
            names.join(", ")
        };
        row![
            meta(label, t).color(color).width(theme::LABEL_WIDTH),
            body(list, t).width(Length::Fill)
        ]
        .spacing(theme::MD)
        .into()
    };
    let mut col = column![
        line("Put away", &plan.stash, t.text_muted),
        line("Bring back", &plan.restore, t.success),
        line("Download", &plan.download, t.accent),
        meta(
            "Put-away addons keep their files and settings in kodo's stash until you switch back.",
            t
        ),
    ]
    .spacing(theme::SM);
    if app.game_running {
        col = col
            .push(meta("WoW is running; the switch waits until you close it.", t).color(t.warning));
    }
    col = col.push(
        row![
            primary_i(
                Some(Icon::Check),
                "Switch",
                (!app.switching).then(|| Message::SwitchProfile(plan.profile.clone())),
                t
            ),
            action("Cancel", Some(Message::CancelSwitch), t),
        ]
        .spacing(theme::SM),
    );
    section(format!("Switch to {}", plan.profile), col.into(), t)
}

fn profile_row<'a>(app: &'a App, p: &'a Profile, is_active: bool, t: Tokens) -> El<'a> {
    let mut name = row![strong(p.name.as_str(), t)]
        .spacing(theme::SM)
        .align_y(Alignment::Center);
    if is_active {
        name = name.push(pill("active", t.success));
    }
    let names: Vec<String> = p.addons.iter().map(|a| a.display_name()).collect();
    let summary = match names.len() {
        0 => "no addons".to_owned(),
        n => format!("{n} addons: {}", names.join(", ")),
    };
    let switch_label = if is_active { "Active" } else { "Switch" };
    let mut r = row![
        column![name, single_line(meta(summary, t))]
            .spacing(theme::XS)
            .width(Length::Fill),
        primary(
            switch_label,
            (!is_active && !app.switching).then(|| Message::PlanSwitch(p.name.clone())),
            t
        ),
        action_i(
            Some(Icon::Copy),
            "Copy code",
            Some(Message::CopyCode(Some(p.name.clone()))),
            t
        ),
        action_i(
            Some(Icon::QrCode),
            "QR",
            Some(Message::ShowQr(Some(p.name.clone()))),
            t
        ),
    ]
    .spacing(theme::SM)
    .align_y(Alignment::Center);
    if !is_active {
        r = r.push(danger("Delete", Message::DeleteProfile(p.name.clone()), t));
    }
    r.into()
}

fn human_count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

// ---- Updates ------------------------------------------------------------------------------

fn updates(app: &App, t: Tokens) -> (El<'_>, El<'_>) {
    let n = app.update_count();
    let check_label = if app.checking {
        "Checking…"
    } else {
        "Check for updates"
    };
    let toolbar = row![
        search_field("Filter updates", &app.filter, Message::Filter, t),
        action_i(
            Some(Icon::Refresh),
            check_label,
            (!app.checking).then_some(Message::CheckUpdates),
            t
        ),
        primary_i(
            Some(Icon::UpdateArrow),
            "Update all",
            (n > 0).then_some(Message::UpdateAll),
            t
        ),
    ]
    .spacing(theme::SM)
    .align_y(Alignment::Center);

    let id = TableId::Updates;
    let w = widths(app, id, &UPDATE_COLS);
    let pending = pending_updates(app);

    let mut list = column![].spacing(theme::XS);
    let digest = app.digest();
    if !digest.is_empty() {
        let mut news = column![].spacing(theme::SM);
        for c in digest {
            let latest = c.latest.as_ref();
            let notes = latest
                .and_then(|r| r.changelog.as_deref())
                .map(snippet)
                .unwrap_or_default();
            news = news.push(
                row![
                    strong(c.entry.display_name(), t).width(theme::COL_NAME),
                    mono(latest.map_or("", |r| r.version.as_str()), t).width(theme::COL_VERSION),
                    single_line(meta(notes, t)).width(Length::Fill),
                ]
                .spacing(theme::MD)
                .align_y(Alignment::Center),
            );
        }
        list = list.push(
            container(
                column![
                    row![
                        icon(Icon::Sparkles).color(t.accent),
                        strong("New since last time", t).width(Length::Fill),
                        action_i(Some(Icon::Check), "Got it", Some(Message::MarkSeen), t),
                    ]
                    .spacing(theme::SM)
                    .align_y(Alignment::Center),
                    news,
                ]
                .spacing(theme::MD),
            )
            .padding(theme::MD)
            .width(Length::Fill)
            .style(theme::panel(t)),
        );
    }
    if pending.is_empty() {
        list = list.push(
            container(meta(
                if app.checks.is_empty() {
                    "Check for updates to see what's new."
                } else {
                    "Everything is up to date."
                },
                t,
            ))
            .padding(theme::XL),
        );
    }
    for (i, c) in pending.into_iter().enumerate() {
        let latest = c.latest.as_ref();
        let open = app.expanded.as_ref() == Some(&c.entry.id);
        let mut info = column![single_line(strong(c.entry.display_name(), t))];
        if let Some(notes) = latest
            .and_then(|r| r.changelog.as_deref())
            .filter(|_| !open)
        {
            info = info.push(single_line(meta(snippet(notes), t)));
        }
        let cells = table::cells(
            vec![
                pick_box(app, &c.entry.id, t),
                info.into(),
                single_line(mono(c.entry.version.as_str(), t)).into(),
                single_line(mono(latest.map_or("", |r| r.version.as_str()), t)).into(),
                source_pill(Some(c.entry.source), t),
            ],
            &w,
            primary_i(
                Some(Icon::UpdateArrow),
                "Update",
                Some(Message::Update(c.entry.id.clone())),
                t,
            ),
            theme::ACTIONS_NARROW,
            t.row,
        );
        let row = button(cells)
            .width(Length::Fill)
            .padding(Padding::from([0.0, theme::MD]))
            .style(theme::ghost_button(t, open))
            .on_press(Message::ToggleChanges(c.entry.id.clone()));
        list = list.push(at_cursor(app, i, row, t));
        if open {
            list = list.push(changes_panel(app.changes.get(&c.entry.id), t));
        }
    }
    (
        with_header(app, toolbar, id, &UPDATE_COLS, &w, theme::ACTIONS_NARROW, t),
        list.into(),
    )
}

/// Release notes from the installed version up to the latest.
fn changes_panel<'a>(notes: Option<&'a Result<Vec<Notes>, String>>, t: Tokens) -> El<'a> {
    let content: El<'a> = match notes {
        None => meta("Loading release notes…", t).into(),
        Some(Err(e)) => meta(e.as_str(), t).color(t.warning).into(),
        Some(Ok(list)) if list.is_empty() => meta("No release notes.", t).into(),
        Some(Ok(list)) => column(list.iter().map(|n| {
            column![
                row![
                    mono(n.version.as_str(), t),
                    meta(n.date.clone().unwrap_or_default(), t)
                ]
                .spacing(theme::MD)
                .align_y(Alignment::Center),
                md(&n.md, no_images(), t),
            ]
            .spacing(theme::SM)
            .into()
        }))
        .spacing(theme::LG)
        .into(),
    };
    container(content)
        .padding(theme::LG)
        .width(Length::Fill)
        .style(theme::panel(t))
        .into()
}

/// Addons with an update, filtered and in table order.
fn pending_updates(app: &App) -> Vec<&UpdateCheck> {
    let mut pending: Vec<&UpdateCheck> = app
        .checks
        .values()
        .filter(|c| c.has_update())
        .filter(|c| matches_filter(&app.filter, &[c.entry.display_name()]))
        .collect();
    pending.sort_by_key(|c| c.entry.display_name().to_lowercase());
    app.tables
        .get(TableId::Updates)
        .apply(&mut pending, |c, col| match col {
            Col::Installed => Key::text(&c.entry.version),
            Col::Latest => Key::text(c.latest.as_ref().map_or("", |r| r.version.as_str())),
            Col::Source => Key::text(c.entry.source.label()),
            _ => Key::text(c.entry.display_name()),
        });
    pending
}

const UPDATE_COLS: [Spec; 5] = [
    spec(Col::Select, "", false),
    spec(Col::Name, "Name", true),
    spec(Col::Installed, "Installed", true),
    spec(Col::Latest, "Latest", true),
    spec(Col::Source, "Source", true),
];

// ---- Characters ---------------------------------------------------------------------------

fn characters(app: &App, t: Tokens) -> (El<'_>, El<'_>) {
    let chars = &app.loaded.characters;
    let current = app
        .character
        .as_deref()
        .and_then(|k| chars.iter().find(|c| c.character.key() == k))
        .or(chars.first());
    let mut toolbar = row![strong(format!("{} characters", chars.len()), t)]
        .spacing(theme::SM)
        .align_y(Alignment::Center);
    if let Some(region) = &app.loaded.region {
        toolbar = toolbar.push(pill(region.as_str(), t.text_muted));
    }
    let Some(current) = current else {
        return (
            toolbar.into(),
            container(meta(
                "No characters yet. Log in to one once and WoW makes its settings folder.",
                t,
            ))
            .padding(theme::XL)
            .into(),
        );
    };
    let key = current.character.key();
    let others: Vec<&CharacterAddons> = chars.iter().filter(|c| c.character.key() != key).collect();

    // Picker: every character with when it was last played.
    let mut picker = column![].spacing(theme::XS).width(theme::SIDEBAR_WIDTH);
    for c in chars {
        let k = c.character.key();
        let label = column![
            single_line(strong(c.character.name.as_str(), t)),
            single_line(meta(
                format!("realm {} · {}", c.character.realm, played(c.last_played)),
                t
            )),
        ]
        .spacing(2.0);
        picker = picker.push(
            button(label)
                .padding(Padding::from([theme::XS, theme::SM]))
                .width(Length::Fill)
                .style(theme::ghost_button(t, k == key))
                .on_press(Message::SelectCharacter(k)),
        );
    }

    let on_for = |c: &CharacterAddons, folders: &[String]| {
        folders
            .iter()
            .all(|f| kodo_core::characters::is_enabled(&c.states, f))
    };
    let set = |folders: &[String], to: bool| {
        Message::CharacterSet(
            key.clone(),
            folders.iter().map(|f| (f.clone(), to)).collect(),
        )
    };
    let mut rows: Vec<(String, Vec<String>, Option<SourceId>)> = app
        .loaded
        .entries
        .iter()
        .map(|e| {
            (
                e.display_name().to_owned(),
                e.folders.clone(),
                Some(e.source),
            )
        })
        .chain(
            app.loaded
                .local
                .iter()
                .map(|l| (local_name(l).to_owned(), vec![l.folder.clone()], None)),
        )
        .collect();
    rows.sort_by_key(|(n, ..)| n.to_lowercase());
    let every: Vec<String> = rows.iter().flat_map(|(_, f, _)| f.clone()).collect();
    let loads = rows.iter().filter(|(_, f, _)| on_for(current, f)).count();

    // Header: who, when, and the actions that change the whole setup.
    let names: Vec<String> = app
        .loaded
        .profiles
        .profiles
        .iter()
        .map(|p| p.name.clone())
        .collect();
    let mut actions = row![
        pick_list(names, None::<String>, {
            let key = key.clone();
            move |p| Message::CharacterProfile(key.clone(), p)
        })
        .placeholder("Load only a profile…")
        .text_size(theme::TEXT_BODY)
        .padding(Padding::from([theme::XS, theme::SM]))
        .style(theme::select(t)),
    ]
    .spacing(theme::SM)
    .align_y(Alignment::Center);
    if !others.is_empty() {
        let from: Vec<CharacterChoice> = others
            .iter()
            .map(|c| CharacterChoice {
                key: c.character.key(),
                name: c.character.name.clone(),
            })
            .collect();
        actions = actions.push(
            pick_list(from, None::<CharacterChoice>, {
                let key = key.clone();
                move |c| Message::CharacterCopy(c.key, key.clone())
            })
            .placeholder("Copy setup from…")
            .text_size(theme::TEXT_BODY)
            .padding(Padding::from([theme::XS, theme::SM]))
            .style(theme::select(t)),
        );
    }
    actions = actions
        .push(Space::new().width(Length::Fill))
        .push(action_i(
            Some(Icon::Power),
            "All on",
            Some(set(&every, true)),
            t,
        ))
        .push(action_i(
            Some(Icon::PowerOff),
            "All off",
            Some(set(&every, false)),
            t,
        ));
    let mut facts = vec![
        format!("realm {}", current.character.realm),
        played(current.last_played),
        format!("loads {loads} of {}", rows.len()),
    ];
    if let Some(region) = &app.loaded.region {
        facts.insert(1, region.clone());
    }
    let mut header = column![
        title(current.character.name.as_str(), t),
        meta(facts.join(" · "), t),
        actions,
    ]
    .spacing(theme::SM);
    if app.game_running {
        header = header
            .push(meta("WoW is running; changes wait until you close it.", t).color(t.warning));
    }

    // Addons, filtered.
    let filter = app.character_filter;
    let mut chips = row![
        chip(
            "All",
            filter == CharacterFilter::All,
            Message::CharacterFilter(CharacterFilter::All),
            t
        ),
        chip(
            "On",
            filter == CharacterFilter::On,
            Message::CharacterFilter(CharacterFilter::On),
            t
        ),
        chip(
            "Off",
            filter == CharacterFilter::Off,
            Message::CharacterFilter(CharacterFilter::Off),
            t
        ),
    ]
    .spacing(theme::XS)
    .align_y(Alignment::Center);
    if !others.is_empty() {
        chips = chips.push(chip(
            "Differs from other characters",
            filter == CharacterFilter::Differs,
            Message::CharacterFilter(CharacterFilter::Differs),
            t,
        ));
    }
    let mut list = column![
        chips,
        meta(
            "Addons a character hasn't seen load by default; this is the same switch as the AddOns button at character select.",
            t
        ),
    ]
    .spacing(theme::SM);
    let mut shown = 0;
    for (name, folders, source) in rows {
        let is_on = on_for(current, &folders);
        let differ: Vec<&str> = others
            .iter()
            .filter(|c| on_for(c, &folders) != is_on)
            .map(|c| c.character.name.as_str())
            .collect();
        let keep = match filter {
            CharacterFilter::All => true,
            CharacterFilter::On => is_on,
            CharacterFilter::Off => !is_on,
            CharacterFilter::Differs => !differ.is_empty(),
        };
        if !keep {
            continue;
        }
        shown += 1;
        let mut r = row![
            checkbox(is_on)
                .label(name)
                .text_size(theme::TEXT_BODY)
                .style(theme::check(t))
                .on_toggle({
                    let msg = set(&folders, !is_on);
                    move |_| msg.clone()
                })
                .width(Length::Fill),
        ]
        .spacing(theme::MD)
        .align_y(Alignment::Center);
        if !differ.is_empty() {
            let state = if is_on { "off" } else { "on" };
            r = r.push(single_line(meta(
                format!("{state} for {}", differ.join(", ")),
                t,
            )));
        }
        if folders
            .iter()
            .any(|f| current.saved.contains(&f.to_lowercase()))
        {
            r = r.push(tooltip(
                pill("has settings", t.accent),
                container(meta("Keeps settings just for this character", t))
                    .padding(theme::XS)
                    .style(theme::panel(t)),
                tooltip::Position::Top,
            ));
        }
        list = list.push(r.push(source_pill(source, t)));
    }
    if shown == 0 {
        list = list.push(meta("Nothing matches this filter.", t));
    }

    // This character's settings in the snapshots.
    let mut saved = column![meta(
        "Put back just this character's addon settings from a snapshot; everything else stays as it is, and the current settings are backed up first.",
        t
    )]
    .spacing(theme::SM);
    if current.snapshots.is_empty() {
        saved = saved.push(meta("No snapshot has this character's settings yet.", t));
    }
    for s in &current.snapshots {
        saved = saved.push(
            row![
                mono(rfc3339(s.made).replace('T', " ").replace('Z', ""), t),
                single_line(body(s.reason.as_str(), t)).width(Length::Fill),
                meta(format!("{} files", s.files), t),
                action(
                    "Restore",
                    Some(Message::RestoreWtf(s.name.clone(), Some(key.clone()))),
                    t
                ),
            ]
            .spacing(theme::MD)
            .align_y(Alignment::Center),
        );
    }

    let detail = column![
        container(header)
            .padding(theme::LG)
            .width(Length::Fill)
            .style(theme::panel(t)),
        section("Addons", list.into(), t),
        section("Saved settings", saved.into(), t),
    ]
    .spacing(theme::MD)
    .width(Length::Fill);
    (
        toolbar.into(),
        row![picker, detail]
            .spacing(theme::MD)
            .padding(Padding::from([theme::SM, 0.0]))
            .into(),
    )
}

/// "played today", "played 3 days ago", or never.
fn played(at: Option<u64>) -> String {
    let Some(at) = at else {
        return "never logged out".to_owned();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    match now.saturating_sub(at) / 86_400 {
        0 => "played today".to_owned(),
        1 => "played yesterday".to_owned(),
        d if d < 60 => format!("played {d} days ago"),
        _ => {
            let date = rfc3339(at);
            format!("played {}", date.get(..10).unwrap_or(&date))
        }
    }
}

/// A character in a pick list.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CharacterChoice {
    key: String,
    name: String,
}

impl fmt::Display for CharacterChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

// ---- History ------------------------------------------------------------------------------

const HISTORY_COLS: [Spec; 3] = [
    spec(Col::When, "When", true),
    spec(Col::Kind, "Kind", true),
    spec(Col::Change, "Change", true),
];

fn history(app: &App, t: Tokens) -> (El<'_>, El<'_>) {
    let id = TableId::History;
    let w = widths(app, id, &HISTORY_COLS);
    let when = |e: &Event| rfc3339(e.at).replace('T', " ").replace('Z', "");
    let mut events: Vec<&Event> = app.loaded.history.iter().collect();
    app.tables.get(id).apply(&mut events, |e, col| match col {
        Col::When => Key::text(&when(e)),
        Col::Kind => Key::text(e.kind.label()),
        _ => Key::text(&e.what),
    });
    let toolbar = row![
        strong(format!("{} changes", app.loaded.history.len()), t),
        meta(
            "Undo the newest changes first; undoing an old one can clash with what came after.",
            t
        )
        .width(Length::Fill),
    ]
    .spacing(theme::MD)
    .align_y(Alignment::Center);
    let mut list = column![].spacing(theme::XS);
    if events.is_empty() {
        let empty = if app.loaded.history.is_empty() {
            "Nothing yet."
        } else {
            "Nothing matches the filters."
        };
        list = list.push(container(meta(empty, t)).padding(theme::XL));
    }
    for e in events.into_iter().take(HISTORY_SHOWN) {
        let undo: El<'_> = match &e.undo {
            Some(u) => action_i(
                Some(Icon::Undo),
                "Undo",
                Some(Message::UndoEvent(u.clone())),
                t,
            ),
            None => Space::new().into(),
        };
        let cells = table::cells(
            vec![
                single_line(mono(when(e), t)).into(),
                pill(e.kind.label(), kind_color(e.kind, t)),
                single_line(body(e.what.as_str(), t)).into(),
            ],
            &w,
            undo,
            theme::ACTIONS_NARROW,
            t.row,
        );
        list = list.push(container(cells).padding(Padding::from([0.0, theme::MD])));
    }
    (
        with_header(
            app,
            toolbar,
            id,
            &HISTORY_COLS,
            &w,
            theme::ACTIONS_NARROW,
            t,
        ),
        list.into(),
    )
}

/// Adding is green, taking away red, going back amber, switching blue.
fn kind_color(kind: Kind, t: Tokens) -> Color {
    match kind {
        Kind::Install | Kind::Enable => t.success,
        Kind::Remove | Kind::Disable => t.danger,
        Kind::Rollback | Kind::Restore => t.warning,
        Kind::Update | Kind::Switch => t.accent,
        Kind::Characters => t.violet,
        Kind::Other => t.text_muted,
    }
}

const HISTORY_SHOWN: usize = 200;

// ---- Health -------------------------------------------------------------------------------

fn health(app: &App, t: Tokens) -> (El<'_>, El<'_>) {
    let issues = &app.loaded.issues;
    let summary = match issues.len() {
        0 => "No problems found.".to_owned(),
        1 => "1 problem".to_owned(),
        n => format!("{n} problems"),
    };
    let mut toolbar = row![strong(summary, t).width(Length::Fill)]
        .spacing(theme::SM)
        .align_y(Alignment::Center);
    if let Some(c) = app.client() {
        toolbar = toolbar.push(action_i(
            Some(Icon::FolderOpen),
            "Open AddOns folder",
            Some(Message::OpenPath(c.addons.clone())),
            t,
        ));
    }

    let mut list = column![].spacing(theme::SM);
    for issue in issues {
        list = list.push(issue_row(app, issue, t));
    }
    if issues.is_empty() {
        list = list.push(meta(
            "Every folder has a TOC for this client, none are duplicated, and every required dependency is installed.",
            t,
        ));
    }
    let mut cards = column![section("Problems", list.into(), t)].spacing(theme::LG);

    if !app.loaded.disabled.is_empty() {
        let mut parked = column![meta(
            "Moved out of AddOns, so WoW doesn't load them. Nothing is deleted.",
            t
        )]
        .spacing(theme::SM);
        for f in &app.loaded.disabled {
            parked = parked.push(
                row![
                    body(f.as_str(), t).width(Length::Fill),
                    action("Enable", Some(Message::Enable(f.clone())), t),
                ]
                .align_y(Alignment::Center),
            );
        }
        cards = cards.push(section("Disabled", parked.into(), t));
    }

    if !app.loaded.unused.is_empty() {
        let all = Message::Many(
            app.loaded
                .unused
                .iter()
                .map(|f| Message::Disable(f.clone()))
                .collect(),
        );
        let mut libs = column![
            row![
                meta(
                    "Libraries no installed addon lists as a dependency, probably left behind by an addon that's gone. Disabling moves them aside; nothing is deleted.",
                    t
                )
                .width(Length::Fill),
                action_i(Some(Icon::BrushCleaning), "Disable all", Some(all), t),
            ]
            .spacing(theme::MD)
            .align_y(Alignment::Center)
        ]
        .spacing(theme::SM);
        for f in &app.loaded.unused {
            libs = libs.push(
                row![
                    body(f.as_str(), t).width(Length::Fill),
                    action_i(
                        Some(Icon::PowerOff),
                        "Disable",
                        Some(Message::Disable(f.clone())),
                        t
                    ),
                ]
                .align_y(Alignment::Center),
            );
        }
        cards = cards.push(section("Probably unused libraries", libs.into(), t));
    }

    let mut saved = column![
        row![
            meta(
                "Addon settings (SavedVariables) are backed up before updates, removals and profile switches. Rolling back an addon restores its settings too.",
                t
            )
            .width(Length::Fill),
            action("Back up now", Some(Message::BackupWtf), t),
        ]
        .spacing(theme::MD)
        .align_y(Alignment::Center)
    ]
    .spacing(theme::SM);
    if app.loaded.snapshots.is_empty() {
        saved = saved.push(meta("No backups yet.", t));
    }
    for s in &app.loaded.snapshots {
        saved = saved.push(
            row![
                mono(rfc3339(s.made).replace('T', " ").replace('Z', ""), t),
                single_line(body(s.reason.as_str(), t)).width(Length::Fill),
                meta(format!("{} files · {}", s.files, human_bytes(s.bytes)), t),
                action(
                    "Restore",
                    Some(Message::RestoreWtf(s.name.clone(), None)),
                    t
                ),
            ]
            .spacing(theme::MD)
            .align_y(Alignment::Center),
        );
    }
    cards = cards.push(section("Saved settings", saved.into(), t));

    (
        toolbar.into(),
        cards.padding(Padding::from([theme::SM, 0.0])).into(),
    )
}

fn issue_row<'a>(app: &'a App, issue: &'a Issue, t: Tokens) -> El<'a> {
    let (what, detail, color) = match issue {
        Issue::OutOfDate { folder } => (
            folder.clone(),
            "Its TOC is older than the game, so WoW skips it unless \"Load out of date AddOns\" is on.".to_owned(),
            t.warning,
        ),
        Issue::NoToc { folder, nested: Some(inner) } => (
            folder.clone(),
            format!("Extracted one level too deep: move {folder}/{inner} into AddOns."),
            t.danger,
        ),
        Issue::NoToc { folder, nested: None } => (
            folder.clone(),
            "Has no TOC file this client loads.".to_owned(),
            t.danger,
        ),
        Issue::Duplicate { title, folders } => (
            title.clone(),
            format!("Installed twice: {}. Keep one.", folders.join(", ")),
            t.warning,
        ),
        Issue::MissingDep { dep, needed_by } => (
            dep.clone(),
            format!("Required by {} but not installed.", needed_by.join(", ")),
            t.danger,
        ),
    };
    let mut actions = row![].spacing(theme::SM).align_y(Alignment::Center);
    match issue {
        Issue::MissingDep { dep, .. } => {
            actions = actions.push(primary_i(
                Some(Icon::Search),
                "Find",
                Some(Message::FindAddon(dep.clone())),
                t,
            ));
        }
        Issue::NoToc {
            folder,
            nested: Some(_),
        } => {
            if let Some(c) = app.client() {
                actions = actions.push(action_i(
                    Some(Icon::FolderOpen),
                    "Open folder",
                    Some(Message::OpenPath(c.addons.join(folder))),
                    t,
                ));
            }
        }
        _ => {
            for f in issue.folders() {
                actions = actions.push(action(
                    format!("Disable {f}"),
                    Some(Message::Disable(f.to_owned())),
                    t,
                ));
            }
        }
    }
    row![
        column![
            row![pill(issue_kind(issue), color), strong(what, t)]
                .spacing(theme::SM)
                .align_y(Alignment::Center),
            meta(detail, t),
        ]
        .spacing(theme::XS)
        .width(Length::Fill),
        actions.wrap(),
    ]
    .spacing(theme::MD)
    .align_y(Alignment::Center)
    .into()
}

fn issue_kind(issue: &Issue) -> &'static str {
    match issue {
        Issue::OutOfDate { .. } => "out of date",
        Issue::NoToc { .. } => "won't load",
        Issue::Duplicate { .. } => "duplicate",
        Issue::MissingDep { .. } => "missing dependency",
    }
}

// ---- Settings -----------------------------------------------------------------------------

fn settings(app: &App, t: Tokens) -> El<'_> {
    let mut clients = column![].spacing(theme::SM);
    for (i, cfg) in app.config.clients.iter().enumerate() {
        let name = app.rename.get(&i).map_or(cfg.name.as_str(), String::as_str);
        clients = clients.push(
            row![
                text_input("Name", name)
                    .on_input(move |s| Message::RenameInput(i, s))
                    .on_submit(Message::RenameSave(i))
                    .size(theme::TEXT_BODY)
                    .padding(theme::XS)
                    .width(theme::SIDEBAR_WIDTH)
                    .style(theme::input(t)),
                meta(cfg.flavor.label().to_owned(), t),
                single_line(mono(cfg.path.display().to_string(), t)).width(Length::Fill),
                checkbox(cfg.enabled)
                    .label("Enabled")
                    .text_size(theme::TEXT_META)
                    .style(theme::check(t))
                    .on_toggle(move |_| Message::ToggleClient(i)),
                danger("Remove", Message::RemoveClient(i), t),
            ]
            .spacing(theme::MD)
            .align_y(Alignment::Center),
        );
    }
    if app.config.clients.is_empty() {
        clients = clients.push(meta("No clients configured yet.", t));
    }
    clients = clients.push(action("Add folder…", Some(Message::AddFolder), t));

    let src = &app.config.sources;
    let sources = column![
        checkbox(src.github)
            .label("GitHub")
            .text_size(theme::TEXT_BODY)
            .style(theme::check(t))
            .on_toggle(|_| Message::ToggleSource(SourceId::Github)),
        meta(
            "Anonymous requests are limited to 60 per hour; a token raises that.",
            t
        ),
        secret_row(app, Secret::GithubToken, "GitHub token", t),
        checkbox(src.wowinterface)
            .label("WoWInterface")
            .text_size(theme::TEXT_BODY)
            .style(theme::check(t))
            .on_toggle(|_| Message::ToggleSource(SourceId::Wowi)),
        checkbox(src.curseforge)
            .label("CurseForge")
            .text_size(theme::TEXT_BODY)
            .style(theme::check(t))
            .on_toggle(|_| Message::ToggleSource(SourceId::Curseforge)),
        meta(
            "Needs your own API key (CurseForge's terms don't allow shared keys).",
            t
        ),
        secret_row(app, Secret::CurseforgeKey, "CurseForge API key", t),
        link(
            "Apply for a key at console.curseforge.com",
            Message::OpenUrl("https://console.curseforge.com/".into()),
            t
        ),
    ]
    .spacing(theme::SM);

    let general = column![
        row![
            body("Concurrent downloads", t).width(Length::Fill),
            pick_list(
                (1..=8).collect::<Vec<usize>>(),
                Some(app.config.concurrency),
                Message::Concurrency
            )
            .text_size(theme::TEXT_BODY)
            .padding(Padding::from([theme::XS, theme::SM]))
            .style(theme::select(t)),
        ]
        .align_y(Alignment::Center),
        row![
            body("Theme", t).width(Length::Fill),
            pick_list(
                [Theme::Dark, Theme::Light, Theme::System].map(ThemeChoice),
                Some(ThemeChoice(app.config.theme)),
                |c| Message::SetTheme(c.0)
            )
            .text_size(theme::TEXT_BODY)
            .padding(Padding::from([theme::XS, theme::SM]))
            .style(theme::select(t)),
        ]
        .align_y(Alignment::Center),
        row![
            body("Row density", t).width(Length::Fill),
            chip(
                "Comfortable",
                app.config.density == Density::Comfortable,
                Message::SetDensity(Density::Comfortable),
                t
            ),
            chip(
                "Compact",
                app.config.density == Density::Compact,
                Message::SetDensity(Density::Compact),
                t
            ),
        ]
        .spacing(theme::XS)
        .align_y(Alignment::Center),
        row![
            body("Check for updates in the background and notify me", t).width(Length::Fill),
            checkbox(app.config.notify)
                .on_toggle(Message::SetNotify)
                .style(theme::check(t)),
        ]
        .align_y(Alignment::Center),
        theme::UI_SCALES
            .iter()
            .fold(
                row![body("Zoom", t).width(Length::Fill)].spacing(theme::XS),
                |r, &f| r.push(chip(
                    format!("{:.0}%", f * 100.0),
                    (app.scale() - f).abs() < 0.01,
                    Message::SetScale(f),
                    t
                ))
            )
            .align_y(Alignment::Center),
    ]
    .spacing(theme::SM);

    let size = app
        .cache_size
        .map_or_else(|| "measuring…".to_owned(), human_bytes);
    let storage = column![
        path_row("Config", app.paths.config_file.clone(), t),
        path_row("Lockfiles", app.paths.data_dir.clone(), t),
        path_row("Cache", app.paths.cache_dir.clone(), t),
        row![
            body(format!("Cache size: {size}"), t).width(Length::Fill),
            action("Clear downloads", Some(Message::ClearDownloads), t),
        ]
        .align_y(Alignment::Center),
        meta("Backups used for rollback are kept.", t),
    ]
    .spacing(theme::SM);

    column![
        section("Clients", clients.into(), t),
        section("Sources", sources.into(), t),
        section("General", general.into(), t),
        section("Storage", storage.into(), t),
    ]
    .spacing(theme::LG)
    .into()
}

/// A secret field: shows only whether it's set; saving stores it in the OS keychain.
fn secret_row<'a>(app: &'a App, secret: Secret, label: &'a str, t: Tokens) -> El<'a> {
    let Some(f) = app.secrets.field(secret) else {
        return Space::new().into();
    };
    if f.from_env {
        return meta(format!("{label}: set via {}", secret.env()), t).into();
    }
    let status = if f.is_set {
        "saved in keychain"
    } else {
        "not set"
    };
    row![
        meta(format!("{label} ({status})"), t).width(theme::SIDEBAR_WIDTH),
        text_input(
            if f.is_set {
                "Enter a new value to replace it"
            } else {
                "Paste here"
            },
            &f.input
        )
        .secure(true)
        .on_input(move |v| Message::SecretInput(secret, v))
        .on_submit(Message::SaveSecret(secret))
        .size(theme::TEXT_BODY)
        .padding(theme::XS)
        .width(Length::Fill)
        .style(theme::input(t)),
        action("Save", Some(Message::SaveSecret(secret)), t),
        action("Clear", f.is_set.then_some(Message::ClearSecret(secret)), t),
    ]
    .spacing(theme::SM)
    .align_y(Alignment::Center)
    .into()
}

fn path_row<'a>(label: &'a str, path: std::path::PathBuf, t: Tokens) -> El<'a> {
    row![
        meta(label, t).width(theme::LABEL_WIDTH),
        single_line(mono(path.display().to_string(), t)).width(Length::Fill),
        link("Open", Message::OpenPath(path), t),
    ]
    .spacing(theme::MD)
    .align_y(Alignment::Center)
    .into()
}

fn human_bytes(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.1} KB", n as f64 / 1024.0),
        1_048_576..1_073_741_824 => format!("{:.1} MB", n as f64 / 1_048_576.0),
        _ => format!("{:.1} GB", n as f64 / 1_073_741_824.0),
    }
}

// ---- First run ----------------------------------------------------------------------------

fn first_run(app: &App, t: Tokens) -> El<'_> {
    let mut list = column![].spacing(theme::SM);
    if app.candidates.is_empty() {
        list = list.push(meta(
            "No WoW install was found automatically. Add its folder (the install, the _forever_ folder or its Interface/AddOns).",
            t,
        ));
    }
    for (i, (c, on)) in app.candidates.iter().enumerate() {
        list = list.push(
            row![
                checkbox(*on)
                    .style(theme::check(t))
                    .on_toggle(move |_| Message::ToggleCandidate(i)),
                column![
                    strong(
                        format!("{} {}", c.name, c.version.as_deref().unwrap_or_default()),
                        t
                    ),
                    single_line(mono(c.addons.display().to_string(), t)),
                ]
                .spacing(theme::XS),
            ]
            .spacing(theme::MD)
            .align_y(Alignment::Center),
        );
    }
    let any = app.candidates.iter().any(|(_, on)| *on);
    let card = container(
        column![
            title("Welcome to kodo", t),
            meta(
                "Pick the game clients kodo should manage. It will then match the addons you already have.",
                t
            ),
            divider(t),
            list,
            row![
                action("Add folder…", Some(Message::AddFolder), t),
                Space::new().width(Length::Fill),
                primary("Continue", any.then_some(Message::FinishFirstRun), t),
            ]
            .align_y(Alignment::Center),
        ]
        .spacing(theme::LG),
    )
    .padding(theme::XL)
    .max_width(theme::CARD_MAX_WIDTH)
    .style(theme::panel(t));
    container(card)
        .center(Length::Fill)
        .padding(theme::XXL)
        .into()
}

#[cfg(test)]
mod tests {
    use kodo_core::addon::{AddonRef, Channel, Release};
    use kodo_core::config::{Config, Paths};
    use kodo_core::detect::Client;
    use kodo_core::engine::UpdateCheck;
    use kodo_core::flavor::Flavor;
    use kodo_core::lockfile::LockEntry;

    use crate::app::{App, Screen};
    use crate::view::{human_bytes, human_count, snippet, view};

    fn sample() -> App {
        let tmp = std::env::temp_dir().join("kodo-app-view-test");
        let (mut app, _) = App::new(Paths::under(&tmp), Config::default());
        let client = Client {
            name: "Forever".into(),
            flavor: Flavor::Forever,
            root: tmp.join("wow"),
            addons: tmp.join("wow/_forever_/Interface/AddOns"),
            product: Some("wowf".into()),
            version: Some("1.60.1.70938".into()),
            interface: Some(16001),
        };
        let id: AddonRef = "github:Cidan/BetterBags".parse().unwrap();
        let entry = LockEntry::new(id.clone(), "v0.9.1", vec!["BetterBags".into()]);
        app.loaded.present = vec!["BetterBags".into()];
        app.loaded.entries = vec![entry.clone()];
        app.loaded.backups.insert(id.clone(), vec!["v0.9.0".into()]);
        app.checks.insert(
            id.clone(),
            UpdateCheck {
                entry,
                latest: Some(Release {
                    addon: id.clone(),
                    name: "BetterBags".into(),
                    version: "v0.9.2".into(),
                    channel: Channel::Stable,
                    download_url: Some("https://example.invalid/x.zip".into()),
                    page_url: None,
                    size: None,
                    changelog: Some("Fixed things.\n\nAnd more things.".into()),
                    published_at: None,
                }),
                error: None,
            },
        );
        app.clients = vec![client.clone()];
        app.candidates = vec![(client, true)];
        app.detail = Some(id.clone());
        app.menu = Some(id);
        app.search = "bags".into();
        app.error_open = true;
        app.last_error = Some(crate::app::JobError {
            message: "boom".into(),
            retry: None,
            page: Some("https://example.invalid".into()),
        });
        app
    }

    #[test]
    fn every_screen_builds() {
        let mut app = sample();
        assert_eq!(app.update_count(), 1);
        for screen in [
            Screen::FirstRun,
            Screen::Installed,
            Screen::Browse,
            Screen::Updates,
            Screen::Profiles,
            Screen::Health,
            Screen::Settings,
        ] {
            app.screen = screen;
            let _ = view(&app);
        }
        app.config.theme = kodo_core::config::Theme::Light;
        let _ = view(&app);

        // Health problems, disabled folders, a queued change, release notes and a QR code.
        use kodo_core::health::Issue;
        app.loaded.issues = vec![
            Issue::OutOfDate {
                folder: "Old".into(),
            },
            Issue::NoToc {
                folder: "Foo-main".into(),
                nested: Some("Foo".into()),
            },
            Issue::NoToc {
                folder: "Empty".into(),
                nested: None,
            },
            Issue::Duplicate {
                title: "Bags".into(),
                folders: vec!["Bags".into(), "Bags2".into()],
            },
            Issue::MissingDep {
                dep: "LibDD".into(),
                needed_by: vec!["Bags".into()],
            },
        ];
        app.loaded.disabled = vec!["BetterBags".into()];
        app.game_running = true;
        app.queued.push(crate::app::Message::UpdateAll);
        let id: AddonRef = "github:Cidan/BetterBags".parse().unwrap();
        app.expanded = Some(id.clone());
        app.changes.insert(
            id,
            Ok(vec![crate::app::Notes::from(kodo_core::addon::Change {
                version: "v0.9.2".into(),
                published_at: Some("2026-11-07T00:00:00Z".into()),
                notes: Some("* fixed [things](https://x)\n\n`code`".into()),
            })]),
        );
        app.qr = Some((
            "Raid".into(),
            iced::widget::qr_code::Data::new("kodo1-abc").ok(),
        ));
        // Toasts, the shortcuts overlay, ticked rows, the cursor and a switch preview.
        app.toast("removed X", Some(("Undo", crate::app::Message::UpdateAll)));
        app.picked
            .insert("github:Cidan/BetterBags".parse().unwrap());
        app.cursor = Some(0);
        app.show_keys = true;
        app.switch_plan = Some(Ok(kodo_core::engine::SwitchPlan {
            profile: "Raid".into(),
            stash: vec!["BetterBags".into()],
            restore: Vec::new(),
            download: vec!["DBM".into()],
        }));
        // Characters, history, unused libraries, stale hints, hover, progress and the digest.
        let character = |name: &str, on: bool| kodo_core::engine::CharacterAddons {
            character: kodo_core::characters::Character {
                account: "1#1".into(),
                realm: "91".into(),
                name: name.into(),
            },
            states: [("BetterBags".to_owned(), on)].into_iter().collect(),
            last_played: Some(1),
            saved: ["betterbags".to_owned()].into(),
            snapshots: vec![kodo_core::engine::CharacterSnapshot {
                name: "wtf-1.zip".into(),
                made: 1,
                reason: "before updating BetterBags".into(),
                files: 2,
            }],
        };
        app.loaded.characters = vec![character("Me-Realm", false), character("Alt-Realm", true)];
        app.loaded.region = Some("US".into());
        app.character_filter = crate::app::CharacterFilter::Differs;
        app.loaded.history = vec![kodo_core::history::Event::now(
            kodo_core::history::Kind::Remove,
            "removed X",
            Some(kodo_core::history::Undo::Enable { name: "X".into() }),
        )];
        app.loaded.unused = vec!["LibOld".into()];
        app.loaded.last_loaded = [("betterbags".to_owned(), 1), ("other".to_owned(), 1 << 30)]
            .into_iter()
            .collect();
        app.hovered = Some("github:Cidan/BetterBags".parse().unwrap());
        app.config.density = kodo_core::config::Density::Compact;
        app.import_matches = Some(vec![kodo_core::engine::ImportMatch {
            wanted: kodo_core::import::Wanted {
                name: "details".into(),
                addon: None,
            },
            addon: Some("wowi:1".parse().unwrap()),
            name: "Details!".into(),
            exact: false,
            installed: false,
        }]);
        assert_eq!(app.digest().len(), 1, "an unseen update is new");
        assert!(super::stale_hint(&app, &["BetterBags".into()]).is_some());
        for screen in [
            Screen::Installed,
            Screen::Updates,
            Screen::Profiles,
            Screen::Characters,
            Screen::Health,
            Screen::History,
        ] {
            app.screen = screen;
            let _ = view(&app);
        }

        app.screen = Screen::Browse;
        app.results = vec![kodo_core::addon::RemoteAddon {
            addon: "github:Cidan/Other".parse().unwrap(),
            name: "Other".into(),
            author: None,
            summary: Some("x".into()),
            downloads: Some(5),
            page_url: None,
            version: None,
            updated: None,
            tracks: vec!["retail".into()],
        }];
        app.browse_cards = true;
        let _ = view(&app);
        app.builds_only = true;
        assert!(
            super::browse_hits(&app).is_empty(),
            "retail-only hidden on Forever"
        );
        app.import_code = "kodo1-junk".into();
        app.page = Some(crate::app::AddonPage {
            hit: kodo_core::addon::RemoteAddon {
                addon: "github:Cidan/BetterBags".parse().unwrap(),
                name: "BetterBags".into(),
                author: Some("Cidan".into()),
                summary: None,
                downloads: Some(220_340),
                page_url: None,
                version: None,
                updated: Some("2026-11-07".into()),
                tracks: vec!["forever".into()],
            },
            release: Some(Err("no build".into())),
            notes: Vec::new(),
            readme: Some(Ok(Some(
                iced::widget::markdown::parse("# Hi\n- [a](b)\n```\ncode\n```").collect(),
            ))),
        });
        let _ = view(&app);
        app.screen = Screen::Profiles;
        let _ = view(&app);
    }

    #[test]
    fn formatting_helpers() {
        assert_eq!(human_count(950), "950");
        assert_eq!(human_count(4_728_176), "4.7M");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(snippet("a\n\n b"), "a b");
        assert!(snippet(&"x ".repeat(500)).ends_with('…'));
    }
}
