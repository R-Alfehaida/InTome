//! Plain-text clipboard writes.

use winsafe::{self as w, co};

use super::to_crlf;

/// `text` as `CF_UNICODETEXT` wants it: CRLF line breaks, UTF-16LE, NUL-terminated.
pub fn unicode_text(text: &str) -> Vec<u8> {
    to_crlf(text).encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect()
}

/// Replaces the clipboard contents with `text`, owned by window `owner`.
pub fn set_text(owner: isize, text: &str) -> anyhow::Result<()> {
    let hwnd = unsafe { w::HWND::from_ptr(owner as _) };
    let clip = hwnd.OpenClipboard()?;
    clip.EmptyClipboard()?;
    clip.SetClipboardData(co::CF::UNICODETEXT, &unicode_text(text))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::unicode_text;

    #[test]
    fn text_becomes_crlf_utf16_with_terminator() {
        assert_eq!(unicode_text("a\nb\r\nc"), [b'a', 0, b'\r', 0, b'\n', 0, b'b', 0, b'\r', 0, b'\n', 0, b'c', 0, 0, 0]);
        assert_eq!(unicode_text("é"), [0xE9, 0, 0, 0]);
    }
}
