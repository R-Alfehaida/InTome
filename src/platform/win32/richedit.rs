//! Native RICHEDIT50W (Msftedit.dll) reading control. Port of
//! prism/platform/win32/richedit.py.
//!
//! Screen readers (NVDA, JAWS, Narrator) read this control line by line
//! natively. It is created read-only with `ES_NOHIDESEL`, so the selection
//! stays visible when focus leaves it, and announces value changes after every
//! text replace.
//!
//! The control counts positions in UTF-16 units and stores every line break
//! (`\r\n`, `\r` or `\n`) as one unit. The core counts char indices into
//! `Page::text`. [`char_to_offset`] and [`offset_to_char`] convert at this
//! boundary; nothing else in the app sees control offsets.

use std::sync::OnceLock;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::LoadLibraryW;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, HMENU, SendMessageW, WINDOW_STYLE, WS_CHILD, WS_EX_CLIENTEDGE, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::w;

use super::a11y;

const ES_MULTILINE: u32 = 0x4;
const ES_AUTOVSCROLL: u32 = 0x40;
const ES_NOHIDESEL: u32 = 0x100;
const ES_READONLY: u32 = 0x800;
const ES_WANTRETURN: u32 = 0x1000;
const WM_USER: u32 = 0x400;
const EM_SCROLLCARET: u32 = 0xB7;
const EM_LINEINDEX: u32 = 0xBB;
const EM_EXGETSEL: u32 = WM_USER + 52;
const EM_EXLIMITTEXT: u32 = WM_USER + 53;
const EM_EXLINEFROMCHAR: u32 = WM_USER + 54;
const EM_EXSETSEL: u32 = WM_USER + 55;
const EM_SETCHARFORMAT: u32 = WM_USER + 68;
const EM_SETEVENTMASK: u32 = WM_USER + 69;
const EM_SETTEXTEX: u32 = WM_USER + 97;
const ENM_SELCHANGE: isize = 0x0008_0000;
const SCF_ALL: usize = 4;
const CFM_SIZE: u32 = 0x8000_0000;
const CFM_FACE: u32 = 0x2000_0000;
const ST_UNICODE: u32 = 8;
const WM_NCDESTROY: u32 = 0x82;

/// `EN_SELCHANGE`, sent to the parent in a `SELCHANGE` block.
pub const EN_SELCHANGE: i32 = 0x0702;

#[repr(C)]
struct SetTextEx {
    flags: u32,
    codepage: u32,
}

#[repr(C)]
#[derive(Default)]
struct CharRange {
    cp_min: i32,
    cp_max: i32,
}

#[repr(C)]
struct CharFormatW {
    cb_size: u32,
    dw_mask: u32,
    dw_effects: u32,
    y_height: i32,
    y_offset: i32,
    cr_text_color: u32,
    b_char_set: u8,
    b_pitch_and_family: u8,
    sz_face_name: [u16; 32],
}

/// The `SELCHANGE` notification block behind an `EN_SELCHANGE` `WM_NOTIFY`.
#[repr(C)]
pub struct SelChange {
    pub hwnd_from: isize,
    pub id_from: usize,
    pub code: i32,
    cp_min: i32,
    cp_max: i32,
    pub seltyp: u16,
}

impl SelChange {
    /// `(cp_min, cp_max)` in control offsets.
    pub fn range(&self) -> (usize, usize) {
        (self.cp_min.max(0) as usize, self.cp_max.max(0) as usize)
    }
}

/// Handles a message sent to a subclassed control. `Some(result)` consumes
/// it; `None` lets the control's own window procedure run.
pub type MessageHook = Box<dyn Fn(u32, usize, isize) -> Option<isize>>;

fn load_msftedit() -> bool {
    static LOADED: OnceLock<bool> = OnceLock::new();
    *LOADED.get_or_init(|| unsafe { LoadLibraryW(w!("Msftedit.dll")) }.is_ok())
}

/// A RICHEDIT50W child window, addressed by its HWND.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RichEdit(pub isize);

impl RichEdit {
    /// Creates a read-only reading control on `parent`. `id` is its control ID,
    /// which `WM_NOTIFY` (`EN_SELCHANGE`) carries in `idFrom`.
    pub fn create(parent: isize, id: u16) -> anyhow::Result<Self> {
        anyhow::ensure!(load_msftedit(), "Msftedit.dll could not be loaded");
        let style = WS_CHILD
            | WS_VISIBLE
            | WS_VSCROLL
            | WS_TABSTOP
            | WINDOW_STYLE(ES_MULTILINE | ES_AUTOVSCROLL | ES_NOHIDESEL | ES_WANTRETURN | ES_READONLY);
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_CLIENTEDGE,
                w!("RICHEDIT50W"),
                w!(""),
                style,
                0,
                0,
                1,
                1,
                Some(HWND(parent as _)),
                Some(HMENU(id as usize as _)),
                None,
                None,
            )?
        };
        let edit = Self(hwnd.0 as isize);
        // Without this, text past 32K characters is silently dropped.
        edit.send(EM_EXLIMITTEXT, 0, 0x7FFF_FFFE);
        edit.send(EM_SETEVENTMASK, 0, ENM_SELCHANGE);
        Ok(edit)
    }

    fn hwnd(self) -> HWND {
        HWND(self.0 as _)
    }

    fn send(self, msg: u32, wparam: usize, lparam: isize) -> isize {
        unsafe { SendMessageW(self.hwnd(), msg, Some(WPARAM(wparam)), Some(LPARAM(lparam))).0 }
    }

    /// Replaces the whole text and tells screen readers the value changed;
    /// without the notification NVDA and JAWS keep reading the old page.
    pub fn set_text(self, text: &str) {
        // A NUL would end the string early; a space keeps every offset in place.
        let mut wide: Vec<u16> = text.replace('\0', " ").encode_utf16().collect();
        wide.push(0);
        let st = SetTextEx { flags: ST_UNICODE, codepage: 1200 };
        self.send(EM_SETTEXTEX, &st as *const _ as usize, wide.as_ptr() as isize);
        a11y::notify_value_change(self.0);
    }

    /// `(start, end)` of the selection in control offsets.
    pub fn selection(self) -> (usize, usize) {
        let mut cr = CharRange::default();
        self.send(EM_EXGETSEL, 0, &mut cr as *mut _ as isize);
        (cr.cp_min.max(0) as usize, cr.cp_max.max(0) as usize)
    }

    /// Selects `start..end` (control offsets) and scrolls it into view.
    pub fn set_selection(self, start: usize, end: usize) {
        let cr = CharRange { cp_min: start as i32, cp_max: end as i32 };
        self.send(EM_EXSETSEL, 0, &cr as *const _ as isize);
        self.send(EM_SCROLLCARET, 0, 0);
    }

    /// 0-based (wrapped) line holding control offset `offset`.
    pub fn line_from_offset(self, offset: usize) -> usize {
        self.send(EM_EXLINEFROMCHAR, 0, offset as isize).max(0) as usize
    }

    /// Control offset where (wrapped) line `line` starts.
    pub fn line_start(self, line: usize) -> usize {
        self.send(EM_LINEINDEX, line, 0).max(0) as usize
    }

    pub fn set_font(self, face: &str, size_pt: u32) {
        let mut cf = CharFormatW {
            cb_size: std::mem::size_of::<CharFormatW>() as u32,
            dw_mask: CFM_SIZE | CFM_FACE,
            dw_effects: 0,
            y_height: (size_pt * 20) as i32, // twips
            y_offset: 0,
            cr_text_color: 0,
            b_char_set: 0,
            b_pitch_and_family: 0,
            sz_face_name: [0; 32],
        };
        for (dst, src) in cf.sz_face_name.iter_mut().zip(face.encode_utf16().take(31)) {
            *dst = src;
        }
        self.send(EM_SETCHARFORMAT, SCF_ALL, &cf as *const _ as isize);
    }

    /// Routes every message the control receives through `hook` first. The
    /// hook lives until the control is destroyed.
    pub fn subclass(self, hook: MessageHook) {
        let data = Box::into_raw(Box::new(hook));
        unsafe {
            let _ = SetWindowSubclass(self.hwnd(), Some(subclass_proc), 1, data as usize);
        }
    }
}

unsafe extern "system" fn subclass_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, id: usize, data: usize) -> LRESULT {
    let hook = data as *mut MessageHook;
    if msg == WM_NCDESTROY {
        unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(subclass_proc), id);
            drop(Box::from_raw(hook));
            return DefSubclassProc(hwnd, msg, wparam, lparam);
        }
    }
    if let Some(result) = unsafe { (*hook)(msg, wparam.0, lparam.0) } {
        return LRESULT(result);
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// Control units taken by each char: its UTF-16 length, except the `\n` of a
/// `\r\n` pair, which shares the pair's single unit.
fn units(text: &str) -> impl Iterator<Item = usize> + '_ {
    let mut prev_cr = false;
    text.chars().map(move |c| {
        let w = if prev_cr && c == '\n' { 0 } else { c.len_utf16() };
        prev_cr = c == '\r';
        w
    })
}

/// Control offset of char index `ch` in `text` (clamped to the end).
pub fn char_to_offset(text: &str, ch: usize) -> usize {
    units(text).take(ch).sum()
}

/// Char index in `text` at control offset `offset`. An offset inside a
/// surrogate pair rounds up to the next char; past the end gives the char count.
pub fn offset_to_char(text: &str, offset: usize) -> usize {
    let mut at = 0;
    let mut n = 0;
    for w in units(text) {
        if at >= offset && w > 0 {
            return n;
        }
        at += w;
        n += 1;
    }
    n
}

/// Length of `text` in control offsets.
pub fn offset_len(text: &str) -> usize {
    units(text).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_counts_as_one_unit_both_ways() {
        let text = "ab\r\ncd\ne";
        assert_eq!(char_to_offset(text, 2), 2); // the \r
        assert_eq!(char_to_offset(text, 4), 3); // 'c', after the one-unit \r\n
        assert_eq!(char_to_offset(text, 7), 6); // 'e', lone \n is one unit
        assert_eq!(offset_to_char(text, 3), 4);
        assert_eq!(offset_to_char(text, 6), 7);
        assert_eq!(offset_len(text), 7);
    }

    #[test]
    fn astral_chars_take_two_units() {
        let text = "a\u{1F600}b";
        assert_eq!(char_to_offset(text, 2), 3);
        assert_eq!(offset_to_char(text, 3), 2);
        assert_eq!(offset_to_char(text, 2), 2); // mid-surrogate rounds up to 'b'
        assert_eq!(offset_len(text), 4);
    }

    #[test]
    fn offsets_past_the_end_clamp() {
        assert_eq!(char_to_offset("xy", 10), 2);
        assert_eq!(offset_to_char("x\r\n", 99), 3);
    }
}
