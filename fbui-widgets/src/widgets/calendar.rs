//! [`Calendar`] — a month-grid date picker.
//!
//! A header (previous/next month arrows around the month-year title), a
//! weekday row, and a fixed 6×7 day grid including the adjacent months' edge
//! days (muted). Fully headless and deterministic: the widget carries its own
//! proleptic-Gregorian [`Date`] math and — per the toolkit's no-wall-clock
//! rule — never asks the system for "today"; the app passes it in with
//! [`Calendar::today`] if it wants the marker.
//!
//! Interaction: tap a day (any, including a muted edge day — the view follows)
//! to pick it; the header arrows page by month. With focus, the arrow keys
//! move the selection by day/week across month boundaries, PageUp/PageDown by
//! month, Home/End to the month's first/last day, and Enter/Space re-emit the
//! pick.

use std::any::Any;

use fbui_render::geom::{Point, Rect, Size};
use fbui_render::{FontContext, PathBuilder};

use crate::ctx::{EventCtx, PaintCtx};
use crate::event::{Event, Key, PointerButton};
use crate::style::Style;
use crate::theme::Theme;
use crate::util::{focus_ring, text_style};
use crate::widget::{AvailableSize, KnownDims, Widget};

/// A calendar date in the proleptic Gregorian calendar. Plain data — no time
/// zone, no clock — with just enough arithmetic for a date picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: i32,
    /// 1–12.
    pub month: u8,
    /// 1–31, always valid for the month.
    pub day: u8,
}

impl Date {
    /// A validated date, or `None` if `month`/`day` are out of range.
    pub fn new(year: i32, month: u8, day: u8) -> Option<Date> {
        if !(1..=12).contains(&month) || day == 0 || day > Date::days_in_month(year, month) {
            return None;
        }
        Some(Date { year, month, day })
    }

    pub fn is_leap_year(year: i32) -> bool {
        year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
    }

    /// Number of days in a month (28–31).
    pub fn days_in_month(year: i32, month: u8) -> u8 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if Date::is_leap_year(year) => 29,
            2 => 28,
            _ => 0,
        }
    }

    /// Days since the epoch 1970-01-01 (negative before it). The standard
    /// era-based civil-calendar algorithm.
    pub fn to_days(self) -> i64 {
        let y = self.year as i64 - if self.month <= 2 { 1 } else { 0 };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400; // [0, 399]
        let m = self.month as i64;
        let d = self.day as i64;
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1; // [0, 365]
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
        era * 146097 + doe - 719468
    }

    /// The date `days` after the epoch (inverse of [`to_days`](Self::to_days)).
    pub fn from_days(days: i64) -> Date {
        let z = days + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = z - era * 146097; // [0, 146096]
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
        let mp = (5 * doy + 2) / 153; // [0, 11]
        let d = (doy - (153 * mp + 2) / 5 + 1) as u8; // [1, 31]
        let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8; // [1, 12]
        Date {
            year: (y + if m <= 2 { 1 } else { 0 }) as i32,
            month: m,
            day: d,
        }
    }

    /// Day of week, 0 = Monday … 6 = Sunday (ISO).
    pub fn weekday(self) -> u8 {
        // 1970-01-01 was a Thursday (ISO index 3).
        (self.to_days() + 3).rem_euclid(7) as u8
    }

    /// The date `n` days later (negative moves back), crossing months/years.
    pub fn offset_days(self, n: i64) -> Date {
        Date::from_days(self.to_days() + n)
    }

    /// The date `n` months later (negative moves back), the day clamped to the
    /// target month's length (Jan 31 + 1 month → Feb 28/29).
    pub fn offset_months(self, n: i32) -> Date {
        let total = self.year as i64 * 12 + (self.month as i64 - 1) + n as i64;
        let year = total.div_euclid(12) as i32;
        let month = (total.rem_euclid(12) + 1) as u8;
        let day = self.day.min(Date::days_in_month(year, month));
        Date { year, month, day }
    }
}

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Weekday initials starting from Monday; the widget rotates for a Sunday
/// week start.
const DAY_INITIALS: [&str; 7] = ["M", "T", "W", "T", "F", "S", "S"];

const CELL_W: f32 = 40.0;
const CELL_H: f32 = 32.0;
const HEADER_H: f32 = 36.0;
const WEEKDAY_H: f32 = 24.0;
const GRID_ROWS: usize = 6;

/// A month-view date picker. See the module docs for the interaction model.
pub struct Calendar<Msg> {
    /// First day of the month being displayed.
    view: Date,
    selected: Date,
    today: Option<Date>,
    week_starts_monday: bool,
    on_pick: Option<Box<dyn Fn(Date) -> Msg>>,
}

impl<Msg> Calendar<Msg> {
    /// A calendar showing (and selecting) `selected`'s month.
    pub fn new(selected: Date) -> Self {
        Calendar {
            view: Date { day: 1, ..selected },
            selected,
            today: None,
            week_starts_monday: true,
            on_pick: None,
        }
    }

    /// Mark a date with the "today" ring. The app supplies it — the widget
    /// never reads a clock.
    pub fn today(mut self, today: Date) -> Self {
        self.today = Some(today);
        self
    }

    /// Start weeks on Sunday instead of Monday.
    pub fn week_starts_sunday(mut self) -> Self {
        self.week_starts_monday = false;
        self
    }

    /// Message to emit when a day is picked (tap, or Enter/Space with focus).
    pub fn on_pick(mut self, f: impl Fn(Date) -> Msg + 'static) -> Self {
        self.on_pick = Some(Box::new(f));
        self
    }

    /// The currently selected date.
    pub fn selected(&self) -> Date {
        self.selected
    }

    /// The `(year, month)` currently displayed.
    pub fn view_month(&self) -> (i32, u8) {
        (self.view.year, self.view.month)
    }

    /// Select a date programmatically, moving the view to its month (call via
    /// [`Ui::with`](crate::Ui::with); pair with a repaint). No message is
    /// emitted.
    pub fn set_selected(&mut self, date: Date) {
        self.selected = date;
        self.view = Date { day: 1, ..date };
    }

    /// The column (0–6) a weekday index (0 = Monday) lands in under the
    /// configured week start.
    fn column_of(&self, weekday: u8) -> u8 {
        if self.week_starts_monday {
            weekday
        } else {
            (weekday + 1) % 7
        }
    }

    /// The date shown in the top-left grid cell: the start of the week
    /// containing the 1st of the viewed month.
    fn grid_start(&self) -> Date {
        let first = Date {
            day: 1,
            ..self.view
        };
        first.offset_days(-(self.column_of(first.weekday()) as i64))
    }

    /// Intrinsic size of the whole widget.
    fn intrinsic() -> Size {
        Size::new(
            7.0 * CELL_W,
            HEADER_H + WEEKDAY_H + GRID_ROWS as f32 * CELL_H,
        )
    }

    /// The rect of grid cell (row, col) within bounds `b`.
    fn cell_rect(b: Rect, row: usize, col: usize) -> Rect {
        Rect::new(
            b.x + col as f32 * CELL_W,
            b.y + HEADER_H + WEEKDAY_H + row as f32 * CELL_H,
            CELL_W,
            CELL_H,
        )
    }

    /// The previous/next month arrow hit zones in the header.
    fn arrow_rects(b: Rect) -> (Rect, Rect) {
        (
            Rect::new(b.x, b.y, HEADER_H, HEADER_H),
            Rect::new(b.right() - HEADER_H, b.y, HEADER_H, HEADER_H),
        )
    }

    /// Move the selection (keyboard navigation): view follows, repaint, no
    /// pick message.
    fn move_selection(&mut self, date: Date, ctx: &mut EventCtx<Msg>) {
        self.selected = date;
        self.view = Date { day: 1, ..date };
        ctx.request_paint();
        ctx.set_handled();
    }

    /// Pick a date: select it, follow the view, and emit.
    fn pick(&mut self, date: Date, ctx: &mut EventCtx<Msg>) {
        self.selected = date;
        self.view = Date { day: 1, ..date };
        if let Some(f) = &self.on_pick {
            ctx.emit(f(date));
        }
        ctx.request_paint();
        ctx.set_handled();
    }
}

impl<Msg: 'static> Widget<Msg> for Calendar<Msg> {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style::default()
    }

    fn measure(
        &mut self,
        _fonts: &mut FontContext,
        _theme: &Theme,
        _known: KnownDims,
        _available: AvailableSize,
    ) -> Option<Size> {
        Some(Self::intrinsic())
    }

    fn focusable(&self) -> bool {
        true
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let theme = ctx.theme();
        let (surface, accent, on_accent, text, muted, line) = (
            theme.palette.surface,
            theme.palette.accent,
            theme.palette.on_accent,
            theme.palette.text,
            theme.palette.muted,
            theme.palette.line,
        );
        let radius = theme.metrics.radius;
        let focus_w = theme.metrics.focus_width;
        let focused = ctx.is_focused();
        let font_size = theme.metrics.font_size;
        let st_title = text_style(theme, font_size, text);
        let st_day = text_style(theme, font_size - 2.0, text);
        let st_day_muted = text_style(theme, font_size - 2.0, muted);
        let st_day_selected = text_style(theme, font_size - 2.0, on_accent);
        let st_weekday = text_style(theme, font_size - 4.0, muted);
        let day_font = font_size - 2.0;

        let view = self.view;
        let grid_start = self.grid_start();
        let title = format!("{} {}", MONTH_NAMES[(view.month - 1) as usize], view.year);
        let week_starts_monday = self.week_starts_monday;

        let (p, fonts) = ctx.painter_and_fonts();
        p.fill_rounded_rect(b, radius, surface);

        // Header: ‹ month year ›
        let (prev_r, next_r) = Self::arrow_rects(b);
        for (r, left) in [(prev_r, true), (next_r, false)] {
            let cx = r.x + r.w / 2.0;
            let cy = r.y + r.h / 2.0;
            let s = 5.0;
            let dir = if left { -1.0 } else { 1.0 };
            let mut pb = PathBuilder::new();
            pb.move_to(cx - dir * s / 2.0, cy - s);
            pb.line_to(cx + dir * s, cy);
            pb.line_to(cx - dir * s / 2.0, cy + s);
            pb.close();
            if let Some(tri) = pb.finish() {
                p.fill_path(&tri, muted);
            }
        }
        let title_w = fonts.layout(&title, &st_title, None).size().w;
        fonts.draw_text(
            p,
            &title,
            &st_title,
            Point::new(
                b.x + (b.w - title_w) / 2.0,
                b.y + (HEADER_H - font_size) / 2.0,
            ),
            None,
        );
        // Hairline under the header.
        p.fill_rect(
            Rect::new(b.x + 6.0, b.y + HEADER_H - 1.0, b.w - 12.0, 1.0),
            line,
        );

        // Weekday initials.
        for col in 0..7usize {
            let idx = if week_starts_monday {
                col
            } else {
                (col + 6) % 7
            };
            let label = DAY_INITIALS[idx];
            let w = fonts.layout(label, &st_weekday, None).size().w;
            let cx = b.x + col as f32 * CELL_W + (CELL_W - w) / 2.0;
            fonts.draw_text(
                p,
                label,
                &st_weekday,
                Point::new(cx, b.y + HEADER_H + (WEEKDAY_H - (day_font - 2.0)) / 2.0),
                None,
            );
        }

        // Day grid: 6 fixed weeks from grid_start.
        let mut d = grid_start;
        for row in 0..GRID_ROWS {
            for col in 0..7usize {
                let cell = Self::cell_rect(b, row, col);
                let in_view = d.month == view.month && d.year == view.year;
                let is_selected = d == self.selected;
                let is_today = Some(d) == self.today;
                let day_rect = cell.inset(2.0);
                let style = if is_selected {
                    p.fill_rounded_rect(day_rect, radius - 2.0, accent);
                    &st_day_selected
                } else if in_view {
                    &st_day
                } else {
                    &st_day_muted
                };
                if is_today && !is_selected {
                    p.stroke_rounded_rect(day_rect.inset(0.5), radius - 2.0, accent, 1.0);
                }
                let label = format!("{}", d.day);
                let w = fonts.layout(&label, style, None).size().w;
                fonts.draw_text(
                    p,
                    &label,
                    style,
                    Point::new(
                        cell.x + (cell.w - w) / 2.0,
                        cell.y + (cell.h - day_font) / 2.0 - 1.0,
                    ),
                    None,
                );
                d = d.offset_days(1);
            }
        }

        if focused {
            focus_ring(p, b, radius, accent, focus_w);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        let b = ctx.bounds();
        let ev = ctx.event().clone();
        match ev {
            Event::PointerDown {
                button: PointerButton::Left,
                pos,
            } if b.contains_point(pos) => {
                ctx.request_focus();
                let (prev_r, next_r) = Self::arrow_rects(b);
                if prev_r.contains_point(pos) {
                    self.view = self.view.offset_months(-1);
                    ctx.request_paint();
                    ctx.set_handled();
                    return;
                }
                if next_r.contains_point(pos) {
                    self.view = self.view.offset_months(1);
                    ctx.request_paint();
                    ctx.set_handled();
                    return;
                }
                // A tap in the grid picks the day under it.
                let grid_top = b.y + HEADER_H + WEEKDAY_H;
                if pos.y >= grid_top {
                    let col = ((pos.x - b.x) / CELL_W).floor();
                    let row = ((pos.y - grid_top) / CELL_H).floor();
                    if (0.0..7.0).contains(&col) && (0.0..GRID_ROWS as f32).contains(&row) {
                        let date = self.grid_start().offset_days(row as i64 * 7 + col as i64);
                        self.pick(date, ctx);
                        return;
                    }
                }
                ctx.set_handled();
            }
            Event::Key {
                key, pressed: true, ..
            } if ctx.is_focused() => match key {
                Key::Left => self.move_selection(self.selected.offset_days(-1), ctx),
                Key::Right => self.move_selection(self.selected.offset_days(1), ctx),
                Key::Up => self.move_selection(self.selected.offset_days(-7), ctx),
                Key::Down => self.move_selection(self.selected.offset_days(7), ctx),
                Key::PageUp => self.move_selection(self.selected.offset_months(-1), ctx),
                Key::PageDown => self.move_selection(self.selected.offset_months(1), ctx),
                Key::Home => self.move_selection(
                    Date {
                        day: 1,
                        ..self.selected
                    },
                    ctx,
                ),
                Key::End => {
                    let last = Date::days_in_month(self.selected.year, self.selected.month);
                    self.move_selection(
                        Date {
                            day: last,
                            ..self.selected
                        },
                        ctx,
                    );
                }
                Key::Enter | Key::Space => {
                    self.pick(self.selected, ctx);
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::Date;

    #[test]
    fn leap_years() {
        assert!(Date::is_leap_year(2000)); // ÷400
        assert!(!Date::is_leap_year(1900)); // ÷100 only
        assert!(Date::is_leap_year(2024));
        assert!(!Date::is_leap_year(2026));
        assert_eq!(Date::days_in_month(2024, 2), 29);
        assert_eq!(Date::days_in_month(2026, 2), 28);
        assert_eq!(Date::days_in_month(2026, 8), 31);
    }

    #[test]
    fn validation() {
        assert!(Date::new(2026, 2, 29).is_none());
        assert!(Date::new(2024, 2, 29).is_some());
        assert!(Date::new(2026, 13, 1).is_none());
        assert!(Date::new(2026, 0, 1).is_none());
        assert!(Date::new(2026, 4, 31).is_none());
    }

    #[test]
    fn epoch_roundtrip_and_weekdays() {
        let epoch = Date::new(1970, 1, 1).unwrap();
        assert_eq!(epoch.to_days(), 0);
        assert_eq!(epoch.weekday(), 3); // Thursday
        assert_eq!(Date::new(2000, 1, 1).unwrap().weekday(), 5); // Saturday
        assert_eq!(Date::new(2026, 8, 14).unwrap().weekday(), 4); // Friday
                                                                  // Roundtrip across a wide range, including pre-epoch.
        for days in [-1_000_000i64, -719468, -1, 0, 1, 365, 20_000, 1_000_000] {
            assert_eq!(Date::from_days(days).to_days(), days, "days {days}");
        }
    }

    #[test]
    fn offset_days_crosses_boundaries() {
        let d = Date::new(2026, 1, 1).unwrap();
        assert_eq!(d.offset_days(-1), Date::new(2025, 12, 31).unwrap());
        assert_eq!(d.offset_days(31), Date::new(2026, 2, 1).unwrap());
        // Across a leap day.
        let feb28 = Date::new(2024, 2, 28).unwrap();
        assert_eq!(feb28.offset_days(1), Date::new(2024, 2, 29).unwrap());
        assert_eq!(feb28.offset_days(2), Date::new(2024, 3, 1).unwrap());
    }

    #[test]
    fn offset_months_clamps_day() {
        let jan31 = Date::new(2026, 1, 31).unwrap();
        assert_eq!(jan31.offset_months(1), Date::new(2026, 2, 28).unwrap());
        assert_eq!(
            jan31.offset_months(1).offset_months(1),
            Date::new(2026, 3, 28).unwrap()
        );
        assert_eq!(jan31.offset_months(-2), Date::new(2025, 11, 30).unwrap());
        assert_eq!(jan31.offset_months(12), Date::new(2027, 1, 31).unwrap());
        let jan31_leap = Date::new(2024, 1, 31).unwrap();
        assert_eq!(jan31_leap.offset_months(1), Date::new(2024, 2, 29).unwrap());
    }
}
