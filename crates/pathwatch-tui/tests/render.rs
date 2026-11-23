//! Whole-screen rendering tests: a scripted scenario, a terminal size, a
//! colour and glyph mode → text. Deterministic; no terminal and no network.

use std::time::Duration;

use pathwatch_core::script::{self, Playback};
use pathwatch_core::snapshot::{Mode, Snapshot, ViewSpec};
use pathwatch_tui::app::{App, Screen, Tab};
use pathwatch_tui::theme::{ColorMode, Glyphs, Theme};
use pathwatch_tui::ui::{self, Chrome, Session};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

struct Shot {
    scenario: &'static str,
    at_s: u64,
    w: u16,
    h: u16,
    theme: Theme,
    window_s: u64,
    mode: Mode,
    tweak: fn(&mut App),
    /// Changes to the scripted scenario before it is played.
    script: fn(&mut script::Scenario),
    /// As if the mouse had been asked for.
    mouse: bool,
}

impl Shot {
    fn new(scenario: &'static str, at_s: u64, w: u16, h: u16) -> Self {
        Self { scenario, at_s, w, h, theme: Theme { color: ColorMode::True, glyphs: Glyphs::Unicode }, window_s: 60, mode: Mode::Icmp, tweak: |_| {}, script: |_| {}, mouse: false }
    }

    fn buffer(&self) -> Buffer {
        self.frame().0
    }

    /// The frame and the largest useful scroll offset the screen reported.
    fn frame(&self) -> (Buffer, usize) {
        let (buf, frame, _, _) = self.stage(&|app| (self.tweak)(app));
        (buf, frame.max_scroll)
    }

    /// One frame with `change` applied to the app on top of nothing else:
    /// what was drawn, what the screen reported, and the app and snapshot
    /// it was drawn from.
    fn stage(&self, change: &dyn Fn(&mut App)) -> (Buffer, ui::Frame, App, Snapshot) {
        let mut scenario = script::by_name(self.scenario).unwrap();
        (self.script)(&mut scenario);
        let mut play = Playback::new(scenario, 7);
        let now = self.at_s * 1_000_000_000;
        play.advance_to(now);
        let view = ViewSpec { mode: self.mode, window: Duration::from_secs(self.window_s), columns: ui::plot_columns_for(self.w, self.h), ..ViewSpec::default() };
        let mut app = App::new(view.clone());
        change(&mut app);
        let snap = play.snapshot(&app.view, now);
        let chrome = Chrome {
            session: Session::Demo { name: self.scenario.into(), end_ns: play.end_ns() },
            source: "via 192.0.2.10".into(),
            notes: vec!["ICMP: unprivileged datagram socket".into()],
            recording: None,
            trace_budget: Some("ICMP echo, TTL 1 to 30, 10 rounds, one every 1s — at most 300 probes in all and 30 a second".into()),
            mouse: self.mouse,
            reduced_motion: false,
        };
        let area = Rect::new(0, 0, self.w, self.h);
        let mut buf = Buffer::empty(area);
        let frame = ui::draw(&mut buf, area, &snap, &app, &self.theme, &chrome);
        (buf, frame, app, snap)
    }

    fn text(&self) -> String {
        let buf = self.buffer();
        (0..self.h).map(|y| (0..self.w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()).collect::<Vec<_>>().join("\n")
    }
}

fn golden(name: &str, text: &str) {
    let path = format!("{}/tests/golden/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{text}\n")).unwrap();
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{path} missing; run with UPDATE_GOLDEN=1"));
    assert_eq!(format!("{text}\n"), want, "{name} changed; review and rerun with UPDATE_GOLDEN=1 if intended");
}

#[test]
fn standard_layout_during_an_isolated_failure() {
    golden("standard-120x35-isolated-failure", &Shot::new("isolated-failure", 90, 120, 35).text());
}

#[test]
fn large_layout_during_an_isolated_failure() {
    golden("large-160x45-isolated-failure", &Shot::new("isolated-failure", 90, 160, 45).text());
}

#[test]
fn compact_layout_with_unconfigured_providers() {
    golden("compact-80x24-needs-config", &Shot::new("needs-config", 90, 80, 24).text());
}

#[test]
fn ascii_monochrome_standard_layout() {
    let mut s = Shot::new("isolated-failure", 90, 120, 35);
    s.theme = Theme { color: ColorMode::Mono, glyphs: Glyphs::Ascii };
    let text = s.text();
    assert!(text.is_ascii(), "ASCII mode must not emit a single non-ASCII character");
    golden("ascii-mono-120x35-isolated-failure", &text);
}

#[test]
fn details_and_help_screens() {
    let mut s = Shot::new("dns-only", 70, 120, 35);
    s.tweak = |app| app.screen = Screen::Details;
    golden("details-120x35-dns-only", &s.text());
    let mut s = Shot::new("healthy", 60, 120, 35);
    s.tweak = |app| app.help = true;
    golden("help-120x35", &s.text());
}

/// High contrast changes how things are drawn, never what is drawn: the
/// same characters in the same places, with no cell left to the terminal's
/// own colours and none dimmed, on every screen.
#[test]
fn high_contrast_draws_the_same_screen_in_explicit_colours_with_nothing_dimmed() {
    use ratatui::style::{Color, Modifier};
    let screens: [fn(&mut App); 5] = [
        |_| {},
        |app| app.screen = Screen::Details,
        |app| (app.screen, app.tab) = (Screen::Details, Tab::Raw),
        |app| app.help = true,
        |app| (app.help, app.glossary) = (true, true),
    ];
    for scenario in ["isolated-failure", "dns-only", "needs-config"] {
        for (w, h) in [(80, 24), (120, 35), (160, 45)] {
            for tweak in screens {
                let mut usual = Shot::new(scenario, 90, w, h);
                usual.tweak = tweak;
                let mut hc = Shot::new(scenario, 90, w, h);
                (hc.tweak, hc.theme) = (tweak, Theme { color: ColorMode::HighContrast, glyphs: Glyphs::Unicode });
                assert_eq!(hc.text(), usual.text(), "{scenario} at {w}x{h}");
                let buf = hc.buffer();
                let mut colours = std::collections::BTreeSet::new();
                for y in 0..h {
                    for x in 0..w {
                        let cell = &buf[(x, y)];
                        assert!(cell.fg != Color::Reset && cell.bg != Color::Reset, "{scenario} at {w}x{h}: cell {x},{y} {:?} is left to the terminal", cell.symbol());
                        assert!(!cell.modifier.contains(Modifier::DIM), "{scenario} at {w}x{h}: cell {x},{y} {:?} is dimmed", cell.symbol());
                        colours.insert(format!("{:?}", cell.fg));
                    }
                }
                // Only the sixteen colours every colour terminal has.
                assert!(colours.iter().all(|c| !c.starts_with("Rgb") && !c.starts_with("Indexed")), "{colours:?}");
            }
        }
    }
    // The failing provider's badge is a filled ground with its word on it.
    let mut hc = Shot::new("isolated-failure", 90, 120, 35);
    hc.theme = Theme { color: ColorMode::HighContrast, glyphs: Glyphs::Unicode };
    let (buf, text) = (hc.buffer(), hc.text());
    let (row, line) = text.lines().enumerate().find(|(_, l)| l.contains("NO REPLY")).expect("a card that is not answering");
    let col = line.chars().take_while(|_| true).collect::<String>().find("NO REPLY").map(|b| line[..b].chars().count()).unwrap();
    let badge = &buf[(col as u16, row as u16)];
    assert_eq!((badge.fg, badge.bg), (Color::White, Color::Red));
}

fn text_of(buf: &Buffer, w: u16, h: u16) -> String {
    (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string()).collect::<Vec<_>>().join("\n")
}

/// `?` twice: one line for every word and number the screens show, from the
/// table the documentation is made from. Whole on a tall terminal; on a
/// small one it scrolls to its last line and no further.
#[test]
fn the_glossary_page_explains_every_term_and_scrolls_where_it_does_not_fit() {
    use pathwatch_core::glossary::terms;
    let open = |app: &mut App| (app.help, app.glossary) = (true, true);
    let tall = Shot::new("healthy", 60, 120, 60);
    let (buf, frame, _, _) = tall.stage(&open);
    let whole = text_of(&buf, 120, 60);
    assert_eq!(frame.help_max_scroll, 0, "it fits in 60 rows");
    // The panel is 80 columns wide in the middle of 120: its own lines only.
    let panel: Vec<String> = whole.lines().map(|l| l.chars().skip(20).take(80).collect()).collect();
    for t in terms() {
        let line = panel.iter().find(|l| l.starts_with(&format!("│  {} ", t.label))).unwrap_or_else(|| panic!("no line for {:?}:\n{whole}", t.label));
        if t.label != "outcome strip" {
            assert!(line.contains(&t.short), "{:?} is cut off: {line}", t.label);
        }
    }
    golden("glossary-120x60", &whole);

    // 80 × 24: twenty lines at a time. Every term is reachable, the last
    // offset shows the last line, and asking for more changes nothing.
    let small = Shot::new("healthy", 60, 80, 24);
    let (_, frame, _, _) = small.stage(&open);
    let max = frame.help_max_scroll;
    assert!(max > 0);
    let at = |offset: usize| {
        let (buf, frame, _, _) = small.stage(&|app| {
            open(app);
            app.help_scroll = offset;
        });
        assert_eq!(frame.help_max_scroll, max);
        text_of(&buf, 80, 24)
    };
    let every_page: String = (0..=max).map(|offset| at(offset) + "\n").collect();
    for t in terms() {
        assert!(every_page.lines().any(|l| l.starts_with(&format!("│  {} ", t.label))), "{:?} cannot be scrolled to", t.label);
    }
    assert!(at(0).contains("1–20 of ") && at(0).contains("j k scroll") && !at(0).contains("outcome strip"));
    assert!(at(max).contains("outcome strip") && at(max).lines().nth(23).is_some_and(|l| l.starts_with('╰')), "{}", at(max));
    assert_eq!(at(max), at(usize::MAX / 2));
    golden("glossary-80x24", &at(0));

    // ASCII: nothing outside ASCII, and nothing that had to be replaced by
    // a question mark — the outcome marks are the ones this mode draws.
    let mut ascii = Shot::new("healthy", 60, 120, 60);
    ascii.theme = Theme { color: ColorMode::Mono, glyphs: Glyphs::Ascii };
    let (buf, _, _, _) = ascii.stage(&open);
    let plain = text_of(&buf, 120, 60);
    assert!(plain.is_ascii());
    let body: Vec<String> = plain.lines().map(|l| l.chars().skip(20).take(80).collect::<String>()).filter(|l| l.starts_with("|  ")).collect();
    assert!(body.len() > 30 && body.iter().all(|l| !l.contains('?')), "{plain}");
    assert!(plain.contains(". answered  o some unanswered  # none answered"), "{plain}");
}

/// A DNS/TCP stream probes once a minute. It is listed with the others
/// under its own name, counts what it sent, and says it is too young to
/// judge rather than showing a rate from three probes.
#[test]
fn a_dns_tcp_stream_is_listed_with_its_few_samples_shown_as_few() {
    use pathwatch_core::model::{Family, Protocol};
    let mut s = Shot::new("healthy", 170, 120, 35);
    s.window_s = 300;
    s.script = |sc| {
        let mut tcp = script::stream("cloudflare", "resolver-v4", Protocol::DnsTcp, Family::V4, "1.1.1.1", 30, 900, 400);
        tcp.info.interval = Duration::from_secs(60);
        sc.streams.push(tcp);
    };
    s.tweak = |app| app.screen = Screen::Details;
    let text = s.text();
    let lines: Vec<&str> = text.lines().collect();
    let at = lines.iter().position(|l| l.contains("dns-tcp v4")).unwrap_or_else(|| panic!("no DNS/TCP row:\n{text}"));
    let (row, counts) = (lines[at], lines[at + 1]);
    assert!(row.contains("1.1.1.1") && row.contains("WARMING UP"), "{row}");
    assert!(counts.contains("sent 3 · pending 0 · timed out 0"), "{counts}");
    // Three answers are shown as a last value and as counts: no median, no
    // 95th percentile, and "0 / 3" where a percentage would overstate it.
    let cells: Vec<&str> = row.split_whitespace().collect();
    assert_eq!(cells.iter().filter(|c| **c == "—").count(), 2, "{row}");
    assert_eq!(row.matches("0 / 3").count(), 2, "{row}");
    assert!(text.contains("A fresh or dns-tcp stream connects anew for each probe; its time includes that."), "{text}");
}

#[test]
fn every_scenario_renders_at_every_size_without_panicking_or_overflowing() {
    for sc in script::all() {
        for (w, h) in [(80, 24), (100, 30), (120, 35), (140, 40), (160, 45), (220, 60), (79, 24), (80, 23), (1, 1)] {
            for (color, glyphs) in [(ColorMode::True, Glyphs::Unicode), (ColorMode::C16, Glyphs::Unicode), (ColorMode::Mono, Glyphs::Unicode), (ColorMode::Mono, Glyphs::Ascii)] {
                for mode in Mode::ALL {
                    let mut s = Shot::new(sc.name, 150, w, h);
                    s.theme = Theme { color, glyphs };
                    s.mode = mode;
                    let text = s.text();
                    assert!(text.lines().count() <= usize::from(h));
                    if (w < 80 || h < 24) && w >= 60 {
                        assert!(text.contains("needs at least 80"), "{}: {w}x{h} must ask for a larger window instead of clipping", sc.name);
                        assert_eq!(text.trim_end().lines().count(), 1, "and draw nothing else");
                    }
                }
            }
        }
    }
}

#[test]
fn inactive_cards_never_show_a_percentage_and_statuses_stay_distinct() {
    for (color, glyphs) in [(ColorMode::True, Glyphs::Unicode), (ColorMode::Mono, Glyphs::Ascii)] {
        let mut s = Shot::new("needs-config", 120, 120, 35);
        s.theme = Theme { color, glyphs };
        let text = s.text();
        let cards: Vec<&str> = text.lines().skip(2).take(9).collect();
        // Columns 60..120 hold the NextDNS and Arelion cards.
        for line in &cards {
            let right: String = line.chars().skip(60).collect();
            assert!(!right.contains('%'), "an unconfigured card shows no rate: {right}");
        }
        assert_eq!(text.matches("NEEDS CONFIG").count(), 2);
        assert!(text.contains("Nothing is being sent") && text.contains("counted as loss"), "the card says nothing is sent and that it is not loss");
        assert!(text.contains("needs configuration"));

        let mut s = Shot::new("isolated-failure", 90, 120, 35);
        s.theme = Theme { color, glyphs };
        let text = s.text();
        assert_eq!(text.matches("NO REPLY").count(), 1, "exactly the failing target");
        assert_eq!(text.matches(" OK ").count() + text.matches(" OK\n").count(), 3);
    }
}

#[test]
fn the_chart_draws_nothing_where_there_was_no_reply() {
    let s = Shot::new("isolated-failure", 90, 120, 35);
    let buf = s.buffer();
    // Find the Arelion row of the timeline and the columns where no probe was answered.
    let text = s.text();
    let lines: Vec<&str> = text.lines().collect();
    let row = lines.iter().position(|l| l.contains(" 4 AR ")).expect("timeline row for Arelion");
    let dead: Vec<u16> = lines[row].chars().enumerate().filter(|(_, c)| *c == '█').map(|(i, _)| i as u16).collect();
    assert!(dead.len() > 40, "the outage is visible on the timeline: {}", dead.len());
    // In the chart above, those columns must hold nothing in Arelion's colour.
    let arelion = Theme { color: ColorMode::True, glyphs: Glyphs::Unicode }.provider(4).fg;
    let chart_top = lines.iter().position(|l| l.contains("Latency ·")).unwrap() as u16 + 1;
    let chart_bottom = lines.iter().position(|l| l.contains("Outcomes ·")).unwrap() as u16 - 3;
    for &x in &dead[2..dead.len() - 2] {
        for y in chart_top..chart_bottom {
            let cell = &buf[(x, y)];
            assert!(cell.symbol() == " " || cell.fg != arelion.unwrap(), "column {x} row {y} draws Arelion latency during the outage: {:?}", cell.symbol());
        }
    }
}

#[test]
fn rendering_is_a_pure_function_of_the_snapshot() {
    let a = Shot::new("flapping", 150, 160, 45).text();
    let b = Shot::new("flapping", 150, 160, 45).text();
    assert_eq!(a, b);
}

#[test]
fn details_tabs() {
    let mut s = Shot::new("correlated-spike", 100, 120, 35);
    s.tweak = |app| (app.screen, app.tab) = (Screen::Details, Tab::Distribution);
    golden("distribution-120x35-correlated-spike", &s.text());
    let mut s = Shot::new("isolated-failure", 90, 80, 24);
    s.tweak = |app| (app.screen, app.tab, app.focus) = (Screen::Details, Tab::Distribution, 2);
    golden("distribution-80x24-isolated-failure", &s.text());
    let mut s = Shot::new("dns-only", 70, 120, 35);
    s.tweak = |app| (app.screen, app.tab) = (Screen::Details, Tab::Dns);
    golden("dns-120x35-dns-only", &s.text());
    let mut s = Shot::new("isolated-failure", 90, 120, 35);
    s.tweak = |app| (app.screen, app.tab, app.focus) = (Screen::Details, Tab::Raw, 4);
    golden("raw-120x35-isolated-failure", &s.text());
    let mut s = Shot::new("flapping", 150, 120, 35);
    s.tweak = |app| (app.screen, app.tab, app.focus) = (Screen::Details, Tab::Events, 4);
    golden("events-120x35-flapping", &s.text());
}

#[test]
fn every_tab_renders_everywhere_and_scrolling_stays_in_range() {
    for sc in script::all() {
        for (w, h) in [(80, 24), (120, 35), (160, 45)] {
            for (color, glyphs) in [(ColorMode::True, Glyphs::Unicode), (ColorMode::Mono, Glyphs::Ascii)] {
                for mode in Mode::ALL {
                    for tab in Tab::ALL {
                        for focus in [1, 3] {
                            let mut s = Shot::new(sc.name, 150, w, h);
                            (s.theme, s.mode) = (Theme { color, glyphs }, mode);
                            // `tweak` is a plain function, so the cases are spelled out.
                            s.tweak = match (tab, focus) {
                                (Tab::Streams, 1) => |a| (a.screen, a.tab, a.focus) = (Screen::Details, Tab::Streams, 1),
                                (Tab::Streams, _) => |a| (a.screen, a.tab, a.focus) = (Screen::Details, Tab::Streams, 3),
                                (Tab::Distribution, 1) => |a| (a.screen, a.tab, a.focus) = (Screen::Details, Tab::Distribution, 1),
                                (Tab::Distribution, _) => |a| (a.screen, a.tab, a.focus) = (Screen::Details, Tab::Distribution, 3),
                                (Tab::Dns, 1) => |a| (a.screen, a.tab, a.focus, a.scroll) = (Screen::Details, Tab::Dns, 1, usize::MAX / 2),
                                (Tab::Dns, _) => |a| (a.screen, a.tab, a.focus) = (Screen::Details, Tab::Dns, 3),
                                (Tab::Raw, 1) => |a| (a.screen, a.tab, a.focus, a.scroll) = (Screen::Details, Tab::Raw, 1, usize::MAX / 2),
                                (Tab::Raw, _) => |a| (a.screen, a.tab, a.focus) = (Screen::Details, Tab::Raw, 3),
                                (Tab::Events, 1) => |a| (a.screen, a.tab, a.focus, a.scroll) = (Screen::Details, Tab::Events, 1, usize::MAX / 2),
                                (Tab::Events, _) => |a| (a.screen, a.tab, a.focus) = (Screen::Details, Tab::Events, 3),
                            };
                            let (text, (_, max_scroll)) = (s.text(), s.frame());
                            let what = format!("{} {w}x{h} {mode:?} {tab:?} focus {focus}", sc.name);
                            assert!(text.contains(&format!("[{}]", tab.label())), "{what}: the current tab is marked");
                            assert!(glyphs == Glyphs::Unicode || text.is_ascii(), "{what}: non-ASCII output in ASCII mode");
                            assert!(max_scroll <= 200, "{what}: scroll range {max_scroll}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn a_long_list_scrolls_to_its_last_row_and_no_further() {
    let shot = |scroll: fn(&mut App)| {
        let mut s = Shot::new("isolated-failure", 150, 120, 35);
        s.tweak = scroll;
        (s.text(), s.frame().1)
    };
    let (top, max) = shot(|a| (a.screen, a.tab) = (Screen::Details, Tab::Raw));
    assert!(max > 50, "150 s of one-second probes do not fit on one screen: {max}");
    assert!(top.contains("rows 1–"), "{top}");
    let (end, _) = shot(|a| (a.screen, a.tab, a.scroll) = (Screen::Details, Tab::Raw, usize::MAX / 2));
    let total = max + top.lines().filter(|l| l.contains(" success ")).count();
    assert!(end.contains(&format!("–{total} of {total}")), "the last row is reachable and labelled: want {total}\n{end}");
    assert_ne!(top, end);
}

#[test]
fn the_cursor_reads_out_exactly_what_the_chart_column_holds() {
    // Arelion is silent from 60 s to 100 s. At 90 s with a 60 s window, a
    // cursor 20 columns back is inside the outage; the others have data there.
    let mut s = Shot::new("isolated-failure", 90, 120, 35);
    s.tweak = |app| app.cursor = Some(20);
    let text = s.text();
    let footer = text.lines().last().unwrap();
    assert!(footer.contains("cursor -") && footer.contains("AR no reply"), "{footer}");
    for tag in ["CF ", "GO ", "ND "] {
        let value = footer.split(tag).nth(1).and_then(|r| r.split([' ', '·']).next()).unwrap_or("");
        assert!(value.parse::<f64>().is_ok_and(|v| v > 1.0 && v < 200.0), "{tag}has a round-trip time at the cursor: {footer}");
    }
    assert!(!footer.contains("AR 0"), "a missing value is never shown as zero: {footer}");
    golden("cursor-120x35-isolated-failure", &text);

    // The cursor rule only ever occupies empty cells: every data cell is unchanged.
    let (with, without) = (s.buffer(), Shot::new("isolated-failure", 90, 120, 35).buffer());
    let chart_top = text.lines().position(|l| l.contains("Latency ·")).unwrap() as u16 + 1;
    let chart_bottom = text.lines().position(|l| l.contains("Outcomes ·")).unwrap() as u16 - 3;
    let mut ruled = 0;
    for y in chart_top..chart_bottom {
        for x in 0..120 {
            let (a, b) = (with[(x, y)].symbol(), without[(x, y)].symbol());
            if a != b {
                assert_eq!((a, b), ("┆", " "), "({x},{y})");
                ruled += 1;
            }
        }
    }
    assert!(ruled > 3, "the cursor is drawn: {ruled}");

    // ASCII and compact layouts say the same thing.
    let mut s = Shot::new("isolated-failure", 90, 80, 24);
    s.theme = Theme { color: ColorMode::Mono, glyphs: Glyphs::Ascii };
    s.tweak = |app| (app.cursor, app.focus) = (Some(15), 4);
    let text = s.text();
    assert!(text.is_ascii() && text.lines().last().unwrap().contains("AR no reply"), "{text}");
}

#[test]
fn stopping_is_asked_about_and_then_stated_and_is_not_the_same_as_frozen() {
    let mut s = Shot::new("healthy", 60, 120, 35);
    s.tweak = |app| app.confirm_stop = true;
    let ask = s.text();
    assert!(ask.lines().last().unwrap().contains("Stop collection?") && ask.contains("recorded as a gap"), "{ask}");
    let mut s = Shot::new("healthy", 60, 80, 24);
    s.tweak = |app| app.confirm_stop = true;
    let footer = s.text().lines().last().unwrap().to_string();
    assert!(footer.contains("Stop collection?") && footer.ends_with("cancels"), "the whole question fits at 80 columns: {footer}");

    s.tweak = |app| app.paused = true;
    assert!(s.text().contains("STOPPED · P resumes"));
    let mut s = Shot::new("healthy", 60, 120, 35);
    s.tweak = |app| (app.paused, app.view.frozen_at_ns) = (true, Some(50_000_000_000));
    let text = s.text();
    assert!(text.contains("nothing is being sent") && text.contains("P resumes"), "{text}");
    assert!(!text.contains("collection continues"), "a stopped session must not claim that collection continues");
    s.tweak = |app| app.view.frozen_at_ns = Some(50_000_000_000);
    assert!(s.text().contains("playback continues"), "a demo collects nothing, so a frozen demo does not say that collection continues");
}

#[test]
fn the_footer_is_never_cut_off() {
    // With the longest state word and the longest provider name, at the
    // narrowest width of each layout, the last key shown is still `q`.
    for (w, h) in [(80, 24), (120, 35), (160, 45)] {
        for (name, tweak) in [
            ("overview", (|_| {}) as fn(&mut App)),
            ("details", |a| a.screen = Screen::Details),
            ("stopped", |a| a.paused = true),
            ("frozen", |a| a.view.frozen_at_ns = Some(50_000_000_000)),
        ] {
            let mut s = Shot::new("healthy", 60, w, h);
            s.tweak = tweak;
            let text = s.text();
            let footer = text.lines().last().unwrap();
            assert!(footer.ends_with("· q"), "{name} at {w}x{h}: {footer}");
        }
    }
}

/// One reply 150 ms slower than the rest, 20 s before the screen is taken.
fn one_slow_reply(sc: &mut script::Scenario) {
    use script::{Behaviour, Rule};
    sc.streams[0].rules.push(Rule { from: Duration::from_secs(70), to: Duration::from_millis(70_900), what: Behaviour::Slower(150_000) });
}

#[test]
fn one_slow_reply_is_marked_off_the_scale_instead_of_flattening_the_chart() {
    let chart = |text: &str| -> Vec<String> {
        let lines: Vec<&str> = text.lines().collect();
        let top = lines.iter().position(|l| l.contains("Latency ·")).unwrap();
        let bottom = lines.iter().position(|l| l.contains("Outcomes ·")).unwrap();
        lines[top..bottom].iter().map(|l| l.to_string()).collect()
    };
    let plain = Shot::new("healthy", 90, 120, 35);
    let mut spiked = Shot::new("healthy", 90, 120, 35);
    spiked.script = one_slow_reply;
    let (before, after) = (chart(&plain.text()), chart(&spiked.text()));

    // The axis is the same with and without the slow reply.
    let axis = |c: &[String]| c[1..].iter().map(|l| l.chars().take(6).collect::<String>()).collect::<Vec<_>>();
    assert_eq!(axis(&before), axis(&after), "the scale did not move");
    assert!(before[0].contains("common scale, ms") && !before.iter().any(|l| l.contains('▲')), "nothing is marked when nothing is off the scale");
    // The title says what the mark means and gives the real value.
    assert!(after[0].contains("▲ off scale, max 16"), "{}", after[0]);
    // The mark is on the top row, across the one time column of the slow
    // reply (a column is two cells wide here: 60 probes over 108 cells).
    let marks: Vec<(usize, usize)> = after.iter().enumerate().skip(1).flat_map(|(y, l)| l.chars().enumerate().filter(|(_, c)| *c == '▲').map(move |(x, _)| (x, y))).collect();
    assert!((1..=2).contains(&marks.len()) && marks.iter().all(|m| m.1 == 1), "{marks:?}");
    assert!(marks.windows(2).all(|w| w[1].0 == w[0].0 + 1), "one column, not two: {marks:?}");
    // Nothing else on the chart moved: only that column, and the cell to its
    // left, where the line rises to meet it.
    let (first, last) = (marks[0].0, marks[marks.len() - 1].0);
    let elsewhere = (1..before.len())
        .flat_map(|y| before[y].chars().zip(after[y].chars()).enumerate().filter(|(_, (a, b))| a != b).map(move |(x, _)| x))
        .filter(|x| *x + 1 < first || *x > last)
        .count();
    assert_eq!(elsewhere, 0, "cells changed away from the slow reply's column");
    golden("off-scale-120x35-one-slow-reply", &spiked.text());

    // The cursor on that column gives the value exactly.
    let cols = usize::from(ui::plot_columns_for(120, 35));
    assert_eq!(cols, 108);
    let mut at_cursor = Shot::new("healthy", 90, 120, 35);
    at_cursor.script = one_slow_reply;
    at_cursor.tweak = |app| app.cursor = Some(35);
    let text = at_cursor.text();
    let footer = text.lines().last().unwrap();
    assert!(footer.contains("CF 16"), "the cursor reads the real value: {footer} (mark at x={})", marks[0].0);

    // ASCII, monochrome, and the compact layout say the same with their own mark.
    let mut ascii = Shot::new("healthy", 90, 80, 24);
    (ascii.script, ascii.theme) = (one_slow_reply, Theme { color: ColorMode::Mono, glyphs: Glyphs::Ascii });
    let text = ascii.text();
    assert!(text.is_ascii() && text.contains("^ off scale, max 16"), "{text}");
}

#[test]
fn a_long_tail_has_its_own_row_in_the_distribution() {
    let mut s = Shot::new("healthy", 90, 120, 35);
    s.script = one_slow_reply;
    s.tweak = |app| (app.screen, app.tab) = (Screen::Details, Tab::Distribution);
    let text = s.text();
    let open: Vec<&str> = text.lines().filter(|l| l.contains("≥ ")).collect();
    assert_eq!(open.len(), 1, "{text}");
    assert!(open[0].contains(" 1 up to 16"), "one sample, and how far it reaches: {}", open[0]);
    assert!(text.contains("The last row is everything above the binned range"));
    // The bulk keeps fine bins: more than a handful of rows have samples.
    let filled = text.lines().filter(|l| l.contains('–') && l.contains('█')).count();
    assert!(filled >= 5, "{filled} bins with samples\n{text}");
    golden("distribution-120x35-one-slow-reply", &text);

    // At 80 × 24 with the full twelve rows, every note still fits inside the panel.
    let mut s = Shot::new("healthy", 90, 80, 24);
    s.script = one_slow_reply;
    s.tweak = |app| (app.screen, app.tab) = (Screen::Details, Tab::Distribution);
    s.theme = Theme { color: ColorMode::Mono, glyphs: Glyphs::Ascii };
    let text = s.text();
    assert!(text.is_ascii() && text.contains(">= ") && text.contains("up to 16"), "{text}");
    assert!(text.contains("low sample") && text.contains("count."), "the last note is not cut off:\n{text}");
}

#[test]
fn incidents_that_open_together_are_one_event_on_screen() {
    // All four ICMP targets stop at 60 s; ten seconds later the overview has one line for it.
    let overview = Shot::new("correlated-outage", 75, 120, 35).text();
    let events: Vec<&str> = overview.lines().skip_while(|l| !l.contains("─ Events ")).collect();
    assert!(events[0].contains("of 1"), "one event, not four or eight: {}", events[0]);
    assert!(events[1].contains("4 of 4 icmp v4") && events[1].contains("4 of 4 ICMP v4 streams began going unanswered"), "{}", events[1]);
    assert!(events[1].trim_end_matches([' ', '│']).ends_with('…'), "the overview cuts it short and says so: {}", events[1]);

    // The Events tab has the whole finding, wrapped, for any provider in it.
    let mut s = Shot::new("correlated-outage", 75, 120, 35);
    s.tweak = |app| (app.screen, app.tab, app.focus) = (Screen::Details, Tab::Events, 2);
    let text = s.text();
    let flat = text.lines().map(|l| l.trim_matches(['│', ' '])).collect::<Vec<_>>().join(" ");
    assert!(flat.contains("google (") && flat.contains("No comparable stream was unaffected.") && flat.contains("No gateway or local-DNS control is configured."), "{text}");
    golden("events-120x35-correlated-outage", &text);
}

/// A gateway that answers and a loopback-stub resolver, as controls.
fn with_controls(sc: &mut script::Scenario) {
    use pathwatch_core::model::{Family, Protocol};
    sc.streams.push(script::stream("gateway", "auto", Protocol::Icmp, Family::V4, "192.168.1.1", 2, 300, 333));
    sc.streams.push(script::stream("local-dns", "auto", Protocol::DnsUdp, Family::V4, "127.0.0.53", 1, 200, 1_666));
}

#[test]
fn controls_are_a_quiet_line_of_their_own_and_never_a_card() {
    let mut s = Shot::new("correlated-outage", 75, 120, 35);
    s.script = with_controls;
    let text = s.text();
    let line = text.lines().find(|l| l.contains("Gateway") && l.contains("Local DNS")).expect("a controls line");
    assert!(line.contains("Gateway 192.168.1.1") && line.contains("Local DNS (stub) 127.0.0.53"), "{line}");
    assert!(line.matches("OK").count() + line.matches("WARMING UP").count() == 2 && line.contains("no-reply 0.0 %"), "both are answering while the providers are not: {line}");
    // Still exactly four cards, and the controls are not in the chart or the timeline.
    let cards = text.lines().nth(2).unwrap();
    assert_eq!(cards.matches("╭─ ").count(), 4, "{cards}");
    assert!(!text.contains("5 Gateway") && !text.lines().any(|l| l.contains("╭─") && l.contains("Gateway")));
    // The grouped event cites them.
    let mut events = Shot::new("correlated-outage", 75, 120, 35);
    events.script = with_controls;
    events.tweak = |app| (app.screen, app.tab) = (Screen::Details, Tab::Events);
    let flat = events.text().lines().map(|l| l.trim_matches(['│', ' ']).to_string()).collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("4 of 4 ICMP v4 streams") && flat.contains("Controls in the same minute: gateway 60 of 60 answered; local DNS (a loopback stub) 12 of 12 answered."),
        "{flat}"
    );
    golden("controls-120x35-correlated-outage", &text);

    // Without controls the layout is exactly as before: no line, no gap.
    assert!(!Shot::new("correlated-outage", 75, 120, 35).text().lines().any(|l| l.contains("Gateway") || l.contains("Local DNS")));

    // The stub caveat is shown where there is room, and every layout and mode fits.
    for (w, h, color, glyphs) in [(80, 24, ColorMode::True, Glyphs::Unicode), (80, 24, ColorMode::Mono, Glyphs::Ascii), (160, 45, ColorMode::True, Glyphs::Unicode)] {
        let mut s = Shot::new("healthy", 90, w, h);
        (s.script, s.theme) = (with_controls, Theme { color, glyphs });
        let text = s.text();
        let line = text.lines().find(|l| l.contains("Gateway") && l.contains("Local DNS")).unwrap_or_else(|| panic!("{w}x{h}: {text}"));
        assert!(line.contains("Gateway") && line.contains("Local DNS"), "{w}x{h}: {line}");
        assert!(line.contains("Local DNS (stub)"), "{w}x{h}: the stub is marked at every width: {line}");
        assert_eq!(line.contains("192.168.1.1"), w >= 120, "{w}x{h}: addresses where there is room for them: {line}");
        assert!(!line.trim_end().ends_with('…') && !line.trim_end().ends_with('~'), "{w}x{h}: nothing is cut mid-way: {line}");
        assert!(glyphs == Glyphs::Unicode || text.is_ascii());
        let footer = text.lines().last().unwrap();
        assert!(footer.ends_with("· q") || footer.ends_with(". q"), "{w}x{h}: the footer is still whole: {footer}");
    }
}

#[test]
fn a_remote_system_resolver_is_labelled_as_such_at_every_width() {
    fn remote(sc: &mut script::Scenario) {
        use pathwatch_core::model::{Family, Protocol};
        sc.streams.push(script::stream("gateway", "auto", Protocol::Icmp, Family::V4, "192.168.1.1", 2, 300, 333));
        sc.streams.push(script::stream("local-dns", "auto", Protocol::DnsUdp, Family::V4, "203.0.113.53", 30, 900, 1_666));
    }
    for (w, h, glyphs) in [(80, 24, Glyphs::Unicode), (120, 35, Glyphs::Unicode), (120, 35, Glyphs::Ascii), (220, 60, Glyphs::Unicode)] {
        let mut s = Shot::new("healthy", 90, w, h);
        (s.script, s.theme) = (remote, Theme { color: if glyphs == Glyphs::Ascii { ColorMode::Mono } else { ColorMode::True }, glyphs });
        let text = s.text();
        let line = text.lines().find(|l| l.contains("Gateway")).unwrap_or_else(|| panic!("{w}x{h}: {text}"));
        assert!(line.contains("System DNS (remote)") && !line.contains("Local DNS"), "{w}x{h}: {line}");
        assert_eq!(line.contains("203.0.113.53"), w >= 120, "{w}x{h}: {line}");
        // Where there is room, the sentence too.
        assert_eq!(line.contains("not the local segment"), w >= 220, "{w}x{h}: {line}");
    }
}

// ───────────────────────── the Route view ─────────────────────────

/// A finished trace over a scripted path: a router that never answers, one
/// seen at two addresses, and a slow one — built by feeding the trace's own
/// state machine, so the notes are the ones it writes.
fn traced(dest: std::net::IpAddr) -> pathwatch_core::trace::TraceView {
    use pathwatch_core::probe::icmp_core::Incoming;
    use pathwatch_core::probe::icmp_packet::{ErrorKind, Parsed, Quoted};
    use pathwatch_core::trace::{Budget, Step, Trace};
    let budget = Budget { max_hops: 30, rounds: 10, round_interval: Duration::from_secs(1), max_probes: 300, max_rate: 30 };
    let mut t = Trace::new(dest, budget, 0);
    let mut now = 0u64;
    loop {
        match t.poll(now) {
            Step::Done => break,
            Step::Wait(until) => now = until,
            Step::Send { ttl, seq } => {
                t.sent(ttl, now);
                // Hop 2 is silent; hop 3 alternates between two addresses; hop 4 is slow; hop 5 is the destination.
                let (from, ms, reached) = match ttl {
                    1 => ("192.168.1.1", 1, false),
                    2 => continue,
                    3 => (if seq % 2 == 0 { "10.20.0.1" } else { "10.20.0.9" }, 6, false),
                    4 => ("198.51.100.14", 48, false),
                    _ => ("", 12, true),
                };
                let parsed = if reached {
                    Parsed::EchoReply { id: 1, seq16: seq, token: None, payload_len: 32 }
                } else {
                    Parsed::Error { kind: ErrorKind::TimeExceeded, icmp_type: 11, code: 0, quoted: Quoted { dest: Some(dest), id: None, seq16: Some(seq), token: None } }
                };
                let src = if reached { dest } else { from.parse().unwrap() };
                t.incoming(&Incoming { recv_ns: now + ms * 1_000_000, src, parsed, ttl: None, offender: None, recv_delay_ns: None });
            }
        }
    }
    let mut view = t.view(now);
    view.names = vec![("198.51.100.14".parse().unwrap(), "ae-4.core1.example.net".to_string())];
    view
}

#[test]
fn the_route_view_proposes_before_it_sends_and_explains_what_it_found() {
    // Before a trace: what it would send, and that nothing has been sent.
    let mut s = Shot::new("healthy", 60, 120, 35);
    s.tweak = |app| (app.screen, app.trace_target) = (Screen::Route, Some("1.1.1.1".parse().unwrap()));
    let text = s.text();
    assert!(text.contains("Route to 1.1.1.1") && text.contains("not started") && text.contains("Nothing has been sent."), "{text}");
    assert!(text.contains("at most 300 probes in all and 30 a second"), "the budget is shown before anything is sent: {text}");
    assert!(text.contains("DNS-over-UDP and HTTPS traffic to the same address may take another path"));
    golden("route-proposal-120x35", &text);

    // After one: every hop, each caveat on the row it concerns, the budget used.
    let mut s = Shot::new("healthy", 60, 120, 35);
    s.tweak = |app| {
        let dest: std::net::IpAddr = "1.1.1.1".parse().unwrap();
        (app.screen, app.trace_target, app.trace) = (Screen::Route, Some(dest), Some(std::sync::Arc::new(traced(dest))));
    };
    let text = s.text();
    let line = |needle: &str| text.lines().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("{needle} in\n{text}"));
    assert!(text.contains("finished") && text.contains("round 10 of 10") && text.contains("of 300"), "{text}");
    assert!(text.lines().nth(line("  2  *") + 1).unwrap().contains("does not send ICMP Time Exceeded — not evidence of loss"));
    assert!(text.lines().nth(line("  3  10.20.0.") + 1).unwrap().contains("also 10.20.0."));
    assert!(text.lines().nth(line("  3  10.20.0.") + 2).unwrap().contains("per-flow load balancing (ECMP) or a route change"));
    assert!(text.lines().nth(line("198.51.100.14 (ae-4.core1.example.net)") + 1).unwrap().contains("reflects that router's own reply handling and return path"));
    assert!(text.contains("1.1.1.1 — destination") && text.contains("rows are not additive"));
    golden("route-finished-120x35", &text);

    // Names can be hidden; the address stays.
    s.tweak = |app| {
        let dest: std::net::IpAddr = "1.1.1.1".parse().unwrap();
        (app.screen, app.trace_target, app.trace, app.names) = (Screen::Route, Some(dest), Some(std::sync::Arc::new(traced(dest))), false);
    };
    let text = s.text();
    assert!(text.contains("198.51.100.14") && !text.contains("core1.example.net"));

    // A trace of another address is not shown as this one's.
    s.tweak = |app| {
        let other: std::net::IpAddr = "8.8.8.8".parse().unwrap();
        (app.screen, app.trace_target, app.trace) = (Screen::Route, Some("1.1.1.1".parse().unwrap()), Some(std::sync::Arc::new(traced(other))));
    };
    assert!(s.text().contains("not started"));

    // Compact and ASCII: the same content, nothing non-ASCII, footer whole.
    let mut s = Shot::new("healthy", 60, 80, 24);
    s.theme = Theme { color: ColorMode::Mono, glyphs: Glyphs::Ascii };
    s.tweak = |app| {
        let dest: std::net::IpAddr = "1.1.1.1".parse().unwrap();
        (app.screen, app.trace_target, app.trace) = (Screen::Route, Some(dest), Some(std::sync::Arc::new(traced(dest))));
    };
    let text = s.text();
    assert!(text.is_ascii() && text.contains("not evidence of loss") && text.lines().last().unwrap().ends_with(". q"), "{text}");
}

// ───────────────────────────── the mouse ─────────────────────────────

mod mouse {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use pathwatch_tui::app::MOUSE;
    use pathwatch_tui::ui::Target;
    use pathwatch_tui::widgets;

    /// One size in each layout that draws a dashboard: compact, standard, large.
    const SIZES: [(u16, u16); 3] = [(100, 30), (120, 35), (170, 48)];

    fn text_in(buf: &Buffer, r: Rect) -> String {
        (r.y..r.bottom()).map(|y| (r.x..r.right()).map(|x| buf[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    fn at(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }
    }

    fn click(x: u16, y: u16) -> MouseEvent {
        at(MouseEventKind::Down(MouseButton::Left), x, y)
    }

    fn middle(r: Rect) -> (u16, u16) {
        (r.x + r.width / 2, r.y + r.height / 2)
    }

    /// The app as it would be after these keys, and after this mouse event, from the same start.
    fn after_keys(start: &App, snap: &Snapshot, keys: &str) -> String {
        let mut app = start.clone();
        for k in keys.chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(k), KeyModifiers::NONE), snap, false);
        }
        format!("{app:?}")
    }

    fn after_mouse(start: &App, snap: &Snapshot, frame: &ui::Frame, event: MouseEvent) -> String {
        let mut app = start.clone();
        app.on_mouse(event, frame, snap);
        format!("{app:?}")
    }

    #[test]
    fn every_target_is_on_the_thing_it_stands_for() {
        for (w, h) in SIZES {
            let s = Shot::new("healthy", 60, w, h);
            let (buf, frame, _, snap) = s.stage(&|_| {});
            let what = format!("{w}x{h}");

            // Every provider can be reached, and each of its targets has its
            // name, or its two-letter tag, written in it.
            for c in &snap.cards {
                let mine: Vec<Rect> = frame.targets.iter().filter(|(_, t)| *t == Target::Provider(c.slot)).map(|(r, _)| *r).collect();
                assert!(mine.len() >= 2, "{what}: {} has {} targets", c.name, mine.len());
                for r in &mine {
                    let text = text_in(&buf, *r);
                    assert!(text.contains(&c.name) || text.contains(&widgets::tag(&c.name)), "{what}: a target for {} covers {text:?}", c.name);
                }
                // In the layouts with a legend, one of them is exactly the legend's name.
                if w >= 120 {
                    assert!(mine.iter().any(|r| text_in(&buf, *r) == c.name), "{what}: no target is the legend entry {:?}", c.name);
                }
            }

            // The chart and the outcomes share one time axis, and the chart's
            // baseline runs under exactly the columns its target claims.
            let spans: Vec<(Rect, u16, u16)> =
                frame.targets.iter().filter_map(|(r, t)| if let Target::Time { x, columns } = t { Some((*r, *x, *columns)) } else { None }).collect();
            assert_eq!(spans.len(), 2, "{what}: the chart and the outcomes");
            assert_eq!((spans[0].1, spans[0].2), (spans[1].1, spans[1].2), "{what}: one time axis");
            let (plot, x, columns) = spans[0];
            assert_eq!(columns, snap.view.columns, "{what}");
            let base = plot.bottom() - 2;
            assert_eq!(buf[(x - 1, base)].symbol(), "└", "{what}");
            assert!((x..x + columns).all(|cx| buf[(cx, base)].symbol() == "─"), "{what}");
            assert_ne!(buf[(x + columns, base)].symbol(), "─", "{what}");

            // Details: each tab's target is its label, and only a tab that is a list scrolls.
            for (tab, scrolls) in [(Tab::Streams, false), (Tab::Distribution, false), (Tab::Dns, true), (Tab::Raw, true), (Tab::Events, true)] {
                let (buf, frame, _, _) = s.stage(&|app| (app.screen, app.tab) = (Screen::Details, tab));
                for t in Tab::ALL {
                    let r = frame.targets.iter().find(|(_, x)| *x == Target::Tab(t)).unwrap_or_else(|| panic!("{what}: no target for {t:?}")).0;
                    assert_eq!(text_in(&buf, r).trim_matches([' ', '[', ']']), t.label(), "{what}");
                }
                assert_eq!(frame.targets.iter().any(|(_, t)| *t == Target::List), scrolls, "{what} {tab:?}");
                assert!(!frame.targets.iter().any(|(_, t)| matches!(t, Target::Provider(_) | Target::Time { .. })), "{what}: nothing of the overview is left behind");
            }

            // Under the help panel there is nothing to click.
            let (_, frame, _, _) = s.stage(&|app| app.help = true);
            assert!(frame.targets.is_empty(), "{what}");
        }

        // The compact layout's switch between all series and the focused one.
        let (buf, frame, _, _) = Shot::new("healthy", 60, 100, 30).stage(&|_| {});
        let r = frame.targets.iter().find(|(_, t)| *t == Target::Series).expect("the switch").0;
        assert_eq!(text_in(&buf, r), "a: all four");
    }

    #[test]
    fn a_click_or_the_wheel_does_exactly_what_its_keys_do() {
        for (w, h) in SIZES {
            let s = Shot::new("healthy", 60, w, h);
            let what = format!("{w}x{h}");
            // Start from somewhere a change shows: provider 3 focused, a list scrolled.
            let (_, frame, start, snap) = s.stage(&|app| (app.focus, app.scroll) = (3, 4));
            let keys = |k: &str| after_keys(&start, &snap, k);
            let mouse = |e: MouseEvent| after_mouse(&start, &snap, &frame, e);
            let unchanged = format!("{start:?}");
            assert_ne!(keys("-"), keys("+"), "the window has somewhere to go");

            for (r, target) in &frame.targets {
                let (mx, my) = middle(*r);
                match *target {
                    Target::Provider(slot) => assert_eq!(mouse(click(mx, my)), keys(&slot.to_string()), "{what}: provider {slot} at {r:?}"),
                    Target::Time { x, columns } => {
                        // The column clicked is where the cursor goes: `c`, then `h` that many times.
                        for back in [0u16, 1, 7, columns - 1] {
                            let typed = format!("c{}", "h".repeat(usize::from(back)));
                            assert_eq!(mouse(click(x + columns - 1 - back, my)), keys(&typed), "{what}: {back} columns back at {r:?}");
                        }
                        // The wheel away from you is `-`, towards you `+`.
                        assert_eq!(mouse(at(MouseEventKind::ScrollUp, mx, my)), keys("-"), "{what}");
                        assert_eq!(mouse(at(MouseEventKind::ScrollDown, mx, my)), keys("+"), "{what}");
                    }
                    Target::Series => assert_eq!(mouse(click(mx, my)), keys("a"), "{what}"),
                    Target::Tab(_) | Target::List => panic!("{what}: {target:?} on the overview"),
                }
                // Only the left button and the wheel do anything.
                for kind in [
                    MouseEventKind::Down(MouseButton::Right),
                    MouseEventKind::Down(MouseButton::Middle),
                    MouseEventKind::Up(MouseButton::Left),
                    MouseEventKind::Drag(MouseButton::Left),
                    MouseEventKind::Moved,
                    MouseEventKind::ScrollLeft,
                ] {
                    assert_eq!(mouse(at(kind, mx, my)), unchanged, "{what}: {kind:?} on {target:?}");
                }
            }
            // The header and the footer are not targets.
            assert_eq!(mouse(click(1, 0)), unchanged, "{what}");
            assert_eq!(mouse(click(1, h - 1)), unchanged, "{what}");

            // While the help panel is open, or a question is waiting, the mouse is not an answer.
            let card = middle(frame.targets.iter().find(|(_, t)| *t == Target::Provider(1)).unwrap().0);
            for (name, block) in [("help", (|app: &mut App| app.help = true) as fn(&mut App)), ("a pending question", |app: &mut App| app.confirm_stop = true)] {
                let mut blocked = start.clone();
                block(&mut blocked);
                assert_eq!(after_mouse(&blocked, &snap, &frame, click(card.0, card.1)), format!("{blocked:?}"), "{what}: {name}");
            }

            // Details: a tab's target opens it, as stepping to it with `l` does.
            let (_, frame, start, snap) = s.stage(&|app| (app.screen, app.tab, app.scroll) = (Screen::Details, Tab::Streams, 0));
            for (steps, tab) in Tab::ALL.iter().enumerate() {
                let r = frame.targets.iter().find(|(_, t)| *t == Target::Tab(*tab)).unwrap().0;
                let (mx, my) = middle(r);
                assert_eq!(after_mouse(&start, &snap, &frame, click(mx, my)), after_keys(&start, &snap, &"l".repeat(steps)), "{what}: {tab:?}");
            }
            // On a tab that is not a list the wheel does nothing.
            assert_eq!(after_mouse(&start, &snap, &frame, at(MouseEventKind::ScrollDown, w / 2, h / 2)), format!("{start:?}"), "{what}");

            // On one that is, a notch is three rows.
            let (_, frame, start, snap) = s.stage(&|app| (app.screen, app.tab, app.scroll) = (Screen::Details, Tab::Raw, 5));
            assert_eq!(after_mouse(&start, &snap, &frame, at(MouseEventKind::ScrollDown, w / 2, h / 2)), after_keys(&start, &snap, "jjj"), "{what}");
            assert_eq!(after_mouse(&start, &snap, &frame, at(MouseEventKind::ScrollUp, w / 2, h / 2)), after_keys(&start, &snap, "kkk"), "{what}");
            // Clicking the tab already open keeps the place in its list.
            let open = middle(frame.targets.iter().find(|(_, t)| *t == Target::Tab(Tab::Raw)).unwrap().0);
            assert_eq!(after_mouse(&start, &snap, &frame, click(open.0, open.1)), format!("{start:?}"), "{what}");
        }
    }

    #[test]
    fn the_route_view_scrolls_under_the_wheel_and_nothing_else() {
        let s = Shot::new("healthy", 60, 120, 35);
        let (_, frame, start, snap) = s.stage(&|app| (app.screen, app.trace_target, app.scroll) = (Screen::Route, Some("192.0.2.1".parse().unwrap()), 3));
        assert_eq!(frame.targets.iter().map(|(_, t)| *t).collect::<Vec<_>>(), [Target::List]);
        assert_eq!(after_mouse(&start, &snap, &frame, at(MouseEventKind::ScrollUp, 60, 10)), after_keys(&start, &snap, "kkk"));
        // A click starts nothing: a trace is started from the keyboard.
        assert_eq!(after_mouse(&start, &snap, &frame, click(60, 10)), format!("{start:?}"));
    }

    #[test]
    fn the_help_panel_explains_the_mouse_only_when_it_is_captured_and_fits_the_smallest_terminal() {
        let mut s = Shot::new("healthy", 60, 80, 24);
        s.tweak = |app| app.help = true;
        let without = s.text();
        assert!(MOUSE.iter().all(|(_, what)| !without.contains(what)), "not captured, not mentioned");
        s.mouse = true;
        let with = s.text();
        for (how, what) in MOUSE {
            assert!(with.lines().any(|l| l.contains(how) && l.contains(what)), "{how}: {what}\n{with}");
        }
        assert!(with.contains("quit and restore the terminal"), "and no key was pushed out to make room");
        golden("help-mouse-80x24", &with);
    }
}

/// What drawing one frame costs. The dashboard draws up to ten a second,
/// so a millisecond here is one per cent of a processor core. Run on
/// request: `cargo test --release -p pathwatch-tui --test render perf -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement"]
fn perf_drawing_a_frame() {
    if cfg!(debug_assertions) {
        panic!("this is a measurement of a release build: add --release");
    }
    let mut ok = true;
    for (name, w, h, tweak) in [
        ("overview, 120 × 35", 120u16, 35u16, (|_| {}) as fn(&mut App)),
        ("overview, 160 × 45", 160, 45, |_| {}),
        ("details, 120 × 35", 120, 35, |app| app.screen = Screen::Details),
    ] {
        let mut s = Shot::new("isolated-failure", 90, w, h);
        s.tweak = tweak;
        // The snapshot and the application are made once; only drawing is timed.
        let (_, _, app, snap) = s.stage(&|app| tweak(app));
        let chrome = Chrome {
            session: Session::Demo { name: "isolated-failure".into(), end_ns: u64::MAX },
            source: "via 192.0.2.10".into(),
            notes: vec!["ICMP: unprivileged datagram socket".into()],
            recording: None,
            trace_budget: None,
            mouse: false,
            reduced_motion: false,
        };
        let area = Rect::new(0, 0, w, h);
        let runs = 500;
        let start = std::time::Instant::now();
        for _ in 0..runs {
            let mut buf = Buffer::empty(area);
            std::hint::black_box(ui::draw(&mut buf, area, &snap, &app, &s.theme, &chrome));
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / f64::from(runs);
        let within = ms <= 0.5;
        ok &= within;
        println!("PERF | drawing one frame, {name} | ≤ 0.5 ms | {ms:.3} ms, mean of {runs} | {}", if within { "within" } else { "OVER" });
    }
    assert!(ok, "a frame costs more than its share of the CPU budget; see the PERF rows");
}
