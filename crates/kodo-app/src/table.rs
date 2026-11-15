//! Data tables: a header (sortable, resizable columns) and an optional filter row that the
//! screen keeps outside its scrollable so they stay put, over rows laid out with the same
//! widths. Widths persist in config as `table.column`.

use std::collections::{BTreeMap, HashMap};

use iced::widget::{Row, Space, button, checkbox, container, mouse_area, row, text, text_input};
use iced::{Alignment, Element, Length, Padding, mouse};

use crate::app::Message;
use crate::theme::{self, Icon, Tokens};

type El<'a> = Element<'a, Message>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TableId {
    Installed,
    Updates,
    Browse,
    History,
}

impl TableId {
    const ALL: [Self; 4] = [Self::Installed, Self::Updates, Self::Browse, Self::History];

    fn key(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Updates => "updates",
            Self::Browse => "browse",
            Self::History => "history",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Col {
    /// Row checkboxes for bulk actions; fixed width, not sortable.
    Select,
    Name,
    Installed,
    Latest,
    Source,
    Status,
    Author,
    Downloads,
    Updated,
    /// When a history event happened.
    When,
    /// A history event's kind.
    Kind,
    /// What a history event did.
    Change,
}

impl Col {
    const ALL: [Self; 12] = [
        Self::Select,
        Self::Name,
        Self::Installed,
        Self::Latest,
        Self::Source,
        Self::Status,
        Self::Author,
        Self::Downloads,
        Self::Updated,
        Self::When,
        Self::Kind,
        Self::Change,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Name => "name",
            Self::Installed => "installed",
            Self::Latest => "latest",
            Self::Source => "source",
            Self::Status => "status",
            Self::Author => "author",
            Self::Downloads => "downloads",
            Self::Updated => "updated",
            Self::When => "when",
            Self::Kind => "kind",
            Self::Change => "change",
        }
    }

    fn default_width(self) -> f32 {
        match self {
            Self::Select => theme::COL_CHECK,
            Self::Name => theme::COL_NAME,
            Self::Installed | Self::Latest => theme::COL_VERSION,
            Self::Source => theme::COL_SOURCE,
            Self::Status => theme::COL_STATUS,
            Self::Author => theme::COL_AUTHOR,
            Self::Downloads => theme::COL_COUNT,
            Self::Updated => theme::COL_DATE,
            Self::When => theme::COL_DATETIME,
            Self::Kind => theme::COL_SOURCE,
            Self::Change => theme::COL_CHANGE,
        }
    }

    /// Counts and dates read best biggest/newest first.
    fn descending_first(self) -> bool {
        matches!(self, Self::Downloads | Self::Updated | Self::When)
    }
}

/// Column width from config, else the default.
pub fn width(columns: &BTreeMap<String, f32>, id: TableId, col: Col) -> f32 {
    if col == Col::Select {
        return theme::COL_CHECK;
    }
    columns
        .get(&width_key(id, col))
        .copied()
        .unwrap_or_else(|| col.default_width())
}

pub fn width_key(id: TableId, col: Col) -> String {
    format!("{}.{}", id.key(), col.key())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Asc,
    Desc,
}

/// A column drag in progress. `anchor` is the cursor x at the first move after the press.
#[derive(Debug, Clone, Copy)]
pub struct Resize {
    pub table: TableId,
    pub col: Col,
    pub start: f32,
    pub anchor: Option<f32>,
}

impl Resize {
    /// New width for cursor x, or `None` while the anchor is being set.
    pub fn moved(&mut self, x: f32) -> Option<f32> {
        match self.anchor {
            None => {
                self.anchor = Some(x);
                None
            }
            Some(a) => Some((self.start + x - a).max(theme::COL_MIN)),
        }
    }
}

/// Sort and per-column filter text for one table.
#[derive(Debug, Clone, Default)]
pub struct View {
    pub sort: Option<(Col, Dir)>,
    pub filters: HashMap<Col, String>,
}

impl View {
    /// Header click: first direction, the other direction, then unsorted.
    pub fn toggle(&mut self, col: Col) {
        let first = if col.descending_first() {
            Dir::Desc
        } else {
            Dir::Asc
        };
        self.sort = match self.sort {
            Some((c, d)) if c == col && d == first => Some((
                col,
                match first {
                    Dir::Asc => Dir::Desc,
                    Dir::Desc => Dir::Asc,
                },
            )),
            Some((c, _)) if c == col => None,
            _ => Some((col, first)),
        };
    }

    /// The sort as config stores it: `column:asc|desc`.
    fn sort_key(&self) -> Option<String> {
        self.sort.map(|(c, d)| {
            let dir = if d == Dir::Asc { "asc" } else { "desc" };
            format!("{}:{dir}", c.key())
        })
    }

    fn set_sort_key(&mut self, s: &str) {
        let (col, dir) = s.split_once(':').unwrap_or((s, "asc"));
        self.sort = Col::ALL
            .into_iter()
            .find(|c| c.key() == col && *c != Col::Select)
            .map(|c| (c, if dir == "desc" { Dir::Desc } else { Dir::Asc }));
    }

    pub fn filter(&self, col: Col) -> &str {
        self.filters.get(&col).map_or("", String::as_str)
    }

    /// Keep rows matching every column filter, then sort.
    pub fn apply<T>(&self, rows: &mut Vec<T>, key: impl Fn(&T, Col) -> Key) {
        for (col, f) in &self.filters {
            let f = f.trim().to_lowercase();
            if !f.is_empty() {
                rows.retain(|r| key(r, *col).as_text().contains(&f));
            }
        }
        if let Some((col, dir)) = self.sort {
            rows.sort_by_cached_key(|r| key(r, col));
            if dir == Dir::Desc {
                rows.reverse();
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct Tables {
    pub installed: View,
    pub updates: View,
    pub browse: View,
    pub history: View,
}

impl Tables {
    pub fn get(&self, id: TableId) -> &View {
        match id {
            TableId::Installed => &self.installed,
            TableId::Updates => &self.updates,
            TableId::Browse => &self.browse,
            TableId::History => &self.history,
        }
    }

    /// Tables sorted as config remembers them.
    pub fn with_sorts(sorts: &BTreeMap<String, String>) -> Self {
        let mut t = Self::default();
        for id in TableId::ALL {
            if let Some(s) = sorts.get(id.key()) {
                t.get_mut(id).set_sort_key(s);
            }
        }
        t
    }

    /// Every table's sort, for config.
    pub fn sorts(&self) -> BTreeMap<String, String> {
        TableId::ALL
            .into_iter()
            .filter_map(|id| Some((id.key().to_owned(), self.get(id).sort_key()?)))
            .collect()
    }

    pub fn get_mut(&mut self, id: TableId) -> &mut View {
        match id {
            TableId::Installed => &mut self.installed,
            TableId::Updates => &mut self.updates,
            TableId::Browse => &mut self.browse,
            TableId::History => &mut self.history,
        }
    }
}

/// A cell's value for sorting and filtering.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    Num(u64),
    Text(String),
}

impl Key {
    pub fn text(s: &str) -> Self {
        Self::Text(s.to_lowercase())
    }

    fn as_text(&self) -> String {
        match self {
            Self::Num(n) => n.to_string(),
            Self::Text(s) => s.clone(),
        }
    }
}

/// One table's columns: what to show and whether it gets a filter box.
pub struct Spec {
    pub col: Col,
    pub label: &'static str,
    pub filter: bool,
}

pub const fn spec(col: Col, label: &'static str, filter: bool) -> Spec {
    Spec { col, label, filter }
}

/// Header row: click a label to sort, drag the divider after it to resize. `select` is the
/// select-all checkbox state and its message, for tables with a [`Col::Select`] column.
pub fn header<'a>(
    id: TableId,
    specs: &[Spec],
    widths: &[f32],
    view: &View,
    trailing: f32,
    select: Option<(bool, Message)>,
    t: Tokens,
) -> El<'a> {
    let mut r = Row::new().align_y(Alignment::Center);
    for (s, w) in specs.iter().zip(widths) {
        if s.col == Col::Select {
            let cell: El<'a> = match &select {
                Some((all, msg)) => {
                    let msg = msg.clone();
                    checkbox(*all)
                        .style(theme::check(t))
                        .on_toggle(move |_| msg.clone())
                        .into()
                }
                None => Space::new().into(),
            };
            r = r
                .push(container(cell).width(*w))
                .push(Space::new().width(theme::RESIZE_HANDLE));
            continue;
        }
        let arrow = match view.sort {
            Some((c, Dir::Asc)) if c == s.col => Some(Icon::ChevronUp),
            Some((c, Dir::Desc)) if c == s.col => Some(Icon::ChevronDown),
            _ => None,
        };
        let mut label = row![
            text(s.label)
                .size(theme::TEXT_META)
                .font(theme::FONT_STRONG)
                .color(t.text_muted)
        ]
        .spacing(theme::XS)
        .align_y(Alignment::Center);
        if let Some(i) = arrow {
            label = label.push(
                text(i.glyph())
                    .font(theme::ICONS)
                    .size(theme::TEXT_META)
                    .color(t.accent),
            );
        }
        r = r
            .push(
                button(label)
                    .width(*w)
                    .padding(Padding::from([theme::XS, 0.0]))
                    .style(theme::ghost_button(t, false))
                    .on_press(Message::TableSort(id, s.col)),
            )
            .push(
                mouse_area(
                    container(
                        container(Space::new())
                            .width(theme::BORDER_WIDTH)
                            .height(Length::Fill)
                            .style(theme::divider(t)),
                    )
                    .center_x(theme::RESIZE_HANDLE)
                    .height(theme::HEADER_HEIGHT),
                )
                .on_press(Message::ResizeStart(id, s.col))
                .interaction(mouse::Interaction::ResizingHorizontally),
            );
    }
    r.push(Space::new().width(Length::Fill))
        .push(Space::new().width(trailing))
        .padding(Padding::from([0.0, theme::MD]))
        .into()
}

/// Filter boxes under the header, for columns that have one.
pub fn filters<'a>(
    id: TableId,
    specs: &[Spec],
    widths: &[f32],
    view: &'a View,
    trailing: f32,
    t: Tokens,
) -> El<'a> {
    let mut r = Row::new().align_y(Alignment::Center);
    for (s, w) in specs.iter().zip(widths) {
        let cell: El<'a> = if s.filter {
            let col = s.col;
            text_input("Filter", view.filter(col))
                .on_input(move |v| Message::TableFilter(id, col, v))
                .size(theme::TEXT_META)
                .font(theme::FONT)
                .padding(theme::XS)
                .style(theme::input(t))
                .into()
        } else {
            Space::new().into()
        };
        r = r
            .push(container(cell).width(*w))
            .push(Space::new().width(theme::RESIZE_HANDLE));
    }
    r.push(Space::new().width(Length::Fill))
        .push(Space::new().width(trailing))
        .padding(Padding::from([0.0, theme::MD]))
        .into()
}

/// A body row's cells at the header's widths, with `trailing` (row actions) at the right.
pub fn cells<'a>(
    cells: Vec<El<'a>>,
    widths: &[f32],
    trailing: El<'a>,
    trailing_w: f32,
    height: f32,
) -> Row<'a, Message> {
    let mut r = row![].align_y(Alignment::Center).height(height);
    for (cell, w) in cells.into_iter().zip(widths) {
        r = r
            .push(container(cell).width(*w).clip(true))
            .push(Space::new().width(theme::RESIZE_HANDLE));
    }
    r.push(Space::new().width(Length::Fill)).push(
        container(trailing)
            .width(trailing_w)
            .align_right(trailing_w),
    )
}

#[cfg(test)]
mod tests {
    use crate::table::{Col, Dir, Key, View};

    #[test]
    fn sorts_filters_and_cycles() {
        let rows = vec![("b", 3u64), ("A", 10), ("c", 1)];
        let key = |r: &(&str, u64), c: Col| match c {
            Col::Downloads => Key::Num(r.1),
            _ => Key::text(r.0),
        };
        let mut v = View::default();
        v.toggle(Col::Name);
        let mut out = rows.clone();
        v.apply(&mut out, key);
        assert_eq!(out.iter().map(|r| r.0).collect::<Vec<_>>(), ["A", "b", "c"]);

        v.toggle(Col::Downloads);
        assert_eq!(v.sort, Some((Col::Downloads, Dir::Desc)));
        v.filters.insert(Col::Downloads, "1".into());
        let mut out = rows.clone();
        v.apply(&mut out, key);
        assert_eq!(out.iter().map(|r| r.0).collect::<Vec<_>>(), ["A", "c"]);

        v.toggle(Col::Downloads);
        v.toggle(Col::Downloads);
        assert_eq!(v.sort, None);
    }
}
