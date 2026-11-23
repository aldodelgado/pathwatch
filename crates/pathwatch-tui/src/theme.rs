//! Colour depth, glyph sets, and the status vocabulary.
//!
//! Colour means identity: a provider keeps its colour in every view. Health is
//! carried by a symbol and a word, so it survives monochrome and never fights
//! the identity colour. See `docs/design/design.md` §9.4–9.5.

use pathwatch_core::snapshot::{Cell, Status};
use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    True,
    C256,
    C16,
    Mono,
    /// The high-contrast palette: bright primaries in bold, white text on
    /// black whatever the terminal's own colours are, and nothing dimmed.
    /// It uses only the sixteen colours every colour terminal has.
    HighContrast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyphs {
    Unicode,
    Ascii,
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub color: ColorMode,
    pub glyphs: Glyphs,
}

impl Theme {
    /// Chooses a colour depth from the environment. `NO_COLOR` (non-empty)
    /// removes colour but keeps bold, dim, and reverse; colours are mapped by
    /// table at each depth, never by automatic nearest-match.
    pub fn detect_color(env: &dyn Fn(&str) -> Option<String>) -> ColorMode {
        if env("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return ColorMode::Mono;
        }
        if let Some("truecolor" | "24bit") = env("COLORTERM").as_deref() {
            return ColorMode::True;
        }
        match env("TERM") {
            Some(t) if t.contains("256color") => ColorMode::C256,
            _ => ColorMode::C16,
        }
    }

    pub fn is_mono(&self) -> bool {
        self.color == ColorMode::Mono
    }

    /// The identity colour of a provider slot (Okabe–Ito palette).
    pub fn provider(&self, slot: u8) -> Style {
        let i = usize::from(slot.clamp(1, 4)) - 1;
        let color = match self.color {
            ColorMode::True => [Color::Rgb(0xE6, 0x9F, 0x00), Color::Rgb(0x56, 0xB4, 0xE9), Color::Rgb(0x00, 0x9E, 0x73), Color::Rgb(0xCC, 0x79, 0xA7)][i],
            ColorMode::C256 => [Color::Indexed(214), Color::Indexed(75), Color::Indexed(36), Color::Indexed(175)][i],
            ColorMode::C16 => [Color::Yellow, Color::LightBlue, Color::Cyan, Color::Magenta][i],
            ColorMode::Mono => return Style::default(),
            ColorMode::HighContrast => return Style::default().fg([Color::LightYellow, Color::LightCyan, Color::LightGreen, Color::LightMagenta][i]).add_modifier(Modifier::BOLD),
        };
        Style::default().fg(color)
    }

    /// Quieter text. High contrast has none: what is on screen is at full
    /// strength or not there.
    pub fn dim(&self) -> Style {
        if self.color == ColorMode::HighContrast { Style::default() } else { Style::default().add_modifier(Modifier::DIM) }
    }

    /// The last pass over a frame in high contrast: every cell is given an
    /// explicit white on black where it had the terminal's own colours, and
    /// loses any dimming, so the result does not depend on the terminal's
    /// palette. Reversed cells become black on white.
    pub fn harden(&self, buf: &mut ratatui::buffer::Buffer, area: ratatui::layout::Rect) {
        if self.color != ColorMode::HighContrast {
            return;
        }
        let area = area.intersection(buf.area);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let cell = &mut buf[(x, y)];
                if cell.fg == Color::Reset {
                    cell.fg = Color::White;
                }
                if cell.bg == Color::Reset {
                    cell.bg = Color::Black;
                }
                cell.modifier.remove(Modifier::DIM);
            }
        }
    }

    pub fn bold(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    pub fn border(&self, focused: bool) -> Style {
        if focused { self.bold() } else { self.dim() }
    }

    fn warn(&self) -> Style {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        match self.color {
            ColorMode::True => bold.fg(Color::Rgb(0xF0, 0xE4, 0x42)),
            ColorMode::C256 => bold.fg(Color::Indexed(227)),
            ColorMode::C16 => bold.fg(Color::LightYellow),
            ColorMode::Mono => bold,
            // A filled badge, so it cannot be taken for a provider's colour.
            ColorMode::HighContrast => bold.fg(Color::Black).bg(Color::LightYellow),
        }
    }

    fn fail(&self) -> Style {
        let rev = Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);
        match self.color {
            ColorMode::True => rev.fg(Color::Rgb(0xD5, 0x5E, 0x00)),
            ColorMode::C256 => rev.fg(Color::Indexed(166)),
            ColorMode::C16 => rev.fg(Color::Red),
            ColorMode::Mono => rev,
            ColorMode::HighContrast => Style::default().add_modifier(Modifier::BOLD).fg(Color::White).bg(Color::Red),
        }
    }

    /// The style of a status badge. `OK` is deliberately the default
    /// foreground: there is no green-means-good to collide with a provider.
    pub fn status_style(&self, s: &Status) -> Style {
        match s {
            Status::Ok => Style::default(),
            Status::WarmingUp | Status::Stale | Status::Disabled => self.dim(),
            Status::Lossy | Status::Slow | Status::Errors | Status::LocalErr | Status::NeedsConfig(_) | Status::Unsupported(_) => self.warn(),
            Status::NoReply => self.fail(),
        }
    }

    pub fn status_symbol(&self, s: &Status) -> &'static str {
        let (unicode, ascii) = match s {
            Status::Ok => ("●", "*"),
            Status::WarmingUp => ("◐", "~"),
            Status::Lossy | Status::Slow | Status::Errors => ("▲", "^"),
            Status::NoReply => ("✖", "x"),
            Status::LocalErr => ("!", "!"),
            Status::Stale => ("◌", "o"),
            Status::NeedsConfig(_) => ("?", "?"),
            Status::Unsupported(_) => ("⊘", "/"),
            Status::Disabled => ("–", "-"),
        };
        if self.glyphs == Glyphs::Unicode { unicode } else { ascii }
    }

    pub fn event_symbol(&self, kind: char) -> char {
        if self.glyphs == Glyphs::Unicode {
            return kind;
        }
        match kind {
            '▲' => '^',
            '●' => '*',
            '╌' => '~',
            '◆' => '#',
            other if other.is_ascii() => other,
            _ => '.',
        }
    }

    /// The timeline glyph for a column, and whether it deserves emphasis.
    pub fn cell(&self, c: Cell) -> (char, CellTone) {
        let u = self.glyphs == Glyphs::Unicode;
        match c {
            Cell::Empty => (' ', CellTone::Quiet),
            Cell::Replied => (if u { '·' } else { '.' }, CellTone::Identity),
            Cell::SomeUnanswered => (if u { '▒' } else { 'o' }, CellTone::Warn),
            Cell::NoneAnswered => (if u { '█' } else { '#' }, CellTone::Fail),
            Cell::ErrorResponse => ('E', CellTone::Warn),
            Cell::LocalError => ('!', CellTone::Warn),
            Cell::Skipped => (if u { '┄' } else { '_' }, CellTone::Quiet),
            Cell::Gap => (if u { '╌' } else { '~' }, CellTone::Quiet),
        }
    }

    pub fn tone(&self, tone: CellTone, slot: u8) -> Style {
        match tone {
            CellTone::Quiet => self.dim(),
            CellTone::Identity => self.provider(slot).add_modifier(Modifier::DIM),
            // On the outcome strip a mark is a glyph, not a badge: in high
            // contrast it is drawn in the colour, without the filled ground.
            CellTone::Warn if self.color == ColorMode::HighContrast => Style::default().fg(Color::LightYellow).add_modifier(Modifier::BOLD),
            CellTone::Fail if self.color == ColorMode::HighContrast => Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD),
            CellTone::Warn => self.warn(),
            // Not reversed: a run of full blocks is already unmistakable.
            CellTone::Fail => self.fail().remove_modifier(Modifier::REVERSED),
        }
    }

    pub fn spark_levels(&self) -> &'static [char] {
        if self.glyphs == Glyphs::Unicode { &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'] } else { &['.', ':', '-', '=', '+', '*', '#', '#'] }
    }

    /// (horizontal, vertical, top-left, top-right, bottom-left, bottom-right)
    pub fn box_chars(&self) -> [char; 6] {
        if self.glyphs == Glyphs::Unicode { ['─', '│', '╭', '╮', '╰', '╯'] } else { ['-', '|', '+', '+', '+', '+'] }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellTone {
    Quiet,
    Identity,
    Warn,
    Fail,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn colour_depth_follows_the_environment() {
        assert_eq!(Theme::detect_color(&env(&[("COLORTERM", "truecolor"), ("TERM", "xterm-256color")])), ColorMode::True);
        assert_eq!(Theme::detect_color(&env(&[("TERM", "xterm-256color")])), ColorMode::C256);
        assert_eq!(Theme::detect_color(&env(&[("TERM", "xterm")])), ColorMode::C16);
        assert_eq!(Theme::detect_color(&env(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")])), ColorMode::Mono);
        assert_eq!(Theme::detect_color(&env(&[("NO_COLOR", ""), ("COLORTERM", "truecolor")])), ColorMode::True, "an empty NO_COLOR is not set");
    }

    #[test]
    fn high_contrast_dims_nothing_and_keeps_badges_apart_from_providers() {
        let hc = Theme { color: ColorMode::HighContrast, glyphs: Glyphs::Unicode };
        // Four providers, four different bright colours, all bold.
        let providers: Vec<Style> = (1..=4).map(|slot| hc.provider(slot)).collect();
        let colours: std::collections::BTreeSet<String> = providers.iter().map(|s| format!("{:?}", s.fg)).collect();
        assert_eq!(colours.len(), 4);
        assert!(providers.iter().all(|s| s.add_modifier.contains(Modifier::BOLD) && s.bg.is_none()));
        // Nothing is asked to be dim.
        assert!(!hc.dim().add_modifier.contains(Modifier::DIM) && !hc.border(false).add_modifier.contains(Modifier::DIM));
        assert!(!hc.status_style(&Status::WarmingUp).add_modifier.contains(Modifier::DIM));
        // Badges are filled grounds, which no provider has; warning and failure differ.
        let (warn, fail) = (hc.status_style(&Status::Lossy), hc.status_style(&Status::NoReply));
        assert!(warn.bg.is_some() && fail.bg.is_some() && warn.bg != fail.bg);
        // On the outcome strip the marks are coloured glyphs without a ground.
        assert!(hc.tone(CellTone::Fail, 1).bg.is_none() && hc.tone(CellTone::Warn, 1).bg.is_none());

        // The last pass: explicit white on black, and whatever was dimmed is not.
        let area = ratatui::layout::Rect::new(0, 0, 3, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        buf[(0, 0)].set_style(Style::default().add_modifier(Modifier::DIM));
        buf[(1, 0)].set_style(hc.provider(2).add_modifier(Modifier::DIM));
        buf[(2, 0)].set_style(fail);
        hc.harden(&mut buf, area);
        assert_eq!((buf[(0, 0)].fg, buf[(0, 0)].bg), (Color::White, Color::Black));
        assert_eq!((buf[(1, 0)].fg, buf[(1, 0)].bg), (Color::LightCyan, Color::Black), "a colour that was set is kept");
        assert_eq!((buf[(2, 0)].fg, buf[(2, 0)].bg), (Color::White, Color::Red));
        assert!((0..3).all(|x| !buf[(x, 0)].modifier.contains(Modifier::DIM)));
        // Every other palette is left exactly as drawn.
        let mut plain = ratatui::buffer::Buffer::empty(area);
        plain[(0, 0)].set_style(Style::default().add_modifier(Modifier::DIM));
        Theme { color: ColorMode::True, glyphs: Glyphs::Unicode }.harden(&mut plain, area);
        assert_eq!((plain[(0, 0)].fg, plain[(0, 0)].bg), (Color::Reset, Color::Reset));
        assert!(plain[(0, 0)].modifier.contains(Modifier::DIM));
    }

    #[test]
    fn every_status_has_a_distinct_word_and_an_ascii_symbol() {
        let all = [
            Status::Ok,
            Status::WarmingUp,
            Status::Lossy,
            Status::Slow,
            Status::Errors,
            Status::NoReply,
            Status::LocalErr,
            Status::Stale,
            Status::NeedsConfig(String::new()),
            Status::Unsupported(String::new()),
            Status::Disabled,
        ];
        let words: std::collections::BTreeSet<&str> = all.iter().map(Status::word).collect();
        assert_eq!(words.len(), all.len());
        let ascii = Theme { color: ColorMode::Mono, glyphs: Glyphs::Ascii };
        assert!(all.iter().all(|s| ascii.status_symbol(s).is_ascii()));
        let cells = [Cell::Replied, Cell::SomeUnanswered, Cell::NoneAnswered, Cell::ErrorResponse, Cell::LocalError, Cell::Skipped, Cell::Gap];
        let glyphs: std::collections::BTreeSet<char> = cells.iter().map(|c| ascii.cell(*c).0).collect();
        assert_eq!(glyphs.len(), cells.len(), "every outcome keeps its own glyph in ASCII");
        assert!(glyphs.iter().all(char::is_ascii));
    }
}
