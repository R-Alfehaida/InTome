pub mod a11y;
pub mod clipboard;
pub mod richedit;

/// `text` with CRLF line breaks. Win32 edit controls and the clipboard treat a
/// lone `\n` as no break at all.
pub fn to_crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn line_breaks_become_crlf_once() {
        assert_eq!(super::to_crlf("a\nb\r\nc"), "a\r\nb\r\nc");
    }
}
