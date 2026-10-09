//! Inline find bar shown below the reading area (Ctrl+F; F3 / Shift+F3;
//! Escape closes). Port of prism/gui/find_bar.py. The main window owns the
//! search itself; this holds the controls and the "query changed" state.

use std::cell::{Cell, RefCell};

use winsafe::{self as w, co, gui, prelude::*};

use crate::platform::win32::a11y;

const H: i32 = 23;

pub struct FindBar {
    label: gui::Label,
    pub input: gui::Edit,
    pub prev: gui::Button,
    pub next: gui::Button,
    pub case: gui::CheckBox,
    pub wrap: gui::CheckBox,
    count: gui::Label,
    pub close: gui::Button,
    shown: Cell<bool>,
    /// (query, match case, wrap) of the last search run.
    last: RefCell<(String, bool, bool)>,
}

impl FindBar {
    /// Creates the controls on `parent`, hidden. Call [`Self::name_controls`]
    /// once the parent exists.
    pub fn new(parent: &gui::WindowMain) -> Self {
        let hidden = co::WS::CHILD | co::WS::TABSTOP | co::WS::GROUP;
        let button = |text| {
            gui::Button::new(parent, gui::ButtonOpts { text, width: gui::dpi_x(80), window_style: hidden, ..Default::default() })
        };
        let check = |text, checked: bool| {
            gui::CheckBox::new(parent, gui::CheckBoxOpts {
                text,
                size: gui::dpi(100, H),
                check_state: if checked { co::BST::CHECKED } else { co::BST::UNCHECKED },
                window_style: hidden,
                ..Default::default()
            })
        };
        let label = |text| gui::Label::new(parent, gui::LabelOpts { text, size: gui::dpi(40, H), window_style: co::WS::CHILD, ..Default::default() });
        Self {
            label: label("Find:"),
            input: gui::Edit::new(parent, gui::EditOpts { control_style: co::ES::AUTOHSCROLL | co::ES::NOHIDESEL, window_style: hidden, ..Default::default() }),
            prev: button("Previous"),
            next: button("Next"),
            case: check("Match case", false),
            wrap: check("Wrap around", true),
            count: label(""),
            close: button("Close"),
            shown: Cell::new(false),
            last: RefCell::default(),
        }
    }

    /// Prism's accessible names; the captions alone are ambiguous out of context.
    pub fn name_controls(&self) {
        let p = |c: &dyn GuiWindow| c.hwnd().ptr() as isize;
        a11y::set_name(p(&self.input), "Find in document");
        a11y::set_name(p(&self.prev), "Previous match");
        a11y::set_name(p(&self.next), "Next match");
        a11y::set_name(p(&self.close), "Close find bar");
    }

    fn hwnds(&self) -> [&w::HWND; 8] {
        [
            self.label.hwnd(),
            self.input.hwnd(),
            self.prev.hwnd(),
            self.next.hwnd(),
            self.case.hwnd(),
            self.wrap.hwnd(),
            self.count.hwnd(),
            self.close.hwnd(),
        ]
    }

    /// Lays the bar out as one row at (`x`, `y`), `width` pixels wide.
    pub fn layout(&self, x: i32, y: i32, width: i32) {
        let (h, gap) = (gui::dpi_y(H), gui::dpi_x(6));
        let fixed = [40, 0, 80, 80, 100, 110, 100, 80].map(gui::dpi_x);
        let input_w = (width - fixed.iter().sum::<i32>() - gap * 7).max(gui::dpi_x(120));
        let mut cx = x;
        for (hwnd, fw) in self.hwnds().into_iter().zip(fixed) {
            let cw = if fw == 0 { input_w } else { fw };
            let dy = if fw == fixed[0] || fw == fixed[6] { gui::dpi_y(4) } else { 0 };
            let _ = hwnd.SetWindowPos(w::HwndPlace::None, w::POINT::with(cx, y + dy), w::SIZE::with(cw, h), co::SWP::NOZORDER);
            cx += cw + gap;
        }
    }

    pub fn is_shown(&self) -> bool {
        self.shown.get()
    }

    pub fn set_shown(&self, shown: bool) {
        self.shown.set(shown);
        for hwnd in self.hwnds() {
            hwnd.ShowWindow(if shown { co::SW::SHOW } else { co::SW::HIDE });
        }
    }

    /// True when `hwnd` is one of the bar's controls.
    pub fn contains(&self, hwnd: &w::HWND) -> bool {
        self.hwnds().iter().any(|h| *h == hwnd)
    }

    /// Focuses the query box with its text selected.
    pub fn focus_input(&self) {
        let _ = self.input.focus();
        self.input.set_selection(0, -1);
    }

    pub fn query(&self) -> String {
        self.input.text().unwrap_or_default()
    }

    pub fn case_sensitive(&self) -> bool {
        self.case.is_checked()
    }

    pub fn wrap_around(&self) -> bool {
        self.wrap.is_checked()
    }

    /// "3 of 17", or "No matches".
    pub fn set_current_match(&self, current: usize, total: usize) {
        let text = if total == 0 { "No matches".to_string() } else { format!("{current} of {total}") };
        let _ = self.count.hwnd().SetWindowText(&text);
    }

    /// True (and remembered) when the query or an option changed since the last search.
    pub fn has_query_changed(&self) -> bool {
        let now = (self.query(), self.case_sensitive(), self.wrap_around());
        let changed = *self.last.borrow() != now;
        if changed {
            *self.last.borrow_mut() = now;
        }
        changed
    }
}
