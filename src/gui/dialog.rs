//! Builder for accessible modal dialogs (winsafe has no sizers).
//!
//! Rows stack top to bottom in creation order, which is also tab order and
//! z-order. Each labelled control is created right after its static label, so
//! MSAA (and so NVDA, JAWS, Narrator) names the control from the label.
//! Escape and the Cancel button close the dialog; Enter and the OK button run
//! the `on_ok` validator and close it on success.
//!
//! ```no_run
//! # use intome::gui::dialog::DialogBuilder;
//! # fn f(parent: &winsafe::gui::WindowMain) {
//! let d = DialogBuilder::new("Go to page", 360);
//! let page = d.edit("&Page number:", "1");
//! d.ok_cancel("OK");
//! if d.run(parent) { let _ = page.text(); }
//! # }
//! ```

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use winsafe::{self as w, co, gui, prelude::*};

pub const MARGIN: i32 = 12;
pub const GAP: i32 = 8;
pub const ROW_H: i32 = 23;
pub const LABEL_W: i32 = 150;
pub const BUTTON_W: i32 = 88;
pub const IDOK: u16 = 1;
pub const IDCANCEL: u16 = 2;

type Validator = Box<dyn Fn() -> bool>;

pub struct DialogBuilder {
    pub wnd: gui::WindowModal,
    width: i32,
    y: Rc<Cell<i32>>,
    accepted: Rc<Cell<bool>>,
    validator: Rc<RefCell<Option<Validator>>>,
}

impl DialogBuilder {
    /// A dialog whose client area is `width` logical pixels wide; height grows with rows.
    pub fn new(title: &str, width: i32) -> Self {
        let wnd = gui::WindowModal::new(gui::WindowModalOpts {
            title,
            size: gui::dpi(width, 200),
            ..Default::default()
        });
        let this = Self {
            wnd,
            width,
            y: Rc::new(Cell::new(MARGIN)),
            accepted: Rc::new(Cell::new(false)),
            validator: Rc::new(RefCell::new(None)),
        };
        this.wire();
        this
    }

    fn wire(&self) {
        let (wnd, y, width) = (self.wnd.clone(), self.y.clone(), self.width);
        self.wnd.on().wm_create(move |_| {
            fit_to_content(wnd.hwnd(), width, y.get() + MARGIN);
            Ok(0)
        });
        // winsafe hands focus to the first child, which may be a static label;
        // a screen-reader user must land on the first actionable control instead.
        let wnd = self.wnd.clone();
        self.wnd.on().wm_set_focus(move |_| {
            if let Ok(first) = wnd.hwnd().GetNextDlgTabItem(&w::HWND::NULL, false) {
                first.SetFocus();
            }
            Ok(())
        });
        let (wnd, accepted, validator) = (self.wnd.clone(), self.accepted.clone(), self.validator.clone());
        self.wnd.on().wm_command_acc_menu(IDOK, move || {
            if validator.borrow().as_ref().is_none_or(|v| v()) {
                accepted.set(true);
                wnd.close();
            }
            Ok(())
        });
        let wnd = self.wnd.clone();
        self.wnd.on().wm_command_acc_menu(IDCANCEL, move || {
            wnd.close();
            Ok(())
        });
    }

    /// Reserves `h` logical pixels and returns the row's top y.
    fn take(&self, h: i32) -> i32 {
        let top = self.y.get();
        self.y.set(top + h + GAP);
        top
    }

    pub fn content_width(&self) -> i32 {
        self.width - 2 * MARGIN
    }

    /// Adds vertical space.
    pub fn space(&self, h: i32) {
        self.y.set(self.y.get() + h);
    }

    /// A static text line (status or explanation), read by screen readers in dialog review.
    pub fn text(&self, text: &str) -> gui::Label {
        let y = self.take(ROW_H);
        gui::Label::new(&self.wnd, gui::LabelOpts {
            text,
            position: gui::dpi(MARGIN, y + 3),
            size: gui::dpi(self.content_width(), ROW_H - 3),
            ..Default::default()
        })
    }

    fn side_label(&self, label: &str, y: i32) -> gui::Label {
        gui::Label::new(&self.wnd, gui::LabelOpts {
            text: label,
            position: gui::dpi(MARGIN, y + 3),
            size: gui::dpi(LABEL_W - GAP, ROW_H - 3),
            ..Default::default()
        })
    }

    fn top_label(&self, label: &str) -> gui::Label {
        let y = self.take(ROW_H - GAP);
        gui::Label::new(&self.wnd, gui::LabelOpts {
            text: label,
            position: gui::dpi(MARGIN, y),
            size: gui::dpi(self.content_width(), ROW_H - GAP),
            ..Default::default()
        })
    }

    /// Single-line edit with a label on its left. `style` extras: `co::ES::PASSWORD`, `co::ES::NUMBER`.
    pub fn edit_with(&self, label: &str, value: &str, style: co::ES) -> gui::Edit {
        let y = self.take(ROW_H);
        self.side_label(label, y);
        gui::Edit::new(&self.wnd, gui::EditOpts {
            text: value,
            position: gui::dpi(MARGIN + LABEL_W, y),
            width: gui::dpi_x(self.content_width() - LABEL_W),
            control_style: co::ES::AUTOHSCROLL | co::ES::NOHIDESEL | style,
            ..Default::default()
        })
    }

    pub fn edit(&self, label: &str, value: &str) -> gui::Edit {
        self.edit_with(label, value, co::ES::NoValue)
    }

    /// Multi-line edit with its label above.
    pub fn multiline(&self, label: &str, value: &str, height: i32, read_only: bool) -> gui::Edit {
        self.top_label(label);
        let y = self.take(height);
        let ro = if read_only { co::ES::READONLY } else { co::ES::NoValue };
        gui::Edit::new(&self.wnd, gui::EditOpts {
            text: value,
            position: gui::dpi(MARGIN, y),
            width: gui::dpi_x(self.content_width()),
            height: gui::dpi_y(height),
            control_style: co::ES::MULTILINE | co::ES::WANTRETURN | co::ES::AUTOVSCROLL | co::ES::NOHIDESEL | ro,
            window_style: co::WS::CHILD | co::WS::VISIBLE | co::WS::TABSTOP | co::WS::GROUP | co::WS::VSCROLL,
            ..Default::default()
        })
    }

    pub fn check(&self, text: &str, checked: bool) -> gui::CheckBox {
        let y = self.take(ROW_H);
        gui::CheckBox::new(&self.wnd, gui::CheckBoxOpts {
            text,
            position: gui::dpi(MARGIN, y + 2),
            size: gui::dpi(self.content_width(), ROW_H - 4),
            check_state: if checked { co::BST::CHECKED } else { co::BST::UNCHECKED },
            ..Default::default()
        })
    }

    /// Drop-down list (not editable) with a label on its left.
    pub fn combo(&self, label: &str, items: &[&str], selected: Option<u32>) -> gui::ComboBox {
        let y = self.take(ROW_H);
        self.side_label(label, y);
        gui::ComboBox::new(&self.wnd, gui::ComboBoxOpts {
            position: gui::dpi(MARGIN + LABEL_W, y),
            width: gui::dpi_x(self.content_width() - LABEL_W),
            items,
            selected_item: selected,
            ..Default::default()
        })
    }

    /// Single-selection list box with its label above.
    pub fn list_box(&self, label: &str, items: &[&str], height: i32) -> gui::ListBox {
        self.top_label(label);
        let y = self.take(height);
        gui::ListBox::new(&self.wnd, gui::ListBoxOpts {
            position: gui::dpi(MARGIN, y),
            size: gui::dpi(self.content_width(), height),
            items,
            ..Default::default()
        })
    }

    /// Report-view list with columns `(title, width)` and its label above.
    pub fn list_view(&self, label: &str, columns: &[(&str, i32)], height: i32) -> gui::ListView {
        self.top_label(label);
        let y = self.take(height);
        gui::ListView::new(&self.wnd, gui::ListViewOpts {
            position: gui::dpi(MARGIN, y),
            size: gui::dpi(self.content_width(), height),
            columns,
            control_style: co::LVS::REPORT | co::LVS::SHOWSELALWAYS | co::LVS::SINGLESEL,
            control_ex_style: co::LVS_EX::FULLROWSELECT | co::LVS_EX::DOUBLEBUFFER,
            ..Default::default()
        })
    }

    /// Slider with a label on its left.
    pub fn trackbar(&self, label: &str, min: u32, max: u32, value: u32) -> gui::Trackbar {
        let y = self.take(ROW_H + 6);
        self.side_label(label, y);
        gui::Trackbar::new(&self.wnd, gui::TrackbarOpts {
            position: gui::dpi(MARGIN + LABEL_W, y),
            size: gui::dpi(self.content_width() - LABEL_W, ROW_H + 6),
            range: (min, max),
            value,
            ..Default::default()
        })
    }

    /// A row of push buttons, right-aligned. Returns them in order.
    pub fn buttons(&self, specs: &[(&str, u16)]) -> Vec<gui::Button> {
        let y = self.take(ROW_H + 2);
        let n = specs.len() as i32;
        let mut x = self.width - MARGIN - n * BUTTON_W - (n - 1) * GAP;
        specs
            .iter()
            .map(|(text, id)| {
                let style = if *id == IDOK { co::BS::DEFPUSHBUTTON } else { co::BS::PUSHBUTTON };
                let b = gui::Button::new(&self.wnd, gui::ButtonOpts {
                    text,
                    position: gui::dpi(x, y),
                    width: gui::dpi_x(BUTTON_W),
                    height: gui::dpi_y(ROW_H + 2),
                    control_style: style,
                    ctrl_id: if *id == 0 { 0 } else { *id },
                    ..Default::default()
                });
                x += BUTTON_W + GAP;
                b
            })
            .collect()
    }

    /// Standard OK / Cancel row. Returns (ok, cancel).
    pub fn ok_cancel(&self, ok_text: &str) -> (gui::Button, gui::Button) {
        let mut b = self.buttons(&[(ok_text, IDOK), ("Cancel", IDCANCEL)]);
        let cancel = b.pop().unwrap();
        (b.pop().unwrap(), cancel)
    }

    /// A lone Close button that acts as Cancel (Escape also closes).
    pub fn close_button(&self) -> gui::Button {
        self.buttons(&[("Close", IDCANCEL)]).pop().unwrap()
    }

    /// Runs before OK closes the dialog; return false to keep it open.
    pub fn on_ok(&self, f: impl Fn() -> bool + 'static) {
        *self.validator.borrow_mut() = Some(Box::new(f));
    }

    /// Shows the dialog modally. True when closed through OK.
    pub fn run(&self, parent: &impl GuiParent) -> bool {
        if let Err(e) = self.wnd.show_modal(parent) {
            log::error!("dialog failed: {e}");
        }
        self.accepted.get()
    }
}

/// Resizes `hwnd` so its client area is `width` x `height` logical pixels, and
/// re-centres it on its owner.
fn fit_to_content(hwnd: &w::HWND, width: i32, height: i32) {
    let (cw, ch) = gui::dpi(width, height);
    let style = hwnd.style();
    let ex = hwnd.style_ex();
    let Ok(rc) = w::AdjustWindowRectEx(w::RECT { left: 0, top: 0, right: cw, bottom: ch }, style, false, ex) else { return };
    let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
    let (x, y) = match hwnd.GetWindow(co::GW::OWNER).and_then(|o| o.GetWindowRect()) {
        Ok(p) => (p.left + (p.right - p.left - ww) / 2, p.top + (p.bottom - p.top - wh) / 2),
        Err(_) => (100, 100),
    };
    let _ = hwnd.SetWindowPos(w::HwndPlace::None, w::POINT::with(x, y), w::SIZE::with(ww, wh), co::SWP::NOZORDER);
}
