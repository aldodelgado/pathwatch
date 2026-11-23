//! Drawing primitives. Everything here writes cells into a buffer from a
//! snapshot; nothing here reads a clock, a socket, or the terminal.

use pathwatch_core::snapshot::{Card, Column, EventLine, Snapshot, Status};
use pathwatch_core::stats::Gated;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::theme::{ColorMode, Glyphs, Theme};

/// Writes `text` at (`x`, `y`), clipped to `area`. Never panics on a small terminal.
///
/// A cell is sent to the terminal as it is, so a control character or a
/// bidirectional override never goes into one: whatever the text's source,
/// it is drawn as `?`. Text is cleaned where it enters the program; this is
/// the last line of defence, not the first.
pub fn put(buf: &mut Buffer, area: Rect, x: u16, y: u16, text: &str, style: Style) {
    let area = area.intersection(buf.area);
    if y < area.y || y >= area.bottom() || x >= area.right() {
        return;
    }
    for (cx, ch) in (x..).zip(text.chars()) {
        if cx >= area.right() {
            break;
        }
        if cx >= area.x {
            let ch = if pathwatch_core::sanitize::is_unsafe(ch) { '?' } else { ch };
            buf[(cx, y)].set_char(ch).set_style(style);
        }
    }
}

pub fn width(s: &str) -> u16 {
    s.chars().count() as u16
}

/// The column a panel's right-hand note starts at, when the panel has room
/// for it beside its title. `panel` draws from this, so a mouse target
/// worked out from it is where the text is.
pub fn panel_note_x(area: Rect, title: &str, right: &str) -> Option<u16> {
    if area.width < 4 || area.height < 2 || right.is_empty() {
        return None;
    }
    let title: String = title.chars().take(usize::from(area.width - 4)).collect();
    (width(&title) + 4 + width(right) + 4 <= area.width).then(|| area.right() - 2 - width(right))
}

/// A panel border with a title on the left and an optional note on the right.
/// Returns the inner area.
pub fn panel(buf: &mut Buffer, area: Rect, title: &str, right: &str, theme: &Theme, border: Style, title_style: Style) -> Rect {
    if area.width < 4 || area.height < 2 {
        return Rect::default();
    }
    let [h, v, tl, tr, bl, br] = theme.box_chars();
    let (x0, x1, y0, y1) = (area.x, area.right() - 1, area.y, area.bottom() - 1);
    let line: String = std::iter::repeat_n(h, usize::from(area.width)).collect();
    put(buf, area, x0, y0, &line, border);
    put(buf, area, x0, y1, &line, border);
    for y in y0 + 1..y1 {
        put(buf, area, x0, y, &v.to_string(), border);
        put(buf, area, x1, y, &v.to_string(), border);
    }
    for (x, y, c) in [(x0, y0, tl), (x1, y0, tr), (x0, y1, bl), (x1, y1, br)] {
        put(buf, area, x, y, &c.to_string(), border);
    }
    let room = area.width.saturating_sub(4);
    let shown: String = title.chars().take(usize::from(room)).collect();
    put(buf, area, x0 + 2, y0, &format!(" {shown} "), title_style);
    if let Some(x) = panel_note_x(area, title, right) {
        put(buf, area, x - 1, y0, &format!(" {right} "), border);
    }
    Rect { x: area.x + 1, y: area.y + 1, width: area.width - 2, height: area.height - 2 }
}

pub fn ms(us: u32) -> String {
    format!("{:.1}", f64::from(us) / 1000.0)
}

/// A statistic gated by sample count: `—` when withheld, `·` when the count is low.
pub fn gated(g: Gated) -> String {
    match g {
        Gated::Ok(v) => format!("{:.1}", v / 1000.0),
        Gated::LowN(v) => format!("{:.1}·", v / 1000.0),
        Gated::Insufficient => "—".into(),
    }
}

/// A rate as a percentage once there are enough finalised probes, else the raw fraction.
pub fn rate(rate: Option<f64>, k: u64, n: u64) -> String {
    match rate {
        Some(r) => format!("{:.1} %", r * 100.0),
        None if n == 0 => "—".into(),
        None => format!("{k} / {n}"),
    }
}

pub fn age(ms: u64) -> String {
    if ms < 10_000 { format!("{:.1} s ago", ms as f64 / 1000.0) } else { format!("{} s ago", ms / 1000) }
}

/// A two-letter tag for a series: initials where the name has them
/// ("NextDNS" → "ND"), a known short form, otherwise the first two letters.
pub fn tag(name: &str) -> String {
    let caps: String = name.chars().filter(|c| c.is_uppercase()).take(2).collect();
    match name {
        "Cloudflare" => "CF".into(),
        _ if caps.chars().count() == 2 => caps,
        _ => name.chars().take(2).collect::<String>().to_uppercase(),
    }
}

/// Resamples a series to `w` columns for a sparkline: each output column is
/// the largest value of the input columns it covers; `None` is a hole.
fn resample(cols: &[Column], w: usize) -> Vec<Option<u32>> {
    (0..w)
        .map(|i| {
            let a = i * cols.len() / w.max(1);
            let b = ((i + 1) * cols.len() / w.max(1)).max(a + 1).min(cols.len());
            cols.get(a..b).and_then(|s| s.iter().filter(|c| c.n > 0).map(|c| c.max_us).max())
        })
        .collect()
}

pub fn sparkline(buf: &mut Buffer, area: Rect, x: u16, y: u16, w: u16, card: &Card, theme: &Theme) {
    let levels = theme.spark_levels();
    let vals = resample(&card.columns, usize::from(w));
    let present: Vec<u32> = vals.iter().flatten().copied().collect();
    let (lo, hi) = (present.iter().min().copied().unwrap_or(0), present.iter().max().copied().unwrap_or(1));
    for (i, v) in vals.iter().enumerate() {
        // A hole stays a hole: where the timeline shows unanswered probes,
        // mark it; otherwise leave it blank. Never draw a zero.
        let (ch, style) = match v {
            Some(v) => {
                let k = if hi > lo { ((u64::from(*v - lo) * 7) / u64::from(hi - lo)) as usize } else { 0 };
                (levels[k.min(7)], theme.provider(card.slot))
            }
            None => {
                let a = i * card.cells.len() / usize::from(w.max(1));
                let b = ((i + 1) * card.cells.len() / usize::from(w.max(1))).max(a + 1).min(card.cells.len());
                let unanswered = card
                    .cells
                    .get(a..b)
                    .is_some_and(|c| c.iter().any(|c| matches!(c, pathwatch_core::snapshot::Cell::NoneAnswered | pathwatch_core::snapshot::Cell::SomeUnanswered)));
                (if unanswered { if theme.glyphs == Glyphs::Unicode { '×' } else { 'x' } } else { ' ' }, theme.dim())
            }
        };
        put(buf, area, x + i as u16, y, &ch.to_string(), style);
    }
}

fn status_text(theme: &Theme, s: &Status) -> String {
    format!("{} {}", theme.status_symbol(s), s.word())
}

/// One provider card.
/// How a card is to be drawn.
#[derive(Debug, Clone, Copy)]
pub struct CardLook<'a> {
    pub mode: &'a str,
    pub family: &'a str,
    pub focused: bool,
    /// Adds the min/mean/max and sent/pending lines.
    pub tall: bool,
}

pub fn card(buf: &mut Buffer, area: Rect, c: &Card, look: CardLook<'_>, theme: &Theme) {
    let CardLook { mode, family, focused, tall } = look;
    let name_style = theme.provider(c.slot).add_modifier(Modifier::BOLD);
    let inner = panel(buf, area, &format!("{} {}", c.slot, c.name), "", theme, theme.border(focused), name_style);
    if inner.width < 10 || inner.height < 3 {
        return;
    }
    let (x, w) = (inner.x + 1, inner.width.saturating_sub(2));
    let mut y = inner.y;
    let status = status_text(theme, &c.status);
    put(buf, inner, x, y, &status, theme.status_style(&c.status));
    // The mode and family, shortened or dropped rather than run into the status.
    let label = [format!("{mode} · v{family}"), format!("v{family}")].into_iter().find(|l| width(&status) + 1 + width(l) <= w);
    if let Some(label) = label {
        put(buf, inner, x + w - width(&label), y, &label, theme.dim());
    }
    y += 1;

    if c.status.is_inactive() {
        // Say so in words, say what to do, and say that nothing is being sent.
        // The fact that nothing is sent comes first, so it can never be
        // pushed off a small card by a long explanation.
        let why = match &c.status {
            Status::NeedsConfig(r) if r.contains("environment variable") => format!("{r}."),
            Status::NeedsConfig(_) => format!("Set [providers.{}] in your configuration and restart.", c.key),
            Status::Unsupported(r) => format!("{r}."),
            _ => "Switched off in configuration.".into(),
        };
        let text = format!("Nothing is being sent; not counted as loss. {why}");
        for line in wrap(&text, usize::from(w)).into_iter().take(usize::from(inner.bottom().saturating_sub(y))) {
            put(buf, inner, x, y, &line, theme.dim());
            y += 1;
        }
        return;
    }

    // The other family's state sits at the right; a long target (a DoH URL
    // that has no address yet) is cut with `…` rather than run into it.
    let other = c.other_family.as_ref().map(|o| (format!("v{} {}", if family == "4" { "6" } else { "4" }, theme.status_symbol(o)), theme.status_style(o)));
    let room = w.saturating_sub(other.as_ref().map_or(0, |(l, _)| width(l) + 1));
    put(buf, inner, x, y, &ellipsis(&c.target, usize::from(room)), Style::default());
    if let Some((label, style)) = other {
        put(buf, inner, x + w.saturating_sub(width(&label)), y, &label, style);
    }
    y += 1;
    let Some(s) = &c.stats else { return };
    let big = c.last_us.map_or_else(|| "—".to_string(), |v| format!("{} ms", ms(v)));
    put(buf, inner, x + 1, y, &big, theme.bold());
    if let Some(a) = c.last_age_ms {
        let a = format!("last {}", age(a));
        put(buf, inner, x + w.saturating_sub(width(&a)), y, &a, theme.dim());
    }
    y += 1;
    put(buf, inner, x, y, &format!("p50 {}  p95 {}  Δ {}", gated(s.p50_us), gated(s.p95_us), gated(s.delta_us)), Style::default());
    y += 1;
    if tall {
        let (lo, hi) = (s.min_us.map_or("—".into(), ms), s.max_us.map_or("—".into(), ms));
        put(buf, inner, x, y, &format!("min {lo}  mean {}  max {hi}", gated(s.mean_us)), theme.dim());
        y += 1;
    }
    let unanswered_style = if s.unanswered > 0 { theme.status_style(&Status::Lossy) } else { Style::default() };
    put(buf, inner, x, y, &format!("no-reply {}", rate(s.no_reply_rate, s.unanswered, s.finalised)), unanswered_style);
    // Error responses are not "no reply": when there are any they take the
    // right of this line, so a stream that answers with errors is not
    // mistaken for a healthy one with nothing to time.
    if s.errors > 0 {
        let errs = format!("err {} / {}", s.errors, s.finalised);
        put(buf, inner, x + w.saturating_sub(width(&errs)), y, &errs, theme.status_style(&Status::Errors));
    } else if s.no_reply_rate.is_some() {
        let frac = format!("{} / {}", s.unanswered, s.finalised);
        put(buf, inner, x + w.saturating_sub(width(&frac)), y, &frac, theme.dim());
    }
    y += 1;
    let coverage = format!("n {} · coverage {:.0} %", s.n, s.coverage * 100.0);
    put(buf, inner, x, y, &coverage, theme.dim());
    if tall {
        y += 1;
        put(buf, inner, x, y, &format!("sent {} · pending {} · local err {}", s.sent, s.pending, s.local_error), theme.dim());
    }
    if inner.bottom() > y + 1 {
        sparkline(buf, inner, x, inner.bottom() - 1, w, c, theme);
    }
}

/// Breaks text into lines of at most `w` characters at word boundaries.
pub fn wrap(text: &str, w: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        let cur = lines.last_mut().unwrap();
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > w {
            lines.push(word.to_string());
        } else {
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
    }
    lines
}

/// The compact layout's provider table: one row per provider.
pub fn table(buf: &mut Buffer, area: Rect, snap: &Snapshot, focus: u8, theme: &Theme) {
    put(buf, area, area.x, area.y, "    provider    target          status          last    p50    p95 no-reply    n", theme.dim());
    for (i, c) in snap.cards.iter().enumerate() {
        let y = area.y + 1 + i as u16;
        let marker = if c.slot == focus { if theme.glyphs == Glyphs::Unicode { "▸" } else { ">" } } else { " " };
        put(buf, area, area.x + 1, y, marker, theme.bold());
        put(buf, area, area.x + 2, y, &format!("{} {:<11}", c.slot, c.name), theme.provider(c.slot).add_modifier(Modifier::BOLD));
        let target: String = if c.status.is_inactive() { "—".into() } else { c.target.chars().take(15).collect() };
        put(buf, area, area.x + 16, y, &target, Style::default());
        put(buf, area, area.x + 32, y, &status_text(theme, &c.status), theme.status_style(&c.status));
        if let (Some(s), false) = (&c.stats, c.status.is_inactive()) {
            let line = format!(
                "{:>5} {:>6} {:>6} {:>8} {:>4}",
                c.last_us.map_or("—".into(), ms),
                gated(s.p50_us),
                gated(s.p95_us),
                rate(s.no_reply_rate, s.unanswered, s.finalised).replace(" %", "%").replace(" / ", "/"),
                s.n
            );
            put(buf, area, area.x + 46, y, &line, Style::default());
        }
    }
}

const AXIS: u16 = 6;
const TAGS: u16 = 4;

/// How many time columns the plot has for a panel of this inner width.
pub fn plot_columns(inner_width: u16) -> u16 {
    inner_width.saturating_sub(AXIS + TAGS).max(1)
}

/// A span of time for an axis label: whole hours or minutes where possible,
/// otherwise minutes and seconds ("3m45").
pub fn window_label(secs: u64) -> String {
    match secs {
        s if s >= 3600 && s % 3600 == 0 => format!("{}h", s / 3600),
        s if s >= 3600 => format!("{}h{:02}", s / 3600, (s % 3600) / 60),
        s if s >= 60 && s % 60 == 0 => format!("{}m", s / 60),
        s if s >= 60 => format!("{}m{:02}", s / 60, s % 60),
        s => format!("{s}s"),
    }
}

/// Where the plot's time columns are in a panel's inner area: the first
/// one's column on screen and how many there are. `None` when the area is
/// too small for a plot to be drawn at all.
pub fn plot_span(area: Rect) -> Option<(u16, u16)> {
    (area.width >= AXIS + TAGS + 8 && area.height >= 4).then(|| (area.x + AXIS, area.width - AXIS - TAGS))
}

/// The same for the outcome timeline, which shares the plot's time axis.
pub fn timeline_span(area: Rect, snap: &Snapshot) -> (u16, u16) {
    let cells = snap.cards.iter().map(|c| c.cells.len()).max().unwrap_or(0);
    (area.x + AXIS, cells.min(usize::from(area.width.saturating_sub(AXIS + TAGS))) as u16)
}

/// The comparison chart: one series per provider on a common time axis and a
/// common latency axis. A column with no round-trip time is a hole in the
/// line; nothing is interpolated across it and no old value is carried forward.
pub fn plot(buf: &mut Buffer, area: Rect, snap: &Snapshot, focus: u8, only_focus: bool, cursor: Option<u16>, theme: &Theme) {
    let Some((px, w)) = plot_span(area) else { return };
    let (h, py) = (area.height - 2, area.y);
    let y_max = snap.y_max_us.max(1);
    let unicode_plot = theme.glyphs == Glyphs::Unicode && theme.color != ColorMode::Mono;
    let dots_h = i32::from(h) * 4;
    let dot_y = |us: u32| -> i32 { ((1.0 - f64::from(us.min(y_max)) / f64::from(y_max)) * f64::from(dots_h - 1)).round() as i32 };

    // Axis: the scale is common to every series and stated in the panel title.
    let (v, corner, tick) = if theme.glyphs == Glyphs::Unicode { ('│', '└', '┤') } else { ('|', '+', '+') };
    for r in 0..h {
        let label = if r % 2 == 0 { format!("{:>4} {tick}", axis_label(f64::from(y_max) / 1000.0 * f64::from(h - r) / f64::from(h))) } else { format!("     {v}") };
        put(buf, area, area.x, py + r, &label, theme.dim());
    }
    let hline = if theme.glyphs == Glyphs::Unicode { '─' } else { '-' };
    put(buf, area, area.x, py + h, &format!("   0 {corner}{}", std::iter::repeat_n(hline, usize::from(w)).collect::<String>()), theme.dim());
    let secs = snap.view.window.as_secs();
    for i in 0..=4u64 {
        let label = if i == 4 { "now".to_string() } else { format!("-{}", window_label(secs * (4 - i) / 4)) };
        let pos = (u64::from(w - 1) * i / 4) as u16;
        let x = (px + pos).saturating_sub(width(&label) / 2).clamp(px, px + w - width(&label).min(w));
        put(buf, area, x, py + h + 1, &label, theme.dim());
    }

    let order: Vec<&Card> = {
        let mut v: Vec<&Card> = snap.cards.iter().filter(|c| !c.status.is_inactive() && (!only_focus || c.slot == focus)).collect();
        v.sort_by_key(|c| c.slot == focus); // the focused series is drawn last, on top
        v
    };

    if unicode_plot {
        // Braille: 2 × 4 dots per cell. Each time column draws the spread of
        // its round trips (so one spike survives downsampling) and joins its
        // median to the next column's — but only if that column has data.
        let cells = usize::from(w) * usize::from(h);
        let mut bits = vec![0u8; cells];
        let mut owner = vec![0u8; cells];
        const DOT: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];
        let mut set = |dx: i32, dy: i32, slot: u8| {
            if dx < 0 || dy < 0 || dx >= i32::from(w) * 2 || dy >= dots_h {
                return;
            }
            let i = (dy / 4) as usize * usize::from(w) + (dx / 2) as usize;
            bits[i] |= DOT[(dy % 4) as usize][(dx % 2) as usize];
            owner[i] = slot;
        };
        for c in &order {
            let cols = &c.columns[..c.columns.len().min(usize::from(w))];
            for (i, col) in cols.iter().enumerate() {
                if col.n == 0 {
                    continue;
                }
                let x = i as i32 * 2;
                let med = dot_y(col.med_us);
                for dy in dot_y(col.max_us)..=dot_y(col.min_us) {
                    set(x, dy, c.slot);
                }
                set(x, med, c.slot);
                set(x + 1, med, c.slot);
                if let Some(next) = cols.get(i + 1).filter(|n| n.n > 0) {
                    let to = dot_y(next.med_us);
                    for dy in med.min(to)..=med.max(to) {
                        set(x + 1, dy, c.slot);
                    }
                }
            }
        }
        for r in 0..h {
            for cx in 0..w {
                let i = usize::from(r) * usize::from(w) + usize::from(cx);
                if bits[i] != 0 {
                    let ch = char::from_u32(0x2800 + u32::from(bits[i])).unwrap_or(' ');
                    let mut style = theme.provider(owner[i]);
                    if owner[i] != focus && !only_focus {
                        style = style.add_modifier(Modifier::DIM);
                    }
                    put(buf, area, px + cx, py + r, &ch.to_string(), style);
                }
            }
        }
    } else {
        // Without colour or braille the series cannot be told apart by hue, so
        // each is drawn with its slot digit (or `*` when colour is available).
        for c in &order {
            let mark = if theme.is_mono() { char::from(b'0' + c.slot) } else { '*' };
            for (i, col) in c.columns.iter().take(usize::from(w)).enumerate() {
                if col.n == 0 {
                    continue;
                }
                let row = (dot_y(col.med_us) / 4) as u16;
                let style = if c.slot == focus { theme.provider(c.slot).add_modifier(Modifier::BOLD) } else { theme.provider(c.slot) };
                put(buf, area, px + i as u16, py + row.min(h - 1), &mark.to_string(), style);
            }
        }
    }

    // A column whose highest round trip is above the scale gets a mark on
    // the top row, in its series' colour: the value is off the chart, not
    // missing, and the cursor and the Distribution tab still give it exactly.
    for c in &order {
        for (i, col) in c.columns.iter().take(usize::from(w)).enumerate() {
            if col.n > 0 && col.max_us > y_max {
                put(buf, area, px + i as u16, py, &off_scale_mark(theme).to_string(), theme.provider(c.slot).add_modifier(Modifier::BOLD));
            }
        }
    }

    // The cursor: a rule through the empty cells of one time column, so it
    // never hides a data point, and a mark on the axis beneath it.
    if let Some(col) = cursor.and_then(|off| cursor_column(usize::from(w), off)) {
        let x = px + col as u16;
        let (rule, mark) = if theme.glyphs == Glyphs::Unicode { ('┆', '┴') } else { (':', '^') };
        for r in 0..h {
            if buf.area.contains((x, py + r).into()) && buf[(x, py + r)].symbol() == " " {
                put(buf, area, x, py + r, &rule.to_string(), theme.dim());
            }
        }
        put(buf, area, x, py + h, &mark.to_string(), theme.bold());
    }

    // Name each series at its right-hand end, so the legend is on the data.
    let mut taken: Vec<u16> = Vec::new();
    for c in order.iter().rev() {
        let Some(last) = c.columns.iter().take(usize::from(w)).rev().find(|col| col.n > 0) else { continue };
        let mut row = ((dot_y(last.med_us) / 4) as u16).min(h - 1);
        while taken.contains(&row) && row > 0 {
            row -= 1;
        }
        taken.push(row);
        put(buf, area, px + w + 1, py + row, &tag(&c.name), theme.provider(c.slot).add_modifier(Modifier::BOLD));
    }
}

/// A tick's value in milliseconds: whole where it is whole, otherwise with
/// one decimal, so a tick at 5.7 ms is never labelled "6".
fn axis_label(ms: f64) -> String {
    if ms >= 100.0 || (ms - ms.round()).abs() < 0.05 { format!("{ms:.0}") } else { format!("{ms:.1}") }
}

/// The mark for a column whose value is above the top of the scale.
pub fn off_scale_mark(theme: &Theme) -> char {
    if theme.glyphs == Glyphs::Unicode { '▲' } else { '^' }
}

/// The highest round trip above the scale among the series that are drawn
/// (`only` restricts it to one provider slot), if any is off the scale.
pub fn off_scale_peak(snap: &Snapshot, only: Option<u8>) -> Option<u32> {
    snap.cards
        .iter()
        .filter(|c| !c.status.is_inactive() && only.is_none_or(|slot| c.slot == slot))
        .flat_map(|c| c.columns.iter().filter(|col| col.n > 0).map(|col| col.max_us))
        .max()
        .filter(|peak| *peak > snap.y_max_us)
}

/// The column a cursor `off` columns left of "now" is on, out of `n`.
pub fn cursor_column(n: usize, off: u16) -> Option<usize> {
    n.checked_sub(1)?.checked_sub(usize::from(off))
}

/// The outcome timeline: one row per provider on the chart's exact time axis.
/// This is where unanswered probes, errors, and gaps live, so every hole in
/// the chart is accounted for directly beneath it.
pub fn timeline(buf: &mut Buffer, area: Rect, snap: &Snapshot, cursor: Option<u16>, theme: &Theme) {
    for (i, c) in snap.cards.iter().enumerate() {
        let y = area.y + i as u16;
        if y >= area.bottom() {
            break;
        }
        put(buf, area, area.x, y, &format!(" {} {} ", c.slot, tag(&c.name)), theme.provider(c.slot).add_modifier(Modifier::BOLD));
        let x0 = area.x + AXIS;
        if c.status.is_inactive() {
            let why = match &c.status {
                Status::NeedsConfig(_) => "needs configuration — nothing sent",
                Status::Unsupported(_) => "unsupported here — nothing sent",
                _ => "disabled — nothing sent",
            };
            put(buf, area, x0 + 2, y, why, theme.dim());
            continue;
        }
        let shown = c.cells.len().min(usize::from(area.width.saturating_sub(AXIS + TAGS)));
        let at_cursor = cursor.and_then(|off| cursor_column(shown, off));
        for (j, cell) in c.cells.iter().take(shown).enumerate() {
            let (ch, tone) = theme.cell(*cell);
            let style = theme.tone(tone, c.slot);
            put(buf, area, x0 + j as u16, y, &ch.to_string(), if at_cursor == Some(j) { style.add_modifier(Modifier::REVERSED) } else { style });
        }
    }
}

pub fn timeline_legend(theme: &Theme) -> String {
    let g = |c| theme.cell(c).0;
    use pathwatch_core::snapshot::Cell::*;
    format!(
        "{} replied  {} some unanswered  {} none answered  {} error  {} local error  {} gap",
        g(Replied),
        g(SomeUnanswered),
        g(NoneAnswered),
        g(ErrorResponse),
        g(LocalError),
        g(Gap)
    )
}

pub fn events(buf: &mut Buffer, area: Rect, events: &[EventLine], theme: &Theme) {
    if events.is_empty() {
        put(buf, area, area.x + 1, area.y, "no events yet", theme.dim());
    }
    for (i, e) in events.iter().take(usize::from(area.height)).enumerate() {
        let y = area.y + i as u16;
        let time = e.utc.get(11..19).unwrap_or("--:--:--");
        put(buf, area, area.x + 1, y, time, theme.dim());
        let style = match e.kind {
            '▲' => theme.status_style(&Status::Lossy),
            '!' => theme.status_style(&Status::LocalErr),
            _ => Style::default(),
        };
        put(buf, area, area.x + 11, y, &theme.event_symbol(e.kind).to_string(), style);
        // Wide enough for the longest stream label ("cloudflare doh v6 fresh").
        put(buf, area, area.x + 13, y, &format!("{:<24}", e.stream), theme.dim());
        put(buf, area, area.x + 38, y, &ellipsis(&e.text, usize::from(area.right().saturating_sub(area.x + 39))), Style::default());
    }
}

/// `text` cut to `room` characters, ending in `…` when anything was cut, so
/// a clipped line is never mistaken for a whole one.
pub fn ellipsis(text: &str, room: usize) -> String {
    if text.chars().count() <= room {
        return text.to_string();
    }
    text.chars().take(room.saturating_sub(1)).chain(std::iter::once('…')).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ColorMode, Glyphs};
    use pathwatch_core::snapshot::{Cell, Column};

    fn text(buf: &Buffer, area: Rect) -> String {
        (area.y..area.bottom()).map(|y| (area.x..area.right()).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn a_cell_never_holds_a_control_character_or_an_override() {
        let area = Rect::new(0, 0, 12, 1);
        let mut buf = Buffer::empty(area);
        put(&mut buf, area, 0, 0, "a\u{1b}]0;x\u{7}\u{9b}\u{202e}\nz", Style::default());
        assert_eq!(text(&buf, area), "a?]0;x????z ");
    }

    fn inactive(status: Status) -> Card {
        Card {
            slot: 3,
            key: "nextdns".into(),
            name: "NextDNS".into(),
            status,
            stream: None,
            target: String::new(),
            last_us: None,
            last_age_ms: None,
            stats: None,
            columns: vec![Column::default(); 10],
            cells: vec![Cell::Empty; 10],
            other_family: None,
            histogram: Vec::new(),
            session: None,
            raw: Vec::new(),
        }
    }

    #[test]
    fn an_inactive_card_always_says_nothing_is_sent_however_long_the_reason() {
        let theme = Theme { color: ColorMode::True, glyphs: Glyphs::Unicode };
        let long =
            "environment variable PATHWATCH_NEXTDNS_PROFILE is not set, so the profile endpoint cannot be formed and no stream for this provider can be started on this host"
                .to_string();
        for status in [
            Status::NeedsConfig("no target configured; set your endpoint under [providers.nextdns] and enable it".into()),
            Status::NeedsConfig(long.clone()),
            Status::Unsupported(long),
            Status::Disabled,
        ] {
            let area = Rect::new(0, 0, 30, 9);
            let mut buf = Buffer::empty(area);
            card(&mut buf, area, &inactive(status.clone()), CardLook { mode: "ICMP", family: "4", focused: false, tall: false }, &theme);
            let t = text(&buf, area).replace(['│', '\n'], " ");
            let flat: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(flat.contains("Nothing is being sent; not counted as loss."), "{status:?}: {flat}");
            assert!(flat.contains(status.word()), "{flat}");
            assert!(!flat.contains('%'), "no rate on an inactive card: {flat}");
        }
    }

    #[test]
    fn a_long_mode_label_never_runs_into_the_status() {
        let theme = Theme { color: ColorMode::True, glyphs: Glyphs::Unicode };
        // The narrowest card of the standard layout, with the longest status and mode.
        for (mode, wanted) in [("ICMP", "ICMP · v4"), ("DNS/UDP", "v4")] {
            let area = Rect::new(0, 0, 28, 9);
            let mut buf = Buffer::empty(area);
            card(&mut buf, area, &inactive(Status::NeedsConfig("no target configured".into())), CardLook { mode, family: "4", focused: false, tall: false }, &theme);
            let line = text(&buf, area).lines().nth(1).unwrap().to_string();
            assert!(line.contains("NEEDS CONFIG ") && line.trim_end_matches(['│', ' ']).ends_with(wanted), "{line}");
        }
    }

    #[test]
    fn a_long_target_never_runs_into_the_other_family_marker() {
        let theme = Theme { color: ColorMode::True, glyphs: Glyphs::Unicode };
        let mut c = inactive(Status::LocalErr);
        (c.target, c.other_family) = ("https://dns.google/dns-query".into(), Some(Status::WarmingUp));
        let area = Rect::new(0, 0, 28, 9);
        let mut buf = Buffer::empty(area);
        card(&mut buf, area, &c, CardLook { mode: "DoH", family: "6", focused: false, tall: false }, &theme);
        let line = text(&buf, area).lines().nth(2).unwrap().to_string();
        assert!(line.contains("… v4 ") && line.trim_end_matches(['│', ' ']).ends_with(theme.status_symbol(&Status::WarmingUp)), "{line}");
    }

    #[test]
    fn axis_ticks_are_labelled_with_their_real_value() {
        // An 8 ms scale over seven rows: ticks at 8, 5.71…, 3.43…, 1.14….
        let labels: Vec<String> = [0u16, 2, 4, 6].iter().map(|r| axis_label(8.0 * f64::from(7 - r) / 7.0)).collect();
        assert_eq!(labels, ["8", "5.7", "3.4", "1.1"]);
        assert_eq!([axis_label(40.0), axis_label(30.0), axis_label(149.6), axis_label(0.0)], ["40", "30", "150", "0"]);
    }

    #[test]
    fn clipped_text_says_it_was_clipped() {
        assert_eq!(ellipsis("certificate verification failed", 12), "certificate…");
        assert_eq!(ellipsis("short", 12), "short");
        assert_eq!(ellipsis("exactly twelve", 14), "exactly twelve");
    }

    #[test]
    fn series_tags_are_distinct_for_the_four_providers() {
        let tags: Vec<String> = ["Cloudflare", "Google", "NextDNS", "Arelion"].into_iter().map(tag).collect();
        assert_eq!(tags, vec!["CF", "GO", "ND", "AR"]);
    }
}
