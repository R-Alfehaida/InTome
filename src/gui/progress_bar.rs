//! Reading-progress slider with a percentage label. Focusable: the screen
//! reader speaks its position, and moving it jumps to that point in the
//! document. Port of prism/gui/progress_bar.py.
//!
//! The slider is always 0-100, the percent of the document's characters
//! before the caret, so the label and the value a screen reader reads agree,
//! and it moves as you read within a page. Prism ranged it over pages, which
//! stood still within a page and, in a one- or two-page document, could only
//! read 0 or 100.

use winsafe::{self as w, co, gui, prelude::*};

use crate::platform::win32::a11y;

pub struct ProgressBar {
    pub slider: gui::Trackbar,
    label: gui::Label,
}

impl ProgressBar {
    pub fn new(parent: &gui::WindowMain) -> Self {
        let slider = gui::Trackbar::new(parent, gui::TrackbarOpts { range: (0, 100), value: 0, ..Default::default() });
        let label = gui::Label::new(parent, gui::LabelOpts { text: "0 %", size: gui::dpi(50, 20), ..Default::default() });
        Self { slider, label }
    }

    pub fn name_control(&self) {
        a11y::set_name(self.slider.hwnd().ptr() as isize, "Reading progress");
    }

    pub fn layout(&self, x: i32, y: i32, width: i32, height: i32) {
        let label_w = gui::dpi_x(50);
        let _ = self.slider.hwnd().SetWindowPos(w::HwndPlace::None, w::POINT::with(x, y), w::SIZE::with(width - label_w - gui::dpi_x(8), height), co::SWP::NOZORDER);
        let _ = self.label.hwnd().SetWindowPos(w::HwndPlace::None, w::POINT::with(x + width - label_w, y + gui::dpi_y(4)), w::SIZE::with(label_w, gui::dpi_y(20)), co::SWP::NOZORDER);
    }

    /// Shows `percent` (0-100) on the slider and the label. Does not move the reading position.
    pub fn show(&self, percent: usize) {
        let percent = percent.min(100);
        // Skipped when unchanged: this runs on every caret move.
        if self.slider.pos() != percent as u32 {
            self.slider.set_pos(percent as u32);
        }
        self.show_label(percent);
    }

    /// Sets only the label, for when the slider itself already holds the value.
    pub fn show_label(&self, percent: usize) {
        let _ = self.label.hwnd().SetWindowText(&format!("{} %", percent.min(100)));
    }

    /// Empties the slider and label for when no document is open.
    pub fn reset(&self) {
        self.show(0);
    }

    /// The percent the user moved the slider to.
    pub fn value(&self) -> usize {
        self.slider.pos() as usize
    }
}
