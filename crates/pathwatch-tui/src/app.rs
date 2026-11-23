//! Interaction state, the keyboard map, the optional mouse, and the event loop.
//!
//! The loop only reads snapshots and sends back what the user wants to look
//! at. It owns no timers that affect measurement and performs no network work.

use std::io;
use std::sync::Arc;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use futures_util::StreamExt;
use pathwatch_core::collector::PauseSwitch;
use pathwatch_core::model::Family;
use pathwatch_core::snapshot::{Snapshot, ViewSpec, WINDOWS};
use pathwatch_core::trace::{Request as TraceRequest, State as TraceState, TraceView};
use ratatui::DefaultTerminal;
use ratatui::backend::Backend;
use std::net::IpAddr;
use tokio::sync::watch;

use crate::theme::Theme;
use crate::ui::{self, Chrome, Frame, Target};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Overview,
    Details,
    /// The on-demand route trace for one provider.
    Route,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Streams,
    Distribution,
    Dns,
    Raw,
    Events,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Streams, Tab::Distribution, Tab::Dns, Tab::Raw, Tab::Events];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Streams => "Streams",
            Tab::Distribution => "Distribution",
            Tab::Dns => "DNS",
            Tab::Raw => "Raw",
            Tab::Events => "Events",
        }
    }

    fn step(self, by: i32) -> Self {
        let i = Self::ALL.iter().position(|t| *t == self).unwrap_or(0) as i32;
        Self::ALL[(i + by).rem_euclid(Self::ALL.len() as i32) as usize]
    }
}

/// Every binding the dashboard has. The help overlay is generated from this
/// table, so it cannot drift from what the keys do.
pub const KEYMAP: &[(&str, &str)] = &[
    ("1 2 3 4", "focus that provider"),
    ("h l  ← →  Tab", "previous / next provider; in details, previous / next tab"),
    ("Enter", "details for the focused provider"),
    ("j k  ↓ ↑  g G", "in details: scroll a list; top / bottom"),
    ("Esc", "back; closes help, the cursor, or a prompt first"),
    ("m  M", "next / previous measurement mode"),
    ("f", "switch address family, IPv4 ↔ IPv6"),
    ("+  -", "longer / shorter time window"),
    ("Space", "freeze / unfreeze the view (collection continues)"),
    ("c", "chart cursor: h l move it, H L by ten; values in the footer"),
    ("a", "compact layout: all four series ↔ focused only"),
    ("P", "stop / resume collection (asks first; recorded as a gap)"),
    ("R", "reload the configuration; unchanged streams keep running"),
    ("t", "route trace to the focused provider: Enter starts, t stops"),
    ("Ctrl-Z", "suspend to the shell; fg resumes"),
    ("Ctrl-L", "redraw"),
    ("?", "this help; again for what the words and numbers mean"),
    ("q  Ctrl-C", "quit and restore the terminal"),
];

/// What the mouse does when it is captured. Shown in the help panel then,
/// under the keys. Everything here can also be done from the keyboard.
pub const MOUSE: &[(&str, &str)] = &[
    ("click", "a card, table row or legend name: focus that provider"),
    ("click", "the chart or outcomes: put the cursor there; a tab: open it"),
    ("wheel", "on the chart: shorter / longer window; on a list: scroll"),
];

/// How many rows one notch of the wheel scrolls a list.
const WHEEL_ROWS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Continue,
    Redraw,
    Quit,
    Suspend,
    /// Stop (true) or resume (false) collection.
    Pause(bool),
    /// Read the configuration file again.
    Reload,
    /// Start or stop a route trace.
    Trace(TraceRequest),
}

#[derive(Debug, Clone)]
pub struct App {
    pub view: ViewSpec,
    pub focus: u8,
    pub screen: Screen,
    pub tab: Tab,
    /// First visible row of the list in the current tab.
    pub scroll: usize,
    pub help: bool,
    /// With help open: its second page, what the words and numbers mean.
    pub glossary: bool,
    /// First visible line of that page.
    pub help_scroll: usize,
    /// Compact layout: draw only the focused series.
    pub only_focus: bool,
    /// Chart cursor, as a column offset from the right edge (0 is "now").
    pub cursor: Option<u16>,
    /// Waiting for the user to confirm stopping collection.
    pub confirm_stop: bool,
    /// Collection has been stopped by the user.
    pub paused: bool,
    /// A one-line message for the footer, cleared by the next key.
    pub note: Option<&'static str>,
    /// The address the Route view is about.
    pub trace_target: Option<IpAddr>,
    /// The latest trace, as the trace service last published it.
    pub trace: Option<Arc<TraceView>>,
    /// Show reverse-DNS names beside addresses in the Route view.
    pub names: bool,
}

impl App {
    fn scroll_help(&mut self, to: usize) -> Step {
        self.help_scroll = to;
        Step::Continue
    }

    pub fn new(view: ViewSpec) -> Self {
        Self {
            view,
            focus: 1,
            screen: Screen::Overview,
            tab: Tab::Streams,
            scroll: 0,
            help: false,
            glossary: false,
            help_scroll: 0,
            only_focus: true,
            cursor: None,
            confirm_stop: false,
            paused: false,
            note: None,
            trace_target: None,
            trace: None,
            names: true,
        }
    }

    fn move_focus(&mut self, by: i8, slots: u8) {
        let n = i16::from(slots.max(1));
        self.focus = ((i16::from(self.focus) - 1 + i16::from(by)).rem_euclid(n) + 1) as u8;
        self.scroll = 0;
    }

    /// Whether the trace shown is still sending.
    pub fn trace_running(&self) -> bool {
        self.trace.as_ref().is_some_and(|t| t.state == TraceState::Running)
    }

    fn move_cursor(&mut self, by: i32) {
        if let Some(c) = self.cursor {
            let max = i32::from(self.view.columns.max(1)) - 1;
            // Left is back in time, so it increases the offset from "now".
            self.cursor = Some((i32::from(c) - by).clamp(0, max) as u16);
        }
    }

    /// Applies a key press. `snap` supplies the live time for freezing;
    /// `can_pause` says whether there is a collector to stop (not in a demo or a replay).
    pub fn on_key(&mut self, key: KeyEvent, snap: &Snapshot, can_pause: bool) -> Step {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let slots = snap.cards.len() as u8;
        self.note = None;

        // A pending confirmation takes the next key, whatever it is.
        if self.confirm_stop {
            self.confirm_stop = false;
            return match key.code {
                KeyCode::Char('y' | 'Y') => {
                    self.paused = true;
                    Step::Pause(true)
                }
                _ => Step::Continue,
            };
        }

        // The glossary is longer than a small terminal: while it is open
        // the list keys scroll it, not what lies underneath.
        if self.help && self.glossary && !ctrl {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => return self.scroll_help(self.help_scroll.saturating_add(1)),
                KeyCode::Char('k') | KeyCode::Up => return self.scroll_help(self.help_scroll.saturating_sub(1)),
                KeyCode::PageDown => return self.scroll_help(self.help_scroll.saturating_add(10)),
                KeyCode::PageUp => return self.scroll_help(self.help_scroll.saturating_sub(10)),
                KeyCode::Char('g') => return self.scroll_help(0),
                KeyCode::Char('G') => return self.scroll_help(usize::MAX / 2),
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char('c') if ctrl => return Step::Quit,
            KeyCode::Char('l') if ctrl => return Step::Redraw,
            KeyCode::Char('z') if ctrl => return Step::Suspend,
            KeyCode::Char('q') => return Step::Quit,
            // Closed → the keys → what the words and numbers mean → closed.
            KeyCode::Char('?') => {
                (self.help, self.glossary) = match (self.help, self.glossary) {
                    (false, _) => (true, false),
                    (true, false) => (true, true),
                    (true, true) => (false, false),
                };
                self.help_scroll = 0;
            }
            KeyCode::Esc if self.help => (self.help, self.glossary) = (false, false),
            KeyCode::Esc if self.cursor.is_some() => self.cursor = None,
            // Leaving the Route view stops a trace that is running.
            KeyCode::Esc if self.screen == Screen::Route => {
                self.screen = Screen::Overview;
                self.scroll = 0;
                if self.trace_running() {
                    return Step::Trace(TraceRequest::Stop);
                }
            }
            KeyCode::Esc => self.screen = Screen::Overview,
            // In the Route view: Enter starts a trace, t stops one, n hides
            // or shows names. Keys that move between providers do nothing
            // here — the view is about one address.
            KeyCode::Enter if self.screen == Screen::Route => {
                if let (Some(dest), false) = (self.trace_target, self.trace_running()) {
                    self.scroll = 0;
                    return Step::Trace(TraceRequest::Start(dest));
                }
            }
            KeyCode::Char('t') if self.screen == Screen::Route => {
                if self.trace_running() {
                    return Step::Trace(TraceRequest::Stop);
                }
                self.note = Some("no trace is running: Enter starts one");
            }
            KeyCode::Char('n') if self.screen == Screen::Route => self.names = !self.names,
            KeyCode::Char('1'..='4' | 'h' | 'l' | 'H' | 'L' | 'c' | 'a') | KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab if self.screen == Screen::Route => {}
            // Opening the Route view sends nothing: it shows what a trace would do.
            KeyCode::Char('t') if !can_pause => self.note = Some("route tracing needs a live session: this is playback, and nothing is being sent"),
            KeyCode::Char('t') => match snap.cards.iter().find(|c| c.slot == self.focus).and_then(|c| c.target.parse::<IpAddr>().ok()) {
                Some(dest) => {
                    (self.screen, self.trace_target, self.scroll, self.cursor) = (Screen::Route, Some(dest), 0, None);
                }
                None => self.note = Some("this provider has no address to trace in this mode and family"),
            },
            KeyCode::Enter => {
                self.screen = Screen::Details;
                self.cursor = None;
                self.scroll = 0;
            }
            KeyCode::Char(c @ '1'..='4') if (c as u8 - b'0') <= slots => {
                self.focus = c as u8 - b'0';
                self.scroll = 0;
            }
            // Left/right: the cursor if it is on, tabs in details, else providers.
            KeyCode::Char('h') | KeyCode::Left if self.cursor.is_some() => self.move_cursor(-1),
            KeyCode::Char('l') | KeyCode::Right if self.cursor.is_some() => self.move_cursor(1),
            KeyCode::Char('H') => self.move_cursor(-10),
            KeyCode::Char('L') => self.move_cursor(10),
            KeyCode::Char('h') | KeyCode::Left | KeyCode::BackTab if self.screen == Screen::Details => {
                self.tab = self.tab.step(-1);
                self.scroll = 0;
            }
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Tab if self.screen == Screen::Details => {
                self.tab = self.tab.step(1);
                self.scroll = 0;
            }
            KeyCode::Char('h') | KeyCode::Left | KeyCode::BackTab => self.move_focus(-1, slots),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Tab => self.move_focus(1, slots),
            KeyCode::Char('j') | KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Char('g') => self.scroll = 0,
            KeyCode::Char('G') => self.scroll = usize::MAX / 2,
            KeyCode::Char('m') => self.view.mode = self.view.mode.step(true),
            KeyCode::Char('M') => self.view.mode = self.view.mode.step(false),
            KeyCode::Char('f') => {
                self.view.family = match self.view.family {
                    Family::V4 => Family::V6,
                    Family::V6 => Family::V4,
                }
            }
            KeyCode::Char('+' | '=') => self.step_window(1),
            KeyCode::Char('-') => self.step_window(-1),
            // Freezing pins the view; it never stops collection.
            KeyCode::Char(' ') => self.view.frozen_at_ns = if self.view.frozen_at_ns.is_some() { None } else { Some(snap.live_ns) },
            KeyCode::Char('c') if self.screen == Screen::Overview => self.cursor = if self.cursor.is_some() { None } else { Some(0) },
            KeyCode::Char('a') => self.only_focus = !self.only_focus,
            // Stopping collection is a separate, confirmed action.
            KeyCode::Char('P') if !can_pause => self.note = Some("there is no collection to stop: this is playback, and nothing is being sent"),
            KeyCode::Char('P') if self.paused => {
                self.paused = false;
                return Step::Pause(false);
            }
            KeyCode::Char('P') => self.confirm_stop = true,
            // A reload only has something to act on in a live session.
            KeyCode::Char('R') if !can_pause => self.note = Some("there is no configuration to reload: this is playback, and nothing is being sent"),
            KeyCode::Char('R') => {
                self.note = Some("reloading the configuration: what happened is in the Events list");
                return Step::Reload;
            }
            _ => {}
        }
        Step::Continue
    }

    /// Applies a click or a turn of the wheel. `frame` is the frame on
    /// screen: it says what is under the pointer. Nothing here can do what a
    /// key cannot, and nothing here asks anything of the collector.
    pub fn on_mouse(&mut self, event: MouseEvent, frame: &Frame, snap: &Snapshot) {
        // The help panel and a pending question are answered from the keyboard.
        if self.help || self.confirm_stop {
            return;
        }
        let wheel = match event.kind {
            MouseEventKind::Down(MouseButton::Left) => 0,
            MouseEventKind::ScrollUp => -1,
            MouseEventKind::ScrollDown => 1,
            _ => return,
        };
        let Some(target) = frame.at(event.column, event.row) else { return };
        self.note = None;
        match (target, wheel) {
            // As the digit keys.
            (Target::Provider(slot), 0) if snap.cards.iter().any(|c| c.slot == slot) => {
                self.focus = slot;
                self.scroll = 0;
            }
            // As h and l in details.
            (Target::Tab(tab), 0) if tab != self.tab => {
                self.tab = tab;
                self.scroll = 0;
            }
            // As a.
            (Target::Series, 0) => self.only_focus = !self.only_focus,
            // As c, then h and l: the cursor goes to the column clicked.
            (Target::Time { x, columns }, 0) if self.screen == Screen::Overview && columns > 0 => {
                let column = event.column.saturating_sub(x).min(columns - 1);
                self.cursor = Some((columns - 1 - column).min(self.view.columns.max(1) - 1));
            }
            // As - and +: towards you is a longer window, away a shorter one.
            (Target::Time { .. }, by) if by != 0 => self.step_window(by),
            // As j and k, a few rows at a time.
            (Target::List, -1) => self.scroll = self.scroll.saturating_sub(WHEEL_ROWS),
            (Target::List, 1) => self.scroll = self.scroll.saturating_add(WHEEL_ROWS),
            _ => {}
        }
    }

    fn step_window(&mut self, by: i32) {
        let i = WINDOWS.iter().position(|w| *w == self.view.window).unwrap_or(1) as i32;
        self.view.window = WINDOWS[(i + by).clamp(0, WINDOWS.len() as i32 - 1) as usize];
    }
}

/// Hands the terminal back to the shell and stops this process, as `Ctrl-Z`
/// does for any other program; continues here when the shell resumes it.
fn suspend(terminal: &mut DefaultTerminal, mouse: bool) -> io::Result<()> {
    // The shell gets back a terminal exactly as it left it: cursor included,
    // which leaving the alternate screen does not restore by itself, and the
    // mouse, which it would otherwise go on reporting to a shell.
    if mouse {
        crate::mouse::capture(false)?;
    }
    terminal.show_cursor()?;
    ratatui::restore();
    // SAFETY: raising a signal on our own process has no preconditions. The
    // default action for SIGTSTP stops the process until SIGCONT.
    unsafe { libc::raise(libc::SIGTSTP) };
    // A fresh terminal has drawn nothing, so the next frame is a full one.
    *terminal = ratatui::try_init()?;
    if mouse {
        crate::mouse::capture(true)?;
    }
    Ok(())
}

/// Erases the screen and makes the next frame a complete one. Unlike
/// `Terminal::clear`, this asks the terminal nothing: that call waits for a
/// reply to a cursor-position query, and fails on a terminal that sends none.
fn redraw(terminal: &mut DefaultTerminal) -> io::Result<()> {
    terminal.backend_mut().clear()?;
    terminal.swap_buffers();
    Ok(())
}

/// How the dashboard reaches the route-trace service.
#[derive(Clone)]
pub struct TraceLink {
    pub requests: tokio::sync::mpsc::UnboundedSender<TraceRequest>,
    pub view: watch::Receiver<Option<Arc<TraceView>>>,
}

/// What only a live session can be asked to do. All absent in a demo or a
/// replay.
#[derive(Default)]
pub struct Controls {
    /// Stops and resumes collection.
    pub pause: Option<PauseSwitch>,
    /// Asks for the configuration file to be read again.
    pub reload: Option<std::sync::mpsc::Sender<()>>,
    pub trace: Option<TraceLink>,
}

/// With reduced motion, the least time between two frames drawn for no
/// other reason than that the data changed. A key is answered at once.
pub const CALM_FRAME: std::time::Duration = std::time::Duration::from_millis(250);

/// Until when new data alone should not cause another frame, given when the
/// last one was drawn; `None` once that time has passed, or when motion is
/// not reduced.
fn hold_until(reduced_motion: bool, drawn: tokio::time::Instant, now: tokio::time::Instant) -> Option<tokio::time::Instant> {
    let until = drawn + CALM_FRAME;
    (reduced_motion && now < until).then_some(until)
}

/// Runs the dashboard until the user quits or the snapshot source goes away.
/// The caller initialises and restores the terminal, mouse capture included
/// when `chrome.mouse` says it is on. `controls` is what a live session can
/// additionally be asked for.
pub async fn run(
    terminal: &mut DefaultTerminal,
    theme: Theme,
    chrome: Chrome,
    mut snapshots: watch::Receiver<Arc<Snapshot>>,
    view: watch::Sender<ViewSpec>,
    controls: Controls,
) -> io::Result<()> {
    let Controls { pause, reload, trace } = controls;
    let mut app = App::new(view.borrow().clone());
    let mut events = EventStream::new();
    loop {
        let snap = snapshots.borrow_and_update().clone();
        let size = terminal.size()?;
        // The chart's width decides how many time columns to downsample to.
        let columns = ui::plot_columns_for(size.width, size.height);
        if columns != app.view.columns {
            app.view.columns = columns;
        }
        if *view.borrow() != app.view {
            view.send_replace(app.view.clone());
        }
        // The Route view draws whatever the trace service last published.
        if let Some(link) = &trace {
            app.trace = link.view.borrow().clone();
        }
        let mut frame = Frame::default();
        terminal.draw(|f| {
            let area = f.area();
            frame = ui::draw(f.buffer_mut(), area, &snap, &app, &theme, &chrome);
        })?;
        // `G` asks for "the end"; the screen knows where that is.
        app.scroll = app.scroll.min(frame.max_scroll);
        app.help_scroll = app.help_scroll.min(frame.help_max_scroll);
        let drawn = tokio::time::Instant::now();
        // Wait for a reason to draw again. With reduced motion new data is
        // not one until a quarter of a second has passed; a key always is.
        loop {
            let hold = hold_until(chrome.reduced_motion, drawn, tokio::time::Instant::now());
            tokio::select! {
                changed = snapshots.changed(), if hold.is_none() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                }
                _ = tokio::time::sleep_until(hold.unwrap_or(drawn)), if hold.is_some() => continue,
                event = events.next() => match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => match app.on_key(key, &snap, pause.is_some()) {
                        Step::Quit => return Ok(()),
                        Step::Redraw => redraw(terminal)?,
                        Step::Suspend => suspend(terminal, chrome.mouse)?,
                        Step::Pause(stop) => {
                            if let Some(p) = &pause {
                                p.set(stop);
                            }
                        }
                        Step::Reload => {
                            if let Some(r) = &reload {
                                let _ = r.send(());
                            }
                        }
                        Step::Trace(request) => {
                            if let Some(link) = &trace {
                                let _ = link.requests.send(request);
                            }
                        }
                        Step::Continue => {}
                    },
                    // Only when capture was asked for; what is under the pointer
                    // is whatever the frame on screen says is there.
                    Some(Ok(Event::Mouse(mouse))) if chrome.mouse => app.on_mouse(mouse, &frame, &snap),
                    Some(Ok(_)) => {} // resize and others: redraw on the next pass
                    Some(Err(e)) => return Err(e),
                    None => return Ok(()),
                },
            }
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pathwatch_core::script::{self, Playback};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn snap() -> Snapshot {
        let mut p = Playback::new(script::by_name("healthy").unwrap(), 7);
        p.advance_to(60_000_000_000);
        p.snapshot(&ViewSpec::default(), 60_000_000_000)
    }

    #[test]
    fn stopping_collection_needs_confirmation_and_freezing_does_not_stop_it() {
        let s = snap();
        let mut app = App::new(ViewSpec::default());
        // Space freezes the view and asks nothing of the collector.
        assert_eq!(app.on_key(key(' '), &s, true), Step::Continue);
        assert_eq!(app.view.frozen_at_ns, Some(60_000_000_000));
        assert!(!app.paused);
        // P only asks; any key but y cancels.
        assert_eq!(app.on_key(key('P'), &s, true), Step::Continue);
        assert!(app.confirm_stop && !app.paused);
        assert_eq!(app.on_key(key('n'), &s, true), Step::Continue);
        assert!(!app.confirm_stop && !app.paused);
        // P then y stops; P again resumes without asking.
        app.on_key(key('P'), &s, true);
        assert_eq!(app.on_key(key('y'), &s, true), Step::Pause(true));
        assert!(app.paused);
        assert_eq!(app.on_key(key('P'), &s, true), Step::Pause(false));
        assert!(!app.paused);
        // In a demo there is nothing to stop, and it says so.
        assert_eq!(app.on_key(key('P'), &s, false), Step::Continue);
        assert!(app.note.is_some() && !app.confirm_stop);
    }

    #[test]
    fn left_and_right_mean_the_cursor_tabs_or_providers_by_context() {
        let s = snap();
        let mut app = App::new(ViewSpec { columns: 100, ..ViewSpec::default() });
        app.on_key(key('l'), &s, false);
        assert_eq!(app.focus, 2, "overview: next provider");
        app.on_key(key('c'), &s, false);
        assert_eq!(app.cursor, Some(0), "the cursor starts at now");
        app.on_key(key('h'), &s, false);
        app.on_key(key('H'), &s, false);
        assert_eq!((app.cursor, app.focus), (Some(11), 2), "left moves back in time, not between providers");
        app.on_key(key('l'), &s, false);
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &s, false);
        assert_eq!((app.cursor, app.screen), (None, Screen::Overview), "Esc leaves the cursor first");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &s, false);
        app.on_key(key('l'), &s, false);
        assert_eq!((app.tab, app.focus), (Tab::Distribution, 2), "details: next tab");
        app.on_key(key('j'), &s, false);
        app.on_key(key('3'), &s, false);
        assert_eq!((app.focus, app.scroll), (3, 0), "changing provider resets the scroll position");
    }

    #[test]
    fn reduced_motion_holds_frames_for_new_data_a_quarter_of_a_second_apart() {
        let drawn = tokio::time::Instant::now();
        let after = |ms: u64| drawn + std::time::Duration::from_millis(ms);
        // Ordinarily nothing is held back.
        assert_eq!(hold_until(false, drawn, after(0)), None);
        // Reduced: held until 250 ms after the last frame, and then free.
        assert_eq!(hold_until(true, drawn, after(0)), Some(after(250)));
        assert_eq!(hold_until(true, drawn, after(249)), Some(after(250)));
        assert_eq!(hold_until(true, drawn, after(250)), None);
        assert_eq!(hold_until(true, drawn, after(4_000)), None);
    }

    #[test]
    fn the_help_key_turns_through_its_pages_and_the_list_keys_scroll_the_second() {
        let s = snap();
        let mut app = App::new(ViewSpec { columns: 100, ..ViewSpec::default() });
        app.screen = Screen::Details;
        app.on_key(key('?'), &s, false);
        assert_eq!((app.help, app.glossary), (true, false));
        // On the first page the list keys still belong to the screen underneath.
        app.on_key(key('j'), &s, false);
        assert_eq!((app.scroll, app.help_scroll), (1, 0));
        app.on_key(key('?'), &s, false);
        assert_eq!((app.help, app.glossary), (true, true));
        for c in ['j', 'j', 'j', 'k'] {
            app.on_key(key(c), &s, false);
        }
        assert_eq!((app.scroll, app.help_scroll), (1, 2), "they scroll the page, not the list under it");
        app.on_key(key('G'), &s, false);
        assert!(app.help_scroll > 1_000, "the end: the screen says where that is");
        app.on_key(key('g'), &s, false);
        assert_eq!(app.help_scroll, 0);
        app.on_key(key('j'), &s, false);
        app.on_key(key('?'), &s, false);
        assert_eq!((app.help, app.glossary, app.help_scroll), (false, false, 0), "a third press closes it");
        // Esc closes it from either page.
        app.on_key(key('?'), &s, false);
        app.on_key(key('?'), &s, false);
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &s, false);
        assert_eq!((app.help, app.glossary), (false, false));
        assert_eq!(app.screen, Screen::Details, "and leaves the screen where it was");
    }

    #[test]
    fn every_help_line_fits_the_help_panel() {
        // The panel is 80 wide, 78 inside its border; keys start at column 2
        // and descriptions at column 18, which leaves them 60.
        for (keys, what) in KEYMAP.iter().chain(MOUSE) {
            assert!(keys.chars().count() <= 15, "{keys:?} runs into its description");
            assert!(what.chars().count() <= 60, "{what:?} is {} characters and would be cut off", what.chars().count());
        }
        // And in height: the smallest terminal is 24 rows, of which the panel's
        // border and the blank row under its title leave 21.
        assert!(KEYMAP.len() + MOUSE.len() <= 21, "{} lines for 21 rows", KEYMAP.len() + MOUSE.len());
    }

    #[test]
    fn every_documented_key_is_handled() {
        // The help table names keys; make sure none of the plain ones is a no-op typo.
        let s = snap();
        for c in ['1', '2', '3', '4', 'h', 'l', 'j', 'k', 'g', 'G', 'm', 'M', 'f', '+', '-', ' ', 'c', 'a', '?', 'H', 'L'] {
            let mut app = App::new(ViewSpec { columns: 100, ..ViewSpec::default() });
            app.cursor = Some(20);
            app.scroll = 5;
            let before = format!("{app:?}");
            app.on_key(key(c), &s, true);
            assert_ne!(format!("{app:?}"), before, "key {c:?} changed nothing");
        }
        assert_eq!(App::new(ViewSpec::default()).on_key(key('q'), &s, true), Step::Quit);
        // t opens the Route view for the focused provider's address and sends nothing;
        // Enter starts, t stops, Esc leaves and stops.
        let mut app = App::new(ViewSpec::default());
        assert_eq!(app.on_key(key('t'), &s, true), Step::Continue);
        let target: IpAddr = s.cards[0].target.parse().unwrap();
        assert_eq!((app.screen, app.trace_target), (Screen::Route, Some(target)));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.on_key(enter, &s, true), Step::Trace(TraceRequest::Start(target)));
        let view = |state| {
            let budget = pathwatch_core::trace::Budget { max_hops: 30, rounds: 10, round_interval: std::time::Duration::from_secs(1), max_probes: 300, max_rate: 30 };
            Some(Arc::new(TraceView { dest: target, state, round: 2, budget, probes_sent: 9, hops: Vec::new(), error: None, names: Vec::new() }))
        };
        app.trace = view(TraceState::Running);
        assert_eq!(app.on_key(enter, &s, true), Step::Continue, "one at a time: Enter does not start a second");
        app.on_key(key('2'), &s, true);
        assert_eq!((app.focus, app.screen), (1, Screen::Route), "the view stays on its address");
        assert_eq!(app.on_key(key('t'), &s, true), Step::Trace(TraceRequest::Stop));
        assert_eq!(app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &s, true), Step::Trace(TraceRequest::Stop), "leaving stops it");
        assert_eq!(app.screen, Screen::Overview);
        app.trace = view(TraceState::Finished);
        app.on_key(key('t'), &s, true);
        assert_eq!(app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &s, true), Step::Continue, "nothing to stop");
        // In playback there is nothing to trace with.
        let mut app = App::new(ViewSpec::default());
        assert_eq!(app.on_key(key('t'), &s, false), Step::Continue);
        assert!(app.screen == Screen::Overview && app.note.is_some_and(|n| n.contains("live session")));
        // R reloads a live session and says so; in playback it explains why not.
        let mut app = App::new(ViewSpec::default());
        assert_eq!(app.on_key(key('R'), &s, true), Step::Reload);
        assert!(app.note.is_some_and(|n| n.contains("Events")));
        assert_eq!(app.on_key(key('R'), &s, false), Step::Continue);
        assert!(app.note.is_some_and(|n| n.contains("playback")));
        assert_eq!(App::new(ViewSpec::default()).on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL), &s, true), Step::Suspend);
    }
}
