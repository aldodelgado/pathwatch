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

fn is_unsafe(c: char) -> bool {
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
    fn limits_length() {
        assert_eq!(clean("abcdef", 3), "abc…");
    }
}
