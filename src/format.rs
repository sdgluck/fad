/// Base-1024 sizes with the unit suffixes `du -h` uses.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "K", "M", "G", "T", "P", "E"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes}B")
    } else if v < 10.0 {
        format!("{v:.1}{}", UNITS[i])
    } else {
        format!("{v:.0}{}", UNITS[i])
    }
}

/// Text that came from the filesystem or a tool, made safe to print to a
/// terminal: every control character — C0, DEL and C1 — written out as an
/// escape (`\n`, `\u{1b}`) and everything else left alone.
///
/// A filename is bytes chosen by whoever made the file. One holding an escape
/// sequence, printed raw in a listing of what is about to be deleted, can
/// clear the line, move the cursor, or recolour the rest of the output — and
/// so make the list a person is approving say something other than what it
/// does. Only controls are touched: a backslash or a quote in a name is just
/// a character, and escaping it would print a path that does not exist.
pub fn escape_controls(s: &str) -> String {
    if !s.chars().any(char::is_control) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if c.is_control() {
            out.extend(c.escape_debug());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::escape_controls;

    #[test]
    fn controls_are_written_out_and_nothing_else_is() {
        assert_eq!(escape_controls("plain/name.txt"), "plain/name.txt");
        assert_eq!(escape_controls("a\\b \"q\" é 日本"), "a\\b \"q\" é 日本");
        assert_eq!(escape_controls("x\ny"), "x\\ny");
        assert_eq!(escape_controls("\u{1b}[2Jgone"), "\\u{1b}[2Jgone");
        assert_eq!(escape_controls("del\u{7f}"), "del\\u{7f}");
        assert_eq!(escape_controls("c1\u{9b}"), "c1\\u{9b}");
    }
}
