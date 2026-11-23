//! Screen composition for the three layouts. See
//! `planning/01-design-spec.md` §9.2 for the sketches these follow.

use pathwatch_core::model::Outcome;
use pathwatch_core::snapshot::{Card, Cell, Snapshot, Status, human_duration, outcome_word};
use pathwatch_core::stats::WindowStats;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::app::{App, KEYMAP, MOUSE, Screen, Tab};
use crate::theme::{Glyphs, Theme};
use crate::widgets::{self, gated, ms, panel, put, rate, width};

/// What kind of session this is. A demo is synthetic data and says so on
/// every frame.
#[derive(Debug, Clone)]
pub enum Session {
    Live,
    Demo {
        name: String,
        end_ns: u64,
    },
    /// A recorded session played back. Nothing is sent, and it says so.
    Replay {
        name: String,
        end_ns: u64,
    },
}

impl Session {
    /// The word for "the view is following the data": only a live session is
    /// live. A demo or a replay is playing, or has reached its end.
    pub fn running_word(&self, now_ns: u64) -> &'static str {
        match self {
            Session::Live => "LIVE",
            Session::Demo { end_ns, .. } | Session::Replay { end_ns, .. } if now_ns >= *end_ns => "ENDED",
            Session::Demo { .. } | Session::Replay { .. } => "PLAYING",
        }
    }
}

/// The parts of the header that do not come from measurements.
#[derive(Debug, Clone)]
pub struct Chrome {
    pub session: Session,
    /// Source context, e.g. `via 192.0.2.10`.
    pub source: String,
    pub notes: Vec<String>,
    /// The recorder's state, when this session is being recorded.
    pub recording: Option<std::sync::Arc<pathwatch_core::recorder::Status>>,
    /// What a route trace is allowed to send, in words; `None` where there
    /// is nothing to trace with.
    pub trace_budget: Option<String>,
    /// The mouse is captured: the help panel then says what it does.
    pub mouse: bool,
    /// Draw for new data at most four times a second.
    pub reduced_motion: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeClass {
    TooSmall,
    Compact,
    Standard,
    Large,
}

pub fn size_class(w: u16, h: u16) -> SizeClass {
    if w < 80 || h < 24 {
        SizeClass::TooSmall
    } else if w >= 160 && h >= 45 {
        SizeClass::Large
    } else if w >= 120 && h >= 35 {
        SizeClass::Standard
    } else {
        SizeClass::Compact
    }
}

/// What a place on the screen is, to a mouse. Drawing says where things
/// are; the app decides what a click or the wheel there means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A card, a table row, a legend name, or a timeline label.
    Provider(u8),
    /// A stretch of the shared time axis: `x` is its first column on screen
    /// and `columns` how many it has, oldest first.
    Time {
        x: u16,
        columns: u16,
    },
    Tab(Tab),
    /// A scrolling list.
    List,
    /// The compact layout's "all four / focused only" switch.
    Series,
}

/// What one frame leaves behind: how far its list can scroll, and where its
/// mouse targets are.
#[derive(Debug, Clone, Default)]
pub struct Frame {
    /// The largest useful scroll offset for the list on screen; 0 without one.
    pub max_scroll: usize,
    /// The same for the glossary page of the help panel.
    pub help_max_scroll: usize,
    pub targets: Vec<(Rect, Target)>,
}

impl Frame {
    /// The target at a cell. Where two overlap, the one added last is the
    /// more specific.
    pub fn at(&self, x: u16, y: u16) -> Option<Target> {
        self.targets.iter().rev().find(|(r, _)| r.contains((x, y).into())).map(|(_, t)| *t)
    }
}

const FOCUS_PANEL: u16 = 50;

/// The number of time columns the chart has at this terminal size — what the
/// snapshot must be downsampled to.
pub fn plot_columns_for(w: u16, h: u16) -> u16 {
    let inner = match size_class(w, h) {
        SizeClass::Large => w.saturating_sub(FOCUS_PANEL + 2),
        _ => w.saturating_sub(2),
    };
    widgets::plot_columns(inner)
}

fn window_label(secs: u64) -> String {
    if secs >= 3600 { format!("{} h", secs / 3600) } else { format!("{} m", secs / 60) }
}

fn rows(area: Rect, heights: &[u16]) -> Vec<Rect> {
    let mut y = area.y;
    heights
        .iter()
        .map(|h| {
            let r = Rect { x: area.x, y, width: area.width, height: (*h).min(area.bottom().saturating_sub(y)) };
            y += r.height;
            r
        })
        .collect()
}

/// Draws one frame. Returns what the caller needs to know about it: how far
/// the list on screen can scroll, so its position can be kept in range, and
/// where a mouse could act.
pub fn draw(buf: &mut Buffer, area: Rect, snap: &Snapshot, app: &App, theme: &Theme, chrome: &Chrome) -> Frame {
    let class = size_class(area.width, area.height);
    if class == SizeClass::TooSmall {
        put(buf, area, area.x, area.y, &format!("pathwatch needs at least 80 × 24; this terminal is {} × {}.", area.width, area.height), Style::default());
        return Frame::default();
    }
    let mut frame = match app.screen {
        Screen::Overview => overview(buf, area, snap, app, theme, chrome, class),
        Screen::Details => details(buf, area, snap, app, theme, chrome, class),
        Screen::Route => route(buf, area, snap, app, theme, chrome, class),
    };
    if app.help && app.glossary {
        frame.help_max_scroll = glossary(buf, area, theme, app.help_scroll);
        frame.targets.clear();
    } else if app.help {
        help(buf, area, theme, chrome.mouse);
        // Nothing under the panel can be clicked through it.
        frame.targets.clear();
    }
    if theme.glyphs == Glyphs::Ascii {
        ascii_only(buf, area);
    }
    theme.harden(buf, area);
    frame
}

fn header(buf: &mut Buffer, area: Rect, snap: &Snapshot, app: &App, theme: &Theme, chrome: &Chrome, lines: u16) {
    let (state, demo_note) = match &chrome.session {
        Session::Live if app.paused => ("STOPPED".to_string(), String::new()),
        Session::Live => ("COLLECTING".to_string(), String::new()),
        Session::Demo { name, end_ns } => {
            let ended = snap.live_ns >= *end_ns;
            (if ended { "DEMO ENDED".into() } else { "DEMO".into() }, format!("[DEMO: synthetic data · scenario {name}]"))
        }
        Session::Replay { name, end_ns } => {
            let ended = snap.live_ns >= *end_ns;
            (if ended { "REPLAY ENDED".into() } else { "REPLAY".into() }, format!("[REPLAY: recorded session {name} · nothing is being sent]"))
        }
    };
    let live_session = matches!(chrome.session, Session::Live);
    let tag = match chrome.session {
        Session::Live => "",
        Session::Demo { .. } => "  [DEMO]",
        Session::Replay { .. } => "  [REPLAY]",
    };
    // The recorder: how much it has written, or that it stopped and why.
    let megabytes = |b: u64| format!("{:.1} MB", b as f64 / 1e6);
    let recording = chrome.recording.as_ref().map(|r| match r.stopped() {
        Some(why) => (format!("REC STOPPED: {why} — measurement continues"), "REC STOPPED".to_string(), true),
        None => {
            let dropped = r.dropped.load(std::sync::atomic::Ordering::Relaxed);
            let lost = if dropped > 0 { format!(", {dropped} records not recorded") } else { String::new() };
            (format!("REC {}{lost}", megabytes(r.bytes.load(std::sync::atomic::Ordering::Relaxed))), "REC".to_string(), false)
        }
    });
    let arrow = if theme.glyphs == Glyphs::Unicode { "▸" } else { ">" };
    let up = human_duration(snap.live_ns / 1_000_000);
    let frozen = snap.view.frozen_at_ns.map(|f| {
        let goes_on = if live_session { "collection continues" } else { "playback continues" };
        format!("FROZEN {} · {goes_on} · {} new", human_duration(snap.live_ns.saturating_sub(f) / 1_000_000), snap.new_since_freeze)
    });
    // Stopped is not frozen: nothing is being sent, and the header says so.
    let stopped = app.paused.then(|| if lines == 1 { "STOPPED · P resumes".to_string() } else { "STOPPED by you · nothing is being sent · P resumes".to_string() });
    let live = stopped.clone().or(frozen.clone()).unwrap_or_else(|| chrome.session.running_word(snap.live_ns).into());
    let live_style = if stopped.is_some() {
        theme.status_style(&Status::NoReply)
    } else if frozen.is_some() {
        theme.status_style(&Status::Lossy)
    } else {
        Style::default()
    };

    put(buf, area, area.x + 1, area.y, "pathwatch", theme.bold());
    put(buf, area, area.x + 11, area.y, &format!("{arrow} {state}"), if app.paused { live_style } else { theme.bold() });
    if lines == 1 {
        let text = format!("{} v{}  {}  ", snap.view.mode.short(), snap.view.family.as_str(), window_label(snap.view.window.as_secs()));
        let x = area.x + 14 + width(&state);
        put(buf, area, x, area.y, &text, Style::default());
        put(buf, area, x + width(&text), area.y, &live, live_style);
        let right = format!("up {up}{tag}");
        let rx = area.right().saturating_sub(width(&right) + 1);
        put(buf, area, rx, area.y, &right, theme.dim());
        if let Some((_, short, stopped)) = &recording {
            let style = if *stopped { theme.status_style(&Status::LocalErr) } else { theme.dim() };
            put(buf, area, rx.saturating_sub(width(short) + 2), area.y, short, style);
        }
        return;
    }
    // A live or replayed session says where it is (or was) sending from,
    // as of the latest probe; a demo keeps its own words.
    let source = match (&chrome.session, &snap.source) {
        (Session::Live | Session::Replay { .. }, Some(now)) => now.as_str(),
        _ => chrome.source.as_str(),
    };
    let first = format!("mode {}   family IPv{}   {source}   up {up}", snap.view.mode.label(), snap.view.family.as_str());
    put(buf, area, area.x + 16 + width(&state), area.y, &first, Style::default());
    let mut x = area.x + 1;
    let window = format!("window {} · ", window_label(snap.view.window.as_secs()));
    put(buf, area, x, area.y + 1, &window, theme.dim());
    x += width(&window);
    put(buf, area, x, area.y + 1, &live, live_style);
    x += width(&live);
    let notes = chrome.notes.iter().fold(String::new(), |acc, n| format!("{acc} · {n}"));
    put(buf, area, x, area.y + 1, &notes, theme.dim());
    x += width(&notes);
    if let Some((long, _, stopped)) = &recording {
        put(buf, area, x, area.y + 1, " · ", theme.dim());
        put(buf, area, x + 3, area.y + 1, long, if *stopped { theme.status_style(&Status::LocalErr) } else { theme.dim() });
    }
    if !demo_note.is_empty() {
        put(buf, area, area.right().saturating_sub(width(&demo_note) + 1), area.y + 1, &demo_note, theme.status_style(&Status::Lossy));
    }
}

/// What one series shows in the cursor's time column.
fn at_cursor(c: &Card, off: u16, ranges: bool) -> String {
    if c.status.is_inactive() {
        return "n/a".into();
    }
    let col = widgets::cursor_column(c.columns.len(), off).and_then(|i| c.columns.get(i).zip(c.cells.get(i)));
    match col {
        Some((col, _)) if col.n > 1 && ranges && col.min_us != col.max_us => format!("{} ({}–{})", ms(col.med_us), ms(col.min_us), ms(col.max_us)),
        Some((col, _)) if col.n > 0 => ms(col.med_us),
        Some((_, Cell::NoneAnswered | Cell::SomeUnanswered)) => "no reply".into(),
        Some((_, Cell::ErrorResponse)) => "error".into(),
        Some((_, Cell::LocalError)) => "local err".into(),
        Some((_, Cell::Skipped)) => "skipped".into(),
        Some((_, Cell::Gap)) => "gap".into(),
        _ => "—".into(),
    }
}

/// The exact values under the chart cursor, for the footer.
fn cursor_readout(snap: &Snapshot, app: &App, off: u16, only_focus: bool, ranges: bool) -> String {
    let n = u64::from(snap.view.columns.max(1));
    let when = if off == 0 { "now".to_string() } else { format!("-{}", widgets::window_label(snap.view.window.as_secs() * u64::from(off) / n)) };
    let values: Vec<String> =
        snap.cards.iter().filter(|c| !only_focus || c.slot == app.focus).map(|c| format!("{} {}", widgets::tag(&c.name), at_cursor(c, off, ranges))).collect();
    format!(" cursor {when} · ms: {} ", values.join(" · "))
}

fn footer(buf: &mut Buffer, area: Rect, snap: &Snapshot, app: &App, theme: &Theme, chrome: &Chrome, wide: bool) {
    // A question, a message, or the cursor takes the line over, in that order.
    if app.confirm_stop {
        let ask = if wide {
            " Stop collection? Nothing is sent until you press P again; the pause is recorded as a gap.   y stop · any other key cancels"
        } else {
            " Stop collection? Recorded as a gap; P resumes.   y stop · any other key cancels"
        };
        put(buf, area, area.x, area.y, ask, theme.status_style(&Status::Lossy).add_modifier(Modifier::BOLD));
        return;
    }
    if let Some(note) = app.note {
        put(buf, area, area.x + 1, area.y, note, theme.bold());
        return;
    }
    if let (Some(off), Screen::Overview) = (app.cursor, app.screen) {
        let text = cursor_readout(snap, app, off, !wide && app.only_focus, area.width >= 120);
        put(buf, area, area.x, area.y, &text, theme.bold());
        put(buf, area, area.x + width(&text), area.y, if wide { "· h/l move · H/L by ten · Esc off" } else { "· h/l H/L · Esc" }, theme.dim());
        return;
    }
    let name = snap.cards.iter().find(|c| c.slot == app.focus).map_or("", |c| c.name.as_str());
    let sep = if theme.glyphs == Glyphs::Unicode { "│" } else { "|" };
    let state = if app.paused {
        "STOPPED"
    } else if snap.view.frozen_at_ns.is_some() {
        "FROZEN"
    } else {
        chrome.session.running_word(snap.live_ns)
    };
    let left = format!(" {} {name} {sep} {} {sep} {state} {sep} ", app.focus, window_label(snap.view.window.as_secs()));
    put(buf, area, area.x, area.y, &left, theme.bold());
    let keys = match (app.screen, wide) {
        (Screen::Overview, true) => "1-4 · Enter · m mode · f family · +/- window · Space freeze · c cursor · ? help · q",
        (Screen::Overview, false) => "1-4 · Enter · m · f · +/- · Space · c · ? · q",
        (Screen::Details, true) => "h/l tab · j/k scroll · 1-4 provider · m mode · f family · Esc back · ? · q",
        (Screen::Details, false) => "h/l tab · j/k scroll · 1-4 · m · f · Esc · ? · q",
        (Screen::Route, true) => "Enter start · t stop · n names · j/k scroll · Esc back (stops it) · ? · q",
        (Screen::Route, false) => "Enter start · t stop · n names · Esc · q",
    };
    put(buf, area, area.x + width(&left), area.y, keys, theme.dim());
}

/// Said in the chart's title whenever a value is above the scale, so a mark
/// on the top row is never left unexplained.
fn off_scale_note(snap: &Snapshot, only: Option<u8>, theme: &Theme) -> String {
    widgets::off_scale_peak(snap, only).map_or_else(String::new, |peak| format!(" · {} off scale, max {}", widgets::off_scale_mark(theme), ms(peak)))
}

fn chart_title(snap: &Snapshot, theme: &Theme) -> String {
    let note = off_scale_note(snap, None, theme);
    // The scale is always common to every series; with a note there is no room to repeat it.
    let scale = if note.is_empty() { "common scale, ms" } else { "ms" };
    format!("Latency · {} · IPv{} · {scale}{note}", snap.view.mode.metric(), snap.view.family.as_str())
}

fn legend_entry(c: &Card) -> String {
    if c.status.is_inactive() { format!("{} n/a", c.name) } else { c.name.clone() }
}

const LEGEND_BETWEEN: &str = " · ";

fn legend(snap: &Snapshot) -> String {
    snap.cards.iter().map(legend_entry).collect::<Vec<_>>().join(LEGEND_BETWEEN)
}

/// The timeline's targets: a row's label is its provider, and the cells are
/// the time axis.
fn timeline_targets(area: Rect, snap: &Snapshot, targets: &mut Vec<(Rect, Target)>) {
    let (x, columns) = widgets::timeline_span(area, snap);
    let rows = (snap.cards.len() as u16).min(area.height);
    for (i, c) in snap.cards.iter().take(usize::from(rows)).enumerate() {
        targets.push((Rect { x: area.x, y: area.y + i as u16, width: x.saturating_sub(area.x).min(area.width), height: 1 }, Target::Provider(c.slot)));
    }
    if columns > 0 && rows > 0 {
        targets.push((Rect { x, y: area.y, width: columns, height: rows }, Target::Time { x, columns }));
    }
}

/// The plot's target: its time columns, over the full height of the panel.
fn plot_target(inner: Rect, targets: &mut Vec<(Rect, Target)>) {
    if let Some((x, columns)) = widgets::plot_span(inner) {
        targets.push((Rect { x, y: inner.y, width: columns, height: inner.height }, Target::Time { x, columns }));
    }
}

fn overview(buf: &mut Buffer, area: Rect, snap: &Snapshot, app: &App, theme: &Theme, chrome: &Chrome, class: SizeClass) -> Frame {
    let (mode, family) = (snap.view.mode.short(), snap.view.family.as_str());
    let mut targets = Vec::new();
    match class {
        SizeClass::Compact => {
            let chart_h = area.height.saturating_sub(12);
            let r = rows(area, &[1, 5, chart_h, 4, 1, 1]);
            let chart_row = controls_line(buf, r[2], snap, theme);
            header(buf, r[0], snap, app, theme, chrome, 1);
            widgets::table(buf, r[1], snap, app.focus, theme);
            let focused = snap.cards.iter().find(|c| c.slot == app.focus);
            let title = if app.only_focus {
                format!("{} {} · {}, ms{}", app.focus, focused.map_or("", |c| c.name.as_str()), snap.view.mode.metric(), off_scale_note(snap, Some(app.focus), theme))
            } else {
                chart_title(snap, theme)
            };
            let switch = if app.only_focus { "a: all four" } else { "a: focused only" };
            let inner = panel(buf, chart_row, &title, switch, theme, theme.border(false), theme.bold());
            widgets::plot(buf, inner, snap, app.focus, app.only_focus, app.cursor, theme);
            // Same x-axis as the chart above: offset by the panel border.
            let timeline = Rect { x: r[3].x + 1, width: r[3].width.saturating_sub(2), ..r[3] };
            widgets::timeline(buf, timeline, snap, app.cursor, theme);
            for (i, c) in snap.cards.iter().enumerate() {
                let row = Rect { y: r[1].y + 1 + i as u16, height: 1, ..r[1] };
                targets.push((row.intersection(r[1]), Target::Provider(c.slot)));
            }
            if let Some(x) = widgets::panel_note_x(chart_row, &title, switch) {
                targets.push((Rect { x, y: chart_row.y, width: width(switch), height: 1 }, Target::Series));
            }
            plot_target(inner, &mut targets);
            timeline_targets(timeline, snap, &mut targets);
            widgets::events(buf, r[4], &snap.events, theme);
            footer(buf, r[5], snap, app, theme, chrome, false);
        }
        SizeClass::Standard | SizeClass::Large => {
            let large = class == SizeClass::Large;
            let (cards_h, tl_h, ev_h) = if large { (10, 7, 6) } else { (9, 6, 5) };
            let chart_h = area.height.saturating_sub(2 + cards_h + tl_h + ev_h + 1);
            let r = rows(area, &[2, cards_h, chart_h, tl_h, ev_h, 1]);
            let chart_row = controls_line(buf, r[2], snap, theme);
            header(buf, r[0], snap, app, theme, chrome, 2);
            let n = snap.cards.len().max(1) as u16;
            let cw = r[1].width / n;
            for (i, c) in snap.cards.iter().enumerate() {
                let x = r[1].x + cw * i as u16;
                let w = if i as u16 == n - 1 { r[1].right() - x } else { cw };
                let card = Rect { x, y: r[1].y, width: w, height: r[1].height };
                widgets::card(buf, card, c, widgets::CardLook { mode, family, focused: c.slot == app.focus, tall: large }, theme);
                targets.push((card, Target::Provider(c.slot)));
            }
            let chart_area = if large { Rect { width: chart_row.width.saturating_sub(FOCUS_PANEL), ..chart_row } } else { chart_row };
            let (title, names) = (chart_title(snap, theme), legend(snap));
            let inner = panel(buf, chart_area, &title, &names, theme, theme.border(false), theme.bold());
            widgets::plot(buf, inner, snap, app.focus, false, app.cursor, theme);
            plot_target(inner, &mut targets);
            // Each name in the legend is its provider.
            if let Some(mut x) = widgets::panel_note_x(chart_area, &title, &names) {
                for c in &snap.cards {
                    let w = width(&legend_entry(c));
                    targets.push((Rect { x, y: chart_area.y, width: w, height: 1 }, Target::Provider(c.slot)));
                    x += w + width(LEGEND_BETWEEN);
                }
            }
            if large {
                focus_panel(buf, Rect { x: chart_area.right(), width: FOCUS_PANEL, ..chart_row }, snap, app, theme);
            }
            let inner = panel(buf, r[3], "Outcomes · same time axis as the chart", &widgets::timeline_legend(theme), theme, theme.border(false), theme.bold());
            widgets::timeline(buf, inner, snap, app.cursor, theme);
            timeline_targets(inner, snap, &mut targets);
            let inner = panel(
                buf,
                r[4],
                "Events",
                &format!("newest first · {} of {}", inner_rows(r[4]).min(snap.events.len() as u16), snap.events.len()),
                theme,
                theme.border(false),
                theme.bold(),
            );
            widgets::events(buf, inner, &snap.events, theme);
            footer(buf, r[5], snap, app, theme, chrome, true);
        }
        SizeClass::TooSmall => {}
    }
    Frame { max_scroll: 0, help_max_scroll: 0, targets }
}

/// The controls, when any is configured: one quiet line between the
/// providers and the chart. They are not providers and never look like one.
/// Returns what is left of `area` for the chart.
fn controls_line(buf: &mut Buffer, area: Rect, snap: &Snapshot, theme: &Theme) -> Rect {
    if snap.controls.is_empty() || area.height < 6 {
        return area;
    }
    // Each control at four levels of detail; the fullest that fits whole
    // is drawn. Nothing on this line is ever cut mid-way.
    let pieces = |c: &pathwatch_core::snapshot::ControlRow, level: u8| -> Vec<(String, Style)> {
        let label = c.tag.map_or_else(|| c.label.to_string(), |tag| format!("{} ({tag})", c.label));
        let mut out = vec![(label, theme.bold())];
        if level >= 1 {
            out.push((c.target.clone(), theme.dim()));
        }
        out.push((format!("{} {}", theme.status_symbol(&c.status), c.status.word()), theme.status_style(&c.status)));
        let s = &c.stats;
        out.push((
            match (&c.status, level) {
                // Why there is nothing to show, in place of numbers.
                (Status::Unsupported(why) | Status::NeedsConfig(why), 2..) => format!("{why}; nothing sent"),
                (Status::Unsupported(_) | Status::NeedsConfig(_) | Status::Disabled, _) => "nothing sent".to_string(),
                (_, 2..) => format!(
                    "{} · no-reply {}",
                    c.last_us.map_or_else(|| "—".to_string(), |v| format!("{} ms", ms(v))),
                    rate(s.no_reply_rate, s.unanswered, s.finalised).replace(" / ", "/")
                ),
                _ => format!("{} {}/{}", c.last_us.map_or_else(|| "—".to_string(), |v| format!("{} ms", ms(v))), s.unanswered, s.finalised),
            },
            theme.dim(),
        ));
        if let (Some(note), 3) = (c.note, level) {
            out.push((format!("({note})"), theme.dim()));
        }
        out
    };
    let room = area.width.saturating_sub(2);
    let line = |level: u8| -> Vec<(String, Style)> {
        snap.controls.iter().enumerate().flat_map(|(i, c)| (i > 0).then(|| ("·".to_string(), theme.dim())).into_iter().chain(pieces(c, level))).collect()
    };
    let fits = |l: &[(String, Style)]| l.iter().map(|(t, _)| width(t) + 1).sum::<u16>() <= room;
    let chosen = [3, 2, 1, 0].into_iter().map(line).find(|l| fits(l)).unwrap_or_else(|| line(0));
    let mut x = area.x + 1;
    for (text, style) in chosen {
        put(buf, area, x, area.y, &widgets::ellipsis(&text, usize::from(area.right().saturating_sub(x + 1))), style);
        x += width(&text) + 1;
    }
    Rect { y: area.y + 1, height: area.height - 1, ..area }
}

fn inner_rows(r: Rect) -> u16 {
    r.height.saturating_sub(2)
}

/// The large layout's side panel: every counter behind the focused card.
fn focus_panel(buf: &mut Buffer, area: Rect, snap: &Snapshot, app: &App, theme: &Theme) {
    let Some(c) = snap.cards.iter().find(|c| c.slot == app.focus) else { return };
    let title = format!("Focus · {} {} · {} v{}", c.slot, c.name, snap.view.mode.short(), snap.view.family.as_str());
    let inner = panel(buf, area, &title, "", theme, theme.border(false), theme.provider(c.slot).add_modifier(Modifier::BOLD));
    let x = inner.x + 1;
    let mut y = inner.y;
    let mut line = |text: String, style: Style, y: &mut u16| {
        put(buf, inner, x, *y, &text, style);
        *y += 1;
    };
    match (&c.status, &c.stats) {
        (s, _) if s.is_inactive() => {
            line(format!("{} {}", theme.status_symbol(s), s.word()), theme.status_style(s), &mut y);
            line("Nothing is being sent; not counted as loss.".into(), theme.dim(), &mut y);
        }
        (_, Some(s)) => {
            line(c.target.clone(), Style::default(), &mut y);
            y += 1;
            line(format!("min {}  mean {}  sd {}", s.min_us.map_or("—".into(), ms), gated(s.mean_us), gated(s.sd_us)), Style::default(), &mut y);
            line(format!("p50 {}  p95 {}  p99 {}", gated(s.p50_us), gated(s.p95_us), gated(s.p99_us)), Style::default(), &mut y);
            line(format!("max {}  Δ {} over {} pairs", s.max_us.map_or("—".into(), ms), gated(s.delta_us), s.delta_pairs), Style::default(), &mut y);
            y += 1;
            line(
                format!("no-reply {}   error responses {}", rate(s.no_reply_rate, s.unanswered, s.finalised), rate(s.error_rate, s.errors, s.finalised)),
                Style::default(),
                &mut y,
            );
            line(format!("longest unanswered run {} · current {}", s.longest_unanswered_run, s.unanswered_run), theme.dim(), &mut y);
            y += 1;
            line(format!("sent {} · replied {} · timed out {}", s.sent, s.replied, s.timed_out), theme.dim(), &mut y);
            line(format!("unreachable {} · late {} · duplicate {}", s.unreachable, s.late, s.duplicates), theme.dim(), &mut y);
            line(format!("local error {} · skipped {} · pending {}", s.local_error, s.skipped, s.pending), theme.dim(), &mut y);
            line(format!("cancelled {} · coverage {:.0} %", s.cancelled, s.coverage * 100.0), theme.dim(), &mut y);
        }
        _ => {}
    }
}

/// A scrolling list of single-style lines. Returns the largest useful offset.
fn list(buf: &mut Buffer, area: Rect, lines: &[(String, Style)], scroll: usize) -> usize {
    let visible = usize::from(area.height);
    let max = lines.len().saturating_sub(visible);
    for (i, (text, style)) in lines.iter().skip(scroll.min(max)).take(visible).enumerate() {
        put(buf, area, area.x + 1, area.y + i as u16, text, *style);
    }
    max
}

/// "rows 1–17 of 200", for a panel's corner.
fn rows_note(total: usize, visible: u16, scroll: usize) -> String {
    let visible = usize::from(visible);
    if total <= visible {
        return if total == 1 { "1 row".to_string() } else { format!("{total} rows") };
    }
    let start = scroll.min(total - visible);
    format!("rows {}–{} of {total} · j/k scroll", start + 1, start + visible)
}

fn details(buf: &mut Buffer, area: Rect, snap: &Snapshot, app: &App, theme: &Theme, chrome: &Chrome, class: SizeClass) -> Frame {
    let header_h = if class == SizeClass::Compact { 1 } else { 2 };
    let r = rows(area, &[header_h, 1, area.height.saturating_sub(header_h + 2), 1]);
    header(buf, r[0], snap, app, theme, chrome, header_h);
    footer(buf, r[3], snap, app, theme, chrome, class != SizeClass::Compact);
    let Some(c) = snap.cards.iter().find(|c| c.slot == app.focus) else { return Frame::default() };
    let mut targets = Vec::new();

    // The tab bar: the provider, then every tab, the current one bracketed and bold.
    let who = format!(" {} {}  ", c.slot, c.name);
    put(buf, r[1], r[1].x, r[1].y, &who, theme.provider(c.slot).add_modifier(Modifier::BOLD));
    let mut x = r[1].x + width(&who);
    for tab in Tab::ALL {
        let (text, style) = if tab == app.tab { (format!("[{}]", tab.label()), theme.bold()) } else { (format!(" {} ", tab.label()), theme.dim()) };
        put(buf, r[1], x, r[1].y, &text, style);
        targets.push((Rect { x, y: r[1].y, width: width(&text), height: 1 }.intersection(r[1]), Target::Tab(tab)));
        x += width(&text) + 1;
    }

    let shown = format!("{} v{}", snap.view.mode.short(), snap.view.family.as_str());
    let name_style = theme.provider(c.slot).add_modifier(Modifier::BOLD);
    let body = r[2];
    let inner_h = body.height.saturating_sub(2);
    // The tabs that are lists scroll under the wheel.
    if matches!(app.tab, Tab::Dns | Tab::Raw | Tab::Events) {
        targets.push((body, Target::List));
    }
    let max_scroll = match app.tab {
        Tab::Streams => {
            let inner = panel(buf, body, "Every stream of this provider", "Esc back", theme, theme.border(true), name_style);
            streams_tab(buf, inner, snap, c, theme);
            0
        }
        Tab::Distribution => {
            let title = format!("Distribution · {shown} · window {}", window_label(snap.view.window.as_secs()));
            let inner = panel(buf, body, &title, "m mode · f family · +/- window", theme, theme.border(true), name_style);
            distribution_tab(buf, inner, c, theme);
            0
        }
        Tab::Dns => {
            let lines = dns_lines(snap, c, theme, usize::from(body.width.saturating_sub(4)));
            let inner = panel(buf, body, "DNS · what each query returned, per stream", &rows_note(lines.len(), inner_h, app.scroll), theme, theme.border(true), name_style);
            list(buf, inner, &lines, app.scroll)
        }
        Tab::Raw => {
            let lines = raw_lines(c, theme);
            let visible = inner_h.saturating_sub(1);
            let inner =
                panel(buf, body, &format!("Raw observations · {shown} · newest first"), &rows_note(lines.len(), visible, app.scroll), theme, theme.border(true), name_style);
            put(buf, inner, inner.x + 1, inner.y, &raw_head(), theme.dim());
            list(buf, Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner }, &lines, app.scroll)
        }
        Tab::Events => {
            let lines = event_lines(snap, c, theme, usize::from(body.width.saturating_sub(4)));
            let inner = panel(
                buf,
                body,
                "Events · this provider and the whole session · newest first",
                &rows_note(lines.len(), inner_h, app.scroll),
                theme,
                theme.border(true),
                name_style,
            );
            if lines.is_empty() {
                put(buf, inner, inner.x + 1, inner.y, "no events yet", theme.dim());
            }
            list(buf, inner, &lines, app.scroll)
        }
    };
    Frame { max_scroll, help_max_scroll: 0, targets }
}

/// The Route view: what a trace would send, then what it found, with every
/// row's caveat beside it. It never suggests more than the counts support.
fn route(buf: &mut Buffer, area: Rect, snap: &Snapshot, app: &App, theme: &Theme, chrome: &Chrome, class: SizeClass) -> Frame {
    use pathwatch_core::trace::State;
    let header_h = if class == SizeClass::Compact { 1 } else { 2 };
    let r = rows(area, &[header_h, area.height.saturating_sub(header_h + 1), 1]);
    header(buf, r[0], snap, app, theme, chrome, header_h);
    footer(buf, r[2], snap, app, theme, chrome, class != SizeClass::Compact);
    let Some(dest) = app.trace_target else { return Frame::default() };
    let room = usize::from(r[1].width.saturating_sub(4));
    let mut lines: Vec<(String, Style)> = Vec::new();
    // A trace of some other address is not this view's.
    let trace = app.trace.as_deref().filter(|t| t.dest == dest);
    let state = match trace.map(|t| t.state) {
        None => "not started",
        Some(State::Running) => "RUNNING",
        Some(State::Finished) => "finished",
        Some(State::Capped) => "stopped at the probe cap",
        Some(State::Stopped) => "stopped",
        Some(State::Failed) => "FAILED",
    };
    match trace {
        None => {
            lines.push((format!("Enter starts a trace to {dest}. Nothing has been sent."), theme.bold()));
            lines.push((String::new(), Style::default()));
            let budget = chrome.trace_budget.as_deref().unwrap_or("no budget is configured");
            lines.extend(widgets::wrap(&format!("It would send: {budget}."), room).into_iter().map(|l| (l, Style::default())));
            lines.extend(
                widgets::wrap("It runs once, on a socket of its own, and stops by itself. `t` stops it early; leaving this view stops it.", room)
                    .into_iter()
                    .map(|l| (l, theme.dim())),
            );
        }
        Some(t) => {
            let b = &t.budget;
            lines.push((format!("round {} of {} · probes {} of {} · at most {} a second", t.round.min(b.rounds), b.rounds, t.probes_sent, b.max_probes, b.max_rate), theme.bold()));
            if let Some(e) = &t.error {
                lines.extend(widgets::wrap(e, room).into_iter().map(|l| (l, theme.status_style(&Status::LocalErr))));
            }
            lines.push((format!("{:>3}  {:<46} {:>9} {:>8} {:>8} {:>8} {:>8}", "ttl", "answered by", "repl/sent", "last", "avg", "best", "worst"), theme.dim()));
            let t_ms = |v: Option<u32>| v.map_or_else(|| "—".to_string(), ms);
            for h in &t.hops {
                let who = match h.addrs.first() {
                    None => "*".to_string(),
                    Some((addr, _)) => {
                        let name = t.names.iter().find(|(a, _)| a == addr).filter(|_| app.names).map(|(_, n)| format!(" ({n})")).unwrap_or_default();
                        let dest_mark = if h.is_destination { " — destination" } else { "" };
                        // With more than one address, say how often this one answered.
                        let share = if h.addrs.len() > 1 { format!(", {} time(s)", h.addrs[0].1) } else { String::new() };
                        widgets::ellipsis(&format!("{addr}{name}{share}{dest_mark}"), 46)
                    }
                };
                let row = format!(
                    "{:>3}  {who:<46} {:>9} {:>8} {:>8} {:>8} {:>8}",
                    h.ttl,
                    format!("{}/{}", h.replies, h.sent),
                    t_ms(h.last_us),
                    t_ms(h.avg_us()),
                    t_ms(h.best_us),
                    t_ms(h.worst_us)
                );
                lines.push((row, if h.replies == 0 { theme.dim() } else { Style::default() }));
                for (addr, n) in h.addrs.iter().skip(1) {
                    lines.push((format!("       also {addr}, {n} time(s)"), theme.dim()));
                }
                for note in &h.notes {
                    lines.push((format!("       ↳ {note}"), theme.dim()));
                }
            }
        }
    }
    lines.push((String::new(), Style::default()));
    for caveat in [
        "This is an ICMP trace. DNS-over-UDP and HTTPS traffic to the same address may take another path.",
        "Each row is its own round trip: rows are not additive, and one must not be subtracted from the next.",
        "If the destination is an anycast address, the answering site can change over time and between protocols.",
    ] {
        lines.extend(widgets::wrap(caveat, room).into_iter().map(|l| (l, theme.dim())));
    }
    let name_style = theme.provider(app.focus).add_modifier(Modifier::BOLD);
    let inner = panel(buf, r[1], &format!("Route to {dest} · ICMP echo with a limited TTL"), state, theme, theme.border(true), name_style);
    Frame { max_scroll: list(buf, inner, &lines, app.scroll), help_max_scroll: 0, targets: vec![(r[1], Target::List)] }
}

fn streams_tab(buf: &mut Buffer, inner: Rect, snap: &Snapshot, c: &Card, theme: &Theme) {
    let x = inner.x + 1;
    // Three widths: everything on one line, the counters on a second line,
    // or the essentials only. Nothing is ever cut off at the panel's edge.
    let (wide, counters) = (inner.width >= 114, inner.width >= 148);
    let head = if wide {
        format!(
            "{:<16} {:<24} {:<14} {:>7} {:>7} {:>7} {:>9} {:>9} {:>5} {:>5}  {}",
            "stream",
            "destination",
            "status",
            "last",
            "p50",
            "p95",
            "no-reply",
            "err-resp",
            "n",
            "cov",
            if counters { "sent/pend/t-o/unr/err/local/skip" } else { "" }
        )
    } else {
        format!("{:<16} {:<14} {:>7} {:>7} {:>7} {:>9} {:>5}", "stream", "status", "last", "p50", "p95", "no-reply", "n")
    };
    put(buf, inner, x, inner.y, &head, theme.dim());
    let mut y = inner.y + 1;
    for row in snap.streams.iter().filter(|s| s.provider == c.key) {
        if y >= inner.bottom() {
            break;
        }
        let (s, row_y) = (&row.stats, y);
        put(buf, inner, x, y, &format!("{:<16}", row.label), Style::default());
        let mut cx = x + 17;
        if wide {
            put(buf, inner, cx, y, &row.target.chars().take(24).collect::<String>(), theme.dim());
            cx += 25;
        }
        put(buf, inner, cx, y, &format!("{} {}", theme.status_symbol(&row.status), row.status.word()), theme.status_style(&row.status));
        cx += 15;
        if row.status.is_inactive() {
            let why = match &row.status {
                Status::NeedsConfig(r) | Status::Unsupported(r) => r.as_str(),
                _ => "switched off",
            };
            put(buf, inner, cx, y, &format!("{why} — nothing sent; not counted as loss"), theme.dim());
        } else {
            let nr = rate(s.no_reply_rate, s.unanswered, s.finalised);
            let text = if wide {
                let line = format!(
                    "{:>7} {:>7} {:>7} {:>9} {:>9} {:>5} {:>4.0}%",
                    row.last_us.map_or("—".into(), ms),
                    gated(s.p50_us),
                    gated(s.p95_us),
                    nr,
                    rate(s.error_rate, s.errors, s.finalised),
                    s.n,
                    s.coverage * 100.0
                );
                if counters {
                    format!("{line}  {}/{}/{}/{}/{}/{}/{}", s.sent, s.pending, s.timed_out, s.unreachable, s.errors, s.local_error, s.skipped)
                } else {
                    y += 1;
                    let detail = format!(
                        "sent {} · pending {} · timed out {} · unreachable {} · error responses {} · local errors {} · skipped {}",
                        s.sent, s.pending, s.timed_out, s.unreachable, s.errors, s.local_error, s.skipped
                    );
                    put(buf, inner, x + 17, y, &detail, theme.dim());
                    line
                }
            } else {
                format!("{:>7} {:>7} {:>7} {:>9} {:>5}", row.last_us.map_or("—".into(), ms), gated(s.p50_us), gated(s.p95_us), nr, s.n)
            };
            put(buf, inner, cx, row_y, &text, Style::default());
        }
        y += 1;
    }
    y += 1;
    for note in [
        "Times are in ms. `—` means too few samples to show; `·` marks a low sample count.",
        "no-reply = (timed out + unreachable) ÷ finalised probes. It is not proof of packet loss on your line.",
        "ICMP round trips end at the kernel's receive stamp; DNS and DoH end at this process's read.",
        "DNS times are cache-influenced. A fresh or dns-tcp stream connects anew for each probe; its time includes that.",
    ] {
        for line in widgets::wrap(note, usize::from(inner.width.saturating_sub(2))) {
            put(buf, inner, x, y, &line, theme.dim());
            y += 1;
        }
    }
}

/// One line of the window / since-start table.
fn stat_rows(w: &WindowStats, s: Option<&WindowStats>) -> Vec<(&'static str, String, String)> {
    let both = |f: &dyn Fn(&WindowStats) -> String| (f(w), s.map_or_else(String::new, f));
    let opt = |v: Option<u32>| v.map_or("—".to_string(), ms);
    let mut out = Vec::new();
    let mut row = |label: &'static str, f: &dyn Fn(&WindowStats) -> String| {
        let (a, b) = both(f);
        out.push((label, a, b));
    };
    row("round trips", &|x| x.n.to_string());
    row("min", &|x| opt(x.min_us));
    row("p50", &|x| gated(x.p50_us));
    row("mean", &|x| gated(x.mean_us));
    row("p95", &|x| gated(x.p95_us));
    row("p99", &|x| gated(x.p99_us));
    row("max", &|x| opt(x.max_us));
    row("sd", &|x| gated(x.sd_us));
    row("Δ adjacent", &|x| gated(x.delta_us));
    row("no-reply", &|x| rate(x.no_reply_rate, x.unanswered, x.finalised));
    row("error resp.", &|x| rate(x.error_rate, x.errors, x.finalised));
    row("no-reply run", &|x| format!("longest {}", x.longest_unanswered_run));
    row("sent", &|x| x.sent.to_string());
    row("coverage", &|x| format!("{:.0} %", x.coverage * 100.0));
    out
}

fn distribution_tab(buf: &mut Buffer, inner: Rect, c: &Card, theme: &Theme) {
    let x = inner.x + 1;
    if c.status.is_inactive() {
        put(buf, inner, x, inner.y, &format!("{} {}", theme.status_symbol(&c.status), c.status.word()), theme.status_style(&c.status));
        put(buf, inner, x, inner.y + 1, "Nothing is being sent; not counted as loss. There is no distribution to show.", theme.dim());
        return;
    }
    let Some(stats) = &c.stats else { return };

    // Right: the numbers, for this window and since the session started.
    const TABLE: u16 = 40;
    let tx = inner.right().saturating_sub(TABLE);
    put(buf, inner, tx, inner.y, &format!("{:<12} {:>12} {:>12}", "ms", "window", "since start"), theme.dim());
    for (i, (label, win, all)) in stat_rows(stats, c.session.as_ref()).into_iter().enumerate() {
        let y = inner.y + 1 + i as u16;
        put(buf, inner, tx, y, &format!("{label:<12}"), theme.dim());
        put(buf, inner, tx + 13, y, &format!("{win:>12} {all:>12}"), Style::default());
    }

    // Left: the histogram. Fixed, labelled bins; bars scaled to the fullest.
    let room = tx.saturating_sub(x + 2);
    put(buf, inner, x, inner.y, &format!("{:<13} {}", "bin, ms", "round trips"), theme.dim());
    if c.histogram.is_empty() {
        put(buf, inner, x, inner.y + 2, "No round trips in this window yet.", theme.dim());
        return;
    }
    // An open bin's upper value is a measurement, not an edge.
    let whole = c.histogram.iter().all(|b| b.lo_us % 1000 == 0 && (b.open || b.hi_us % 1000 == 0));
    let edge = |us: u32| if whole { format!("{}", us / 1000) } else { format!("{:.1}", f64::from(us) / 1000.0) };
    let most = c.histogram.iter().map(|b| b.count).max().unwrap_or(1).max(1);
    let bar_w = u32::from(room.saturating_sub(14 + 7)).max(1);
    for (i, b) in c.histogram.iter().enumerate() {
        let y = inner.y + 1 + i as u16;
        // The open last bin holds everything above the binned range.
        let label = match (b.open, theme.glyphs) {
            (false, _) => format!("{}–{}", edge(b.lo_us), edge(b.hi_us)),
            (true, Glyphs::Unicode) => format!("≥ {}", edge(b.lo_us)),
            (true, Glyphs::Ascii) => format!(">= {}", edge(b.lo_us)),
        };
        put(buf, inner, x, y, &format!("{label:>13}"), theme.dim());
        // Eighths of a cell, so a bin with one sample is still visible.
        let eighths = (u64::from(b.count) * u64::from(bar_w) * 8).div_ceil(u64::from(most)) as u32;
        let bar: String = if theme.glyphs == Glyphs::Unicode {
            const PART: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
            let mut s: String = std::iter::repeat_n('█', (eighths / 8) as usize).collect();
            if !eighths.is_multiple_of(8) {
                s.push(PART[(eighths % 8) as usize]);
            }
            s
        } else {
            std::iter::repeat_n('#', eighths.div_ceil(8) as usize).collect()
        };
        put(buf, inner, x + 14, y, &bar, theme.provider(c.slot));
        let count = b.count.to_string();
        put(buf, inner, x + 15 + width(&bar), y, &count, Style::default());
        if b.open {
            put(buf, inner, x + 16 + width(&bar) + width(&count), y, &format!("up to {}", ms(b.hi_us)), theme.dim());
        }
    }
    let mut y = inner.y + 2 + c.histogram.len() as u16;
    let mut notes = vec!["Bars count answered probes only. An unanswered probe has no time, so it is in no bin; see no-reply."];
    if c.histogram.iter().any(|b| b.open) {
        notes.push("The last row is everything above the binned range, so a long tail cannot stretch the bins.");
    }
    notes.push("`—` too few samples to show · `·` low sample count.");
    for note in notes {
        for line in widgets::wrap(note, usize::from(room)) {
            put(buf, inner, x, y, &line, theme.dim());
            y += 1;
        }
    }
}

fn dns_lines(snap: &Snapshot, c: &Card, theme: &Theme, room: usize) -> Vec<(String, Style)> {
    let mut out = Vec::new();
    let views: Vec<_> = snap.dns.iter().filter(|v| v.provider == c.key).collect();
    if views.is_empty() {
        out.push(("No DNS stream of this provider is collecting, so there is nothing to show here.".to_string(), theme.dim()));
        return out;
    }
    for v in views {
        out.push((v.label.clone(), theme.bold()));
        out.push((format!("  last  {}", v.last.as_deref().unwrap_or("nothing finalised yet")), Style::default()));
        out.push((format!("  {:<28} {:<6} {:<10} {:>5}  {}", "query", "type", "expect", "sent", "result × count"), theme.dim()));
        for q in &v.queries {
            let results = q.results.iter().map(|(label, n)| format!("{label} × {n}")).collect::<Vec<_>>().join(" · ");
            let name: String = q.qname.chars().take(28).collect();
            out.push((format!("  {name:<28} {:<6} {:<10} {:>5}  {results}", q.qtype, q.expect, q.sent), Style::default()));
        }
        out.push((String::new(), Style::default()));
    }
    for note in [
        "A result is what the resolver returned (its response code) or, with no response, what happened instead.",
        "DNS times are cache-influenced: they describe the resolver's answer, not the network path alone.",
    ] {
        out.extend(widgets::wrap(note, room).into_iter().map(|l| (l, theme.dim())));
    }
    out
}

fn raw_head() -> String {
    format!("{:<12} {:>7}  {:<16} {:>9} {:>9} {:>9}  {}", "sent, UTC", "seq", "outcome", "rtt ms", "rx wait", "sched lag", "notes")
}

fn raw_lines(c: &Card, theme: &Theme) -> Vec<(String, Style)> {
    let ms3 = |us: u64| format!("{:.3}", us as f64 / 1000.0);
    c.raw
        .iter()
        .map(|r| {
            let style = match r.outcome {
                Outcome::Success => Style::default(),
                Outcome::Timeout | Outcome::Unreachable => theme.status_style(&Status::Lossy),
                Outcome::ServiceError | Outcome::ProtocolError => theme.status_style(&Status::Errors),
                Outcome::LocalError => theme.status_style(&Status::LocalErr),
                Outcome::Skipped | Outcome::Cancelled => theme.dim(),
            };
            let text = format!(
                "{:<12} {:>7}  {:<16} {:>9} {:>9} {:>9}  {}",
                r.utc.get(11..23).unwrap_or("—"),
                r.seq,
                outcome_word(r.outcome),
                r.rtt_us.map_or("—".into(), |v| ms3(u64::from(v))),
                r.recv_delay_us.map_or("—".into(), |v| ms3(u64::from(v))),
                r.sched_lag_us.map_or("—".into(), ms3),
                r.note
            );
            (text, style)
        })
        .collect()
}

/// One event per entry, long ones continued on indented lines so that the
/// whole text can be read here even when the overview had to cut it.
fn event_lines(snap: &Snapshot, c: &Card, theme: &Theme, room: usize) -> Vec<(String, Style)> {
    const INDENT: usize = 37;
    snap.events
        .iter()
        .filter(|e| e.providers.is_empty() || e.providers.contains(&c.key))
        .flat_map(|e| {
            let style = match e.kind {
                '▲' => theme.status_style(&Status::Lossy),
                '!' => theme.status_style(&Status::LocalErr),
                '╌' => theme.dim(),
                _ => Style::default(),
            };
            let mut text = widgets::wrap(&e.text, room.saturating_sub(INDENT).max(20)).into_iter();
            let first = format!("{}  {} {:<24} {}", e.utc.get(11..19).unwrap_or("--:--:--"), theme.event_symbol(e.kind), e.stream, text.next().unwrap_or_default());
            std::iter::once((first, style)).chain(text.map(move |l| (format!("{:INDENT$}{l}", ""), style))).collect::<Vec<_>>()
        })
        .collect()
}

fn help(buf: &mut Buffer, area: Rect, theme: &Theme, mouse: bool) {
    let mouse_lines: &[(&str, &str)] = if mouse { MOUSE } else { &[] };
    let h = ((KEYMAP.len() + mouse_lines.len()) as u16 + 4).min(area.height);
    let w = 80.min(area.width);
    let rect = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    for y in rect.y..rect.bottom() {
        put(buf, rect, rect.x, y, &" ".repeat(usize::from(w)), Style::default());
    }
    let inner = panel(buf, rect, "Keys", "? what the numbers mean · Esc close", theme, theme.border(true), theme.bold());
    for (i, (keys, what)) in KEYMAP.iter().chain(mouse_lines).enumerate() {
        let y = inner.y + 1 + i as u16;
        put(buf, inner, inner.x + 2, y, keys, theme.bold());
        put(buf, inner, inner.x + 18, y, what, Style::default());
    }
}

/// The second page of help: every word and number on screen, one line each,
/// from the same table the documentation is made from. It is longer than a
/// small terminal, so it scrolls; returns the largest useful offset.
fn glossary(buf: &mut Buffer, area: Rect, theme: &Theme, scroll: usize) -> usize {
    use pathwatch_core::glossary::{Group, terms};
    use pathwatch_core::snapshot::Cell;
    // (label, text, heading)
    let mut lines: Vec<(String, String, bool)> = Vec::new();
    let all = terms();
    for group in Group::ALL {
        if !lines.is_empty() {
            lines.push((String::new(), String::new(), false));
        }
        lines.push((group.title().to_string(), String::new(), true));
        for t in all.iter().filter(|t| t.group == group) {
            // The outcome marks are drawn as this terminal draws them.
            let short = if t.label == "outcome strip" {
                let mark = |c: Cell| theme.cell(c).0;
                format!("{} answered  {} some unanswered  {} none answered", mark(Cell::Replied), mark(Cell::SomeUnanswered), mark(Cell::NoneAnswered))
            } else {
                t.short.clone()
            };
            lines.push((t.label.to_string(), short, false));
        }
    }
    let h = (lines.len() as u16 + 4).min(area.height);
    let w = 80.min(area.width);
    let rect = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    for y in rect.y..rect.bottom() {
        put(buf, rect, rect.x, y, &" ".repeat(usize::from(w)), Style::default());
    }
    let rows = usize::from(h.saturating_sub(4));
    let max_scroll = lines.len().saturating_sub(rows);
    let first = scroll.min(max_scroll);
    let hint = if max_scroll == 0 { "? or Esc to close".to_string() } else { format!("{}–{} of {} · j k scroll · ? or Esc close", first + 1, first + rows, lines.len()) };
    let inner = panel(buf, rect, "What the words and numbers mean", &hint, theme, theme.border(true), theme.bold());
    for (i, (label, text, heading)) in lines.iter().skip(first).take(rows).enumerate() {
        let y = inner.y + 1 + i as u16;
        if *heading {
            put(buf, inner, inner.x + 1, y, label, theme.bold());
        } else {
            put(buf, inner, inner.x + 2, y, label, theme.bold());
            put(buf, inner, inner.x + 18, y, text, Style::default());
        }
    }
    max_scroll
}

/// ASCII mode's guarantee: after everything is drawn, no cell holds a
/// character outside ASCII. Punctuation used in labels is transliterated.
fn ascii_only(buf: &mut Buffer, area: Rect) {
    let area = area.intersection(buf.area);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let cell = &mut buf[(x, y)];
            let Some(ch) = cell.symbol().chars().next() else { continue };
            if ch.is_ascii() {
                continue;
            }
            cell.set_char(match ch {
                '·' => '.',
                '—' | '–' | '─' | '╌' | '┄' => '-',
                '│' | '┆' => '|',
                '×' => 'x',
                '÷' => '/',
                'Δ' => 'd',
                '≈' => '~',
                '▸' | '→' | '↳' => '>',
                '▲' => '^',
                '…' => '~',
                '┴' => '^',
                '←' => '<',
                '↔' => '-',
                _ => '?',
            });
        }
    }
}
