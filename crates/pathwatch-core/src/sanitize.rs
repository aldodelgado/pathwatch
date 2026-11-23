//! Untrusted text handling. Names, error strings, and anything that came off
//! the network are cleaned before they are stored, logged, or displayed.

/// Returns `s` limited to `max` characters with control characters, escape
/// sequences, and bidirectional overrides replaced by `\xNN` / `\u{NNNN}`.
pub fn clean(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max));
    for (n, c) in s.chars().enumerate() {
        if n >= max {
            out.push('…');
            break;
        }
        if is_unsafe(c) {
            let v = c as u32;
            if v <= 0xff {
                out.push_str(&format!("\\x{v:02x}"));
            } else {
                out.push_str(&format!("\\u{{{v:04x}}}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Returns `s` with everything [`clean`] replaces replaced the same way,
/// except line breaks and tabs, and with no limit on length. For text that
/// is legitimately several lines long, such as a recorded configuration.
pub fn clean_multiline(s: &str) -> String {
    if !s.chars().any(|c| is_unsafe(c) && c != '\n' && c != '\t') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for line in s.split_inclusive('\n') {
        let (body, end) = line.strip_suffix('\n').map_or((line, ""), |b| (b, "\n"));
        for (n, part) in body.split('\t').enumerate() {
            if n > 0 {
                out.push('\t');
            }
            out.push_str(&clean(part, usize::MAX));
        }
        out.push_str(end);
    }
    out
}

/// Whether a character may not reach a terminal or a log as it is: a control
/// character (C0, DEL, C1) or a bidirectional override.
pub fn is_unsafe(c: char) -> bool {
    let v = c as u32;
    v < 0x20                      // C0, including ESC, CR, LF, TAB
        || v == 0x7f
        || (0x80..=0x9f).contains(&v) // C1
        || matches!(v, 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069) // bidi controls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_escape_sequences_and_bidi() {
        assert_eq!(clean("ok.example.net", 64), "ok.example.net");
        assert_eq!(clean("a\x1b[31mb", 64), "a\\x1b[31mb");
        assert_eq!(clean("x\u{202e}y", 64), "x\\u{202e}y");
        assert_eq!(clean("line\nbreak", 64), "line\\x0abreak");
    }

    #[test]
    fn multiline_text_keeps_its_lines_and_nothing_else() {
        assert_eq!(clean_multiline("[a]\n\tb = 1\n"), "[a]\n\tb = 1\n");
        assert_eq!(clean_multiline("a\x1b]0;x\x07\nb\rc\u{9b}"), "a\\x1b]0;x\\x07\nb\\x0dc\\x9b");
    }

    #[test]
    fn limits_length() {
        assert_eq!(clean("abcdef", 3), "abc…");
    }
}
