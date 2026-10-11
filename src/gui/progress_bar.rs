//! Reading-progress slider with a percentage label. Focusable: the screen
//! reader speaks its position, and moving it turns to that page. Port of
//! prism/gui/progress_bar.py.

use winsafe::{self as w, co, gui, prelude::*};

use crate::platform::win32::a11y;

pub struct ProgressBar {
    pub slider: gui::Trackbar,
    label: gui::Label,
}

/// Percent of the document read at 1-based `page_num`.
pub fn percent(page_num: usize, page_count: usize) -> usize {
    page_num * 100 / page_count.max(1)
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

    /// Shows page `page_num` (1-based) of `page_count`. Does not request a page.
    pub fn update(&self, page_num: usize, page_count: usize) {
        // A trackbar needs min < max.
        self.slider.set_range(1, page_count.max(2) as u32);
        self.slider.set_pos(page_num as u32);
        let _ = self.label.hwnd().SetWindowText(&format!("{} %", percent(page_num, page_count)));
    }

    /// Empties the slider and label for when no document is open.
    pub fn reset(&self) {
        self.slider.set_range(0, 100);
        self.slider.set_pos(0);
        let _ = self.label.hwnd().SetWindowText("0 %");
    }

    /// The 0-based page the user moved the slider to.
    pub fn requested_page(&self) -> usize {
        (self.slider.pos() as usize).saturating_sub(1)
    }
}
