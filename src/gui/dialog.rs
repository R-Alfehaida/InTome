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
//! // Read controls in on_ok: once run() returns the window is destroyed.
//! let value = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
//! let v = value.clone();
//! d.on_ok(move || { *v.borrow_mut() = page.text().unwrap_or_default(); true });
//! if d.run(parent) { let _ = value.borrow(); }
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
        // winsafe's modal, unlike its main window, forgets the focused control
        // while a child modal (file picker, message box) is up. Put focus back
        // where the user was, as a real dialog does.
        let (wnd, prev) = (self.wnd.clone(), Rc::new(Cell::new(0isize)));
        self.wnd.on().wm_activate(move |p| {
            if p.is_minimized {
                return Ok(());
            }
            if p.event == co::WA::INACTIVE {
                if let Some(focus) = w::HWND::GetFocus()
                    && wnd.hwnd().IsChild(&focus)
                {
                    prev.set(focus.ptr() as isize);
                }
                return Ok(());
            }
            let last = unsafe { w::HWND::from_ptr(prev.get() as _) };
            if prev.get() != 0 && last.IsWindow() && last.IsWindowVisible() && last.IsWindowEnabled() {
                last.SetFocus();
            } else if let Ok(first) = wnd.hwnd().GetNextDlgTabItem(&w::HWND::NULL, false) {
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
        route_next_dlg_ctl(self.wnd.on());
    }

    pub fn content_width(&self) -> i32 {
        self.width - 2 * MARGIN
    }

    /// Adds vertical space.
    pub fn space(&self, h: i32) {
        self.y.set(self.y.get() + h);
    }

    fn take(&self, h: i32) -> i32 {
        self.column().take(h)
    }

    /// A static text line (status or explanation), read by screen readers in dialog review.
    pub fn text(&self, text: &str) -> gui::Label {
        self.column().text(text, 1)
    }

    /// Single-line edit with a label on its left. `style` extras: `co::ES::PASSWORD`, `co::ES::NUMBER`.
    pub fn edit_with(&self, label: &str, value: &str, style: co::ES) -> gui::Edit {
        self.column().edit(label, value, style)
    }

    pub fn edit(&self, label: &str, value: &str) -> gui::Edit {
        self.edit_with(label, value, co::ES::NoValue)
    }

    /// Multi-line edit with its label above.
    pub fn multiline(&self, label: &str, value: &str, height: i32, read_only: bool) -> gui::Edit {
        self.column().multiline(label, value, height, read_only)
    }

    pub fn check(&self, text: &str, checked: bool) -> gui::CheckBox {
        self.column().check(text, checked)
    }

    /// Drop-down list (not editable) with a label on its left.
    pub fn combo(&self, label: &str, items: &[&str], selected: Option<u32>) -> gui::ComboBox {
        self.column().combo(label, items, selected)
    }

    /// Single-selection list box with its label above.
    pub fn list_box(&self, label: &str, items: &[&str], height: i32) -> gui::ListBox {
        self.column().list_box(label, items, height)
    }

    /// Report-view list with columns `(title, width)` and its label above.
    pub fn list_view(&self, label: &str, columns: &[(&str, i32)], height: i32) -> gui::ListView {
        self.column().list_view(label, columns, height)
    }

    /// Slider with a label on its left.
    pub fn trackbar(&self, label: &str, min: u32, max: u32, value: u32) -> gui::Trackbar {
        self.column().trackbar(label, min, max, value)
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

    /// A [`Column`] over this dialog that shares its running y, for row types
    /// the builder lacks (spin, browse, progress, left-aligned buttons).
    pub fn column(&self) -> Column<gui::WindowModal> {
        Column { parent: self.wnd.clone(), x: MARGIN, width: self.content_width(), y: self.y.clone() }
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

/// A multi-line edit moves focus on Tab by sending WM_NEXTDLGCTL to its parent,
/// which only real dialog classes handle; without this, Tab is stuck in it.
/// Call for every window that parents a multi-line edit.
pub fn route_next_dlg_ctl(events: &impl GuiEventsWindow) {
    events.wm(co::WM::NEXTDLGCTL, |p| {
        if let Some(focus) = w::HWND::GetFocus()
            && let Some(root) = focus.GetAncestor(co::GA::ROOT)
            && let Ok(next) = root.GetNextDlgTabItem(&focus, p.wparam != 0)
        {
            next.SetFocus();
        }
        Ok(0)
    });
}

/// Rows stacked top to bottom on any parent: a page window inside a dialog
/// (preferences), or the dialog itself through [`DialogBuilder::column`].
/// Same rules as the builder: creation order is tab order, and each labelled
/// control is created right after its label so MSAA names it from the label.
#[derive(Clone)]
pub struct Column<P: GuiParent + Clone + 'static> {
    parent: P,
    x: i32,
    width: i32,
    y: Rc<Cell<i32>>,
}

impl<P: GuiParent + Clone + 'static> Column<P> {
    /// Rows start at (`x`, `y`) in `parent` and span `width` logical pixels.
    pub fn new(parent: &P, x: i32, y: i32, width: i32) -> Self {
        Self { parent: parent.clone(), x, width, y: Rc::new(Cell::new(y)) }
    }

    /// Reserves `h` logical pixels and returns the row's top y.
    pub fn take(&self, h: i32) -> i32 {
        let top = self.y.get();
        self.y.set(top + h + GAP);
        top
    }

    pub fn parent(&self) -> &P {
        &self.parent
    }

    fn label_at(&self, text: &str, x: i32, y: i32, w: i32, h: i32) -> gui::Label {
        gui::Label::new(&self.parent, gui::LabelOpts { text, position: gui::dpi(x, y), size: gui::dpi(w, h), ..Default::default() })
    }

    fn side_label(&self, label: &str, y: i32) -> gui::Label {
        self.label_at(label, self.x, y + 3, LABEL_W - GAP, ROW_H - 3)
    }

    fn top_label(&self, label: &str) -> gui::Label {
        let y = self.take(ROW_H - GAP);
        self.label_at(label, self.x, y, self.width, ROW_H - GAP)
    }

    /// Static text of `lines` lines.
    pub fn text(&self, text: &str, lines: i32) -> gui::Label {
        let h = 16 * lines + 4;
        let y = self.take(h);
        self.label_at(text, self.x, y, self.width, h)
    }

    pub fn check(&self, text: &str, checked: bool) -> gui::CheckBox {
        let y = self.take(ROW_H);
        gui::CheckBox::new(&self.parent, gui::CheckBoxOpts {
            text,
            position: gui::dpi(self.x, y + 2),
            size: gui::dpi(self.width, ROW_H - 4),
            check_state: if checked { co::BST::CHECKED } else { co::BST::UNCHECKED },
            ..Default::default()
        })
    }

    /// Single-line edit with a label on its left.
    pub fn edit(&self, label: &str, value: &str, style: co::ES) -> gui::Edit {
        let y = self.take(ROW_H);
        self.side_label(label, y);
        gui::Edit::new(&self.parent, gui::EditOpts {
            text: value,
            position: gui::dpi(self.x + LABEL_W, y),
            width: gui::dpi_x(self.width - LABEL_W),
            control_style: co::ES::AUTOHSCROLL | co::ES::NOHIDESEL | style,
            ..Default::default()
        })
    }

    /// Numeric edit with an up-down buddy (the wx.SpinCtrl equivalent). Read it with [`spin_value`].
    pub fn spin(&self, label: &str, value: u32, min: u32, max: u32) -> gui::Edit {
        let y = self.take(ROW_H);
        self.side_label(label, y);
        let edit = gui::Edit::new(&self.parent, gui::EditOpts {
            text: &value.to_string(),
            position: gui::dpi(self.x + LABEL_W, y),
            width: gui::dpi_x(90),
            control_style: co::ES::AUTOHSCROLL | co::ES::NOHIDESEL | co::ES::NUMBER,
            ..Default::default()
        });
        // AUTOBUDDY binds to the previous sibling, the edit just created.
        let _ = gui::UpDown::new(&self.parent, gui::UpDownOpts {
            position: gui::dpi(self.x + LABEL_W + 90, y),
            height: gui::dpi_y(ROW_H),
            control_style: co::UDS::AUTOBUDDY | co::UDS::SETBUDDYINT | co::UDS::ALIGNRIGHT | co::UDS::ARROWKEYS | co::UDS::NOTHOUSANDS,
            range: (min as i32, max as i32),
            value: value as i32,
            ..Default::default()
        });
        edit
    }

    /// Drop-down list (not editable) with a label on its left.
    pub fn combo(&self, label: &str, items: &[&str], selected: Option<u32>) -> gui::ComboBox {
        let y = self.take(ROW_H);
        self.side_label(label, y);
        gui::ComboBox::new(&self.parent, gui::ComboBoxOpts {
            position: gui::dpi(self.x + LABEL_W, y),
            width: gui::dpi_x(self.width - LABEL_W),
            items,
            selected_item: selected,
            ..Default::default()
        })
    }

    /// Drop-down that also takes typed text, starting with `value` as its only item.
    /// Read it with `hwnd().GetWindowText()`, which gives the typed or picked text.
    pub fn editable_combo(&self, label: &str, value: &str) -> gui::ComboBox {
        let y = self.take(ROW_H);
        self.side_label(label, y);
        let items: &[&str] = if value.is_empty() { &[] } else { &[value] };
        gui::ComboBox::new(&self.parent, gui::ComboBoxOpts {
            position: gui::dpi(self.x + LABEL_W, y),
            width: gui::dpi_x(self.width - LABEL_W),
            items,
            selected_item: (!value.is_empty()).then_some(0),
            control_style: co::CBS::DROPDOWN | co::CBS::AUTOHSCROLL,
            ..Default::default()
        })
    }

    /// Multi-line edit with its label above.
    pub fn multiline(&self, label: &str, value: &str, height: i32, read_only: bool) -> gui::Edit {
        self.top_label(label);
        let y = self.take(height);
        let ro = if read_only { co::ES::READONLY } else { co::ES::NoValue };
        gui::Edit::new(&self.parent, gui::EditOpts {
            text: value,
            position: gui::dpi(self.x, y),
            width: gui::dpi_x(self.width),
            height: gui::dpi_y(height),
            control_style: co::ES::MULTILINE | co::ES::WANTRETURN | co::ES::AUTOVSCROLL | co::ES::NOHIDESEL | ro,
            window_style: co::WS::CHILD | co::WS::VISIBLE | co::WS::TABSTOP | co::WS::GROUP | co::WS::VSCROLL,
            ..Default::default()
        })
    }

    /// Single-selection list box with its label above.
    pub fn list_box(&self, label: &str, items: &[&str], height: i32) -> gui::ListBox {
        self.top_label(label);
        let y = self.take(height);
        gui::ListBox::new(&self.parent, gui::ListBoxOpts {
            position: gui::dpi(self.x, y),
            size: gui::dpi(self.width, height),
            items,
            ..Default::default()
        })
    }

    /// Report-view list with columns `(title, width)` and its label above.
    pub fn list_view(&self, label: &str, columns: &[(&str, i32)], height: i32) -> gui::ListView {
        self.top_label(label);
        let y = self.take(height);
        gui::ListView::new(&self.parent, gui::ListViewOpts {
            position: gui::dpi(self.x, y),
            size: gui::dpi(self.width, height),
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
        gui::Trackbar::new(&self.parent, gui::TrackbarOpts {
            position: gui::dpi(self.x + LABEL_W, y),
            size: gui::dpi(self.width - LABEL_W, ROW_H + 6),
            range: (min, max),
            value,
            ..Default::default()
        })
    }

    fn edit_and_button(&self, x: i32, y: i32, value: &str, style: co::ES, button: &str) -> (gui::Edit, gui::Button) {
        let edit_w = self.x + self.width - x - BUTTON_W - GAP;
        let edit = gui::Edit::new(&self.parent, gui::EditOpts {
            text: value,
            position: gui::dpi(x, y),
            width: gui::dpi_x(edit_w),
            control_style: co::ES::AUTOHSCROLL | co::ES::NOHIDESEL | style,
            ..Default::default()
        });
        let button = gui::Button::new(&self.parent, gui::ButtonOpts {
            text: button,
            position: gui::dpi(x + edit_w + GAP, y - 1),
            width: gui::dpi_x(BUTTON_W),
            height: gui::dpi_y(ROW_H + 2),
            ..Default::default()
        });
        (edit, button)
    }

    /// Single-line edit with a label on its left and a push button on its right.
    pub fn edit_button(&self, label: &str, value: &str, style: co::ES, button: &str) -> (gui::Edit, gui::Button) {
        let y = self.take(ROW_H);
        self.side_label(label, y);
        self.edit_and_button(self.x + LABEL_W, y, value, style, button)
    }

    /// A path edit with its label above (for long labels) and a "&Browse..." button on its right.
    pub fn browse(&self, label: &str, value: &str) -> (gui::Edit, gui::Button) {
        self.top_label(label);
        let y = self.take(ROW_H);
        self.edit_and_button(self.x, y, value, co::ES::NoValue, "&Browse...")
    }

    /// A row of left-aligned push buttons, each `width` logical pixels wide.
    pub fn buttons(&self, texts: &[&str], width: i32) -> Vec<gui::Button> {
        let y = self.take(ROW_H + 2);
        let mut x = self.x;
        texts
            .iter()
            .map(|text| {
                let b = gui::Button::new(&self.parent, gui::ButtonOpts {
                    text,
                    position: gui::dpi(x, y),
                    width: gui::dpi_x(width),
                    height: gui::dpi_y(ROW_H + 2),
                    ..Default::default()
                });
                x += width + GAP;
                b
            })
            .collect()
    }

    /// Full-width progress bar, range 0-100. Place it after its status text so
    /// MSAA names it from that text.
    pub fn progress(&self) -> gui::ProgressBar {
        let y = self.take(ROW_H - 6);
        gui::ProgressBar::new(&self.parent, gui::ProgressBarOpts {
            position: gui::dpi(self.x, y),
            size: gui::dpi(self.width, ROW_H - 6),
            ..Default::default()
        })
    }
}

/// The value of a [`Column::spin`] edit, clamped to `min..=max`; `fallback` if it is not a number.
pub fn spin_value(edit: &gui::Edit, min: u32, max: u32, fallback: u32) -> u32 {
    parse_clamped(&edit.text().unwrap_or_default(), min, max, fallback)
}

fn parse_clamped(text: &str, min: u32, max: u32, fallback: u32) -> u32 {
    text.trim().parse::<u64>().map_or(fallback, |v| v.clamp(min as u64, max as u64) as u32)
}

fn shell_item(path: &str) -> Option<w::IShellItem> {
    if path.is_empty() {
        return None;
    }
    w::SHCreateItemFromParsingName::<w::IShellItem>(path, None::<&w::IBindCtx>).ok()
}

/// Folder picker starting at `initial` (when it exists). `None` when cancelled.
pub fn pick_folder(owner: &w::HWND, title: &str, initial: &str) -> Option<String> {
    let run = || -> w::HrResult<Option<String>> {
        let _com = w::CoInitializeEx(co::COINIT::APARTMENTTHREADED)?;
        let fd = w::CoCreateInstance::<w::IFileOpenDialog>(&co::CLSID::FileOpenDialog, None::<&w::IUnknown>, co::CLSCTX::INPROC_SERVER)?;
        fd.SetOptions(fd.GetOptions()? | co::FOS::PICKFOLDERS | co::FOS::FORCEFILESYSTEM)?;
        fd.SetTitle(title)?;
        if let Some(item) = shell_item(initial) {
            fd.SetFolder(&item)?;
        }
        if !fd.Show(owner)? {
            return Ok(None);
        }
        Ok(Some(fd.GetResult()?.GetDisplayName(co::SIGDN::FILESYSPATH)?))
    };
    run().unwrap_or_else(|e| {
        log::error!("folder dialog failed: {e}");
        None
    })
}

/// Multi-select open dialog. `filters` are `(label, "*.ext;*.ext")` pairs; empty when cancelled.
pub fn pick_open_files(owner: &w::HWND, title: &str, filters: &[(String, String)]) -> Vec<String> {
    let run = || -> w::HrResult<Vec<String>> {
        let _com = w::CoInitializeEx(co::COINIT::APARTMENTTHREADED)?;
        let fd = w::CoCreateInstance::<w::IFileOpenDialog>(&co::CLSID::FileOpenDialog, None::<&w::IUnknown>, co::CLSCTX::INPROC_SERVER)?;
        fd.SetOptions(fd.GetOptions()? | co::FOS::ALLOWMULTISELECT | co::FOS::FILEMUSTEXIST | co::FOS::FORCEFILESYSTEM)?;
        fd.SetTitle(title)?;
        fd.SetFileTypes(filters)?;
        if !fd.Show(owner)? {
            return Ok(Vec::new());
        }
        fd.GetResults()?.iter()?.map(|item| item?.GetDisplayName(co::SIGDN::FILESYSPATH)).collect()
    };
    run().unwrap_or_else(|e| {
        log::error!("open dialog failed: {e}");
        Vec::new()
    })
}

/// Save dialog seeded with `initial` (folder and file name); prompts before overwriting.
pub fn pick_save_file(owner: &w::HWND, title: &str, filters: &[(String, String)], initial: &str) -> Option<String> {
    let run = || -> w::HrResult<Option<String>> {
        let _com = w::CoInitializeEx(co::COINIT::APARTMENTTHREADED)?;
        let fd = w::CoCreateInstance::<w::IFileSaveDialog>(&co::CLSID::FileSaveDialog, None::<&w::IUnknown>, co::CLSCTX::INPROC_SERVER)?;
        fd.SetOptions(fd.GetOptions()? | co::FOS::OVERWRITEPROMPT | co::FOS::FORCEFILESYSTEM)?;
        fd.SetTitle(title)?;
        fd.SetFileTypes(filters)?;
        let path = std::path::Path::new(initial);
        if let Some(item) = path.parent().and_then(|p| shell_item(&p.to_string_lossy())) {
            fd.SetFolder(&item)?;
        }
        if let Some(name) = path.file_name() {
            fd.SetFileName(&name.to_string_lossy())?;
        }
        if !fd.Show(owner)? {
            return Ok(None);
        }
        Ok(Some(fd.GetResult()?.GetDisplayName(co::SIGDN::FILESYSPATH)?))
    };
    run().unwrap_or_else(|e| {
        log::error!("save dialog failed: {e}");
        None
    })
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

#[cfg(test)]
mod tests {
    use super::parse_clamped;

    #[test]
    fn spin_text_is_clamped_and_falls_back_when_not_a_number() {
        assert_eq!(parse_clamped(" 42 ", 1, 50, 10), 42);
        assert_eq!(parse_clamped("999", 1, 50, 10), 50);
        assert_eq!(parse_clamped("0", 1, 50, 10), 1);
        assert_eq!(parse_clamped("", 1, 50, 10), 10);
        assert_eq!(parse_clamped("1,000", 100, 5000, 500), 500);
    }
}
