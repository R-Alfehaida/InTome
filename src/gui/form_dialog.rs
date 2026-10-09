//! Accessible form-filling dialog for PDF documents (port of prism/gui/form_dialog.py).
//!
//! Fields are laid out in page reading order, in group boxes by AI group, inside
//! a scrolled panel. Every control is named from the field's resolved label
//! (cached AI label, else author `/TU`, else the field name). Saving goes
//! through `pdf::save_form`, which also bakes each label into `/TU` and stamps
//! synthetic flat-form fields per their mark style.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use winsafe::{self as w, co, gui, prelude::*};

use crate::document::formats::pdf::{self, PdfDocument};
use crate::document::{Document, FieldType, FormField, cache};
use crate::events::{self, AppEvent};
use crate::gui::dialog::{self, DialogBuilder, GAP, IDCANCEL, MARGIN, ROW_H};
use crate::platform::win32::a11y;

/// Checkbox values that mean "checked".
const CHECKED_VALUES: [&str; 4] = ["yes", "on", "true", "1"];
const WIDTH: i32 = 600;
const VIEW_H: i32 = 380;
const LABEL_H: i32 = 18;
const LIST_H: i32 = 80;
/// Group box caption above its first row, and padding below its last.
const BOX_TOP: i32 = 20;
const BOX_BOTTOM: i32 = 4;
const SCROLLBAR_W: i32 = 20;
const OTHER_GROUP: &str = "Other Fields";
const SAVE_FAILED: &str = "Could not save form values to the PDF.\n\nTry Save As to save to a different file.";

/// Ctrl+S saves from any control: winsafe's modal loop has no accelerator table.
macro_rules! hook_ctrl_s_on {
    ($ctrl:expr, $ui:expr) => {{
        let (c, u) = ($ctrl.clone(), $ui.clone());
        $ctrl.on_subclass().wm_key_down(move |p| {
            if p.vkey_code == co::VK::CHAR_S && w::GetKeyState(co::VK::CONTROL).0 {
                u.save_to_original();
            } else {
                unsafe { c.hwnd().DefSubclassProc(p) };
            }
            Ok(())
        });
    }};
}

/// Focus moves the status line and scrolls the field into view.
macro_rules! hook_focus_on {
    ($ctrl:expr, $ui:expr, $index:expr) => {{
        let (c, u, index) = ($ctrl.clone(), $ui.clone(), $index);
        $ctrl.on_subclass().wm_set_focus(move |p| {
            unsafe { c.hwnd().DefSubclassProc(p) };
            u.on_focus(index);
            Ok(())
        });
    }};
}

/// The accessible form-filling dialog for `fields` of an open PDF, including
/// Save (to the original) and Save As. Emits `FormSaved` after a save.
pub fn show(parent: &impl GuiParent, doc: &mut PdfDocument, fields: Vec<FormField>, filename: &str) {
    let field_count = fields.len();
    let groups = group_fields(fields);
    let total: usize = groups.iter().map(|(_, g)| g.len()).sum();

    let title = format!("Fill Form: {filename}");
    let d = DialogBuilder::new(&title, WIDTH);
    let first_group = groups.first().map_or("", |(g, _)| g.as_str());
    let status = d.text(&if total > 0 { status_text(0, total, first_group) } else { String::new() });

    let view_top = d.column().take(VIEW_H);
    let view = gui::WindowControl::new(&d.wnd, gui::WindowControlOpts {
        position: gui::dpi(MARGIN, view_top),
        size: gui::dpi(d.content_width(), VIEW_H),
        class_bg_brush: gui::Brush::Color(co::COLOR::BTNFACE),
        // Not a tab stop itself; CONTROLPARENT makes the dialog manager tab into its controls.
        style: co::WS::CHILD | co::WS::VISIBLE | co::WS::VSCROLL | co::WS::CLIPCHILDREN,
        ex_style: co::WS_EX::CONTROLPARENT,
        ..Default::default()
    });
    let box_w = d.content_width() - SCROLLBAR_W - 8;
    let content_h = groups.iter().map(|(_, g)| group_height(g) + GAP).sum::<i32>() + 4;
    let content = gui::WindowControl::new(&view, gui::WindowControlOpts {
        position: gui::dpi(0, 0),
        size: gui::dpi(d.content_width() - SCROLLBAR_W, content_h),
        class_bg_brush: gui::Brush::Color(co::COLOR::BTNFACE),
        style: co::WS::CHILD | co::WS::VISIBLE | co::WS::CLIPSIBLINGS,
        ex_style: co::WS_EX::CONTROLPARENT,
        ..Default::default()
    });

    let mut rows = Vec::with_capacity(total);
    let mut y = 4;
    for (group, group_fields) in groups {
        let h = group_height(&group_fields);
        // Created before its rows so it sits behind them, where screen readers look for a field's group.
        let _ = gui::Button::new(&content, gui::ButtonOpts {
            text: &escape_mnemonics(&group),
            position: gui::dpi(4, y),
            width: gui::dpi_x(box_w),
            height: gui::dpi_y(h),
            control_style: co::BS::GROUPBOX,
            window_style: co::WS::CHILD | co::WS::VISIBLE,
            ..Default::default()
        });
        let mut row_y = y + BOX_TOP;
        for field in group_fields {
            let label = display_label(&field, rows.len());
            let input = Input::create(&content, &field, &label, 14, row_y, box_w - 20);
            row_y += row_height(field.field_type);
            rows.push(Row { field, group: group.clone(), label, input });
        }
        y += h + GAP;
    }

    let buttons = d.buttons(&[("&Save to PDF", 0), ("Save &As...", 0), ("Cancel", IDCANCEL)]);
    // No default button, as in Prism: Enter must not close the form and lose the typing.
    d.on_ok(|| false);

    let ui = Rc::new(FormUi {
        wnd: d.wnd.clone(),
        status,
        view,
        content,
        rows,
        scroll: Cell::new(0),
        src: doc.path().to_path_buf(),
        password: doc.password().map(str::to_string),
        filename: filename.to_string(),
    });

    let u = ui.clone();
    d.wnd.on().wm_show_window(move |p| {
        if p.being_shown {
            u.on_shown(field_count);
        }
        Ok(())
    });
    let u = ui.clone();
    ui.view.on().wm_v_scroll(move |p| {
        u.on_scrollbar(p.request);
        Ok(())
    });
    let u = ui.clone();
    ui.view.on().wm_mouse_wheel(move |p| {
        u.scroll_to(u.scroll.get() - p.wheel_distance as i32 * gui::dpi_y(3 * ROW_H) / 120);
        Ok(())
    });
    for (index, row) in ui.rows.iter().enumerate() {
        row.input.hook(&ui, index);
    }
    for b in &buttons[..2] {
        hook_ctrl_s_on!(b, ui);
    }
    let u = ui.clone();
    buttons[0].on().bn_clicked(move || {
        u.save_to_original();
        Ok(())
    });
    let u = ui.clone();
    buttons[1].on().bn_clicked(move || {
        u.save_as();
        Ok(())
    });

    d.run(parent);
}

/// A field and the control that edits it.
struct Row {
    field: FormField,
    group: String,
    label: String,
    input: Input,
}

enum Input {
    Text(gui::Edit),
    /// Signature and unknown fields: shown, not written.
    ReadOnly(gui::Edit),
    Check(gui::CheckBox),
    /// Values are read back by index into these choices, not from the shown text.
    Choice(gui::ComboBox, Vec<String>),
    List(gui::ListBox, Vec<String>),
}

impl Input {
    /// Creates the label (if any) and control for `field` at (`x`, `y`) in `parent`.
    fn create(parent: &gui::WindowControl, field: &FormField, label: &str, x: i32, y: i32, width: i32) -> Self {
        if field.field_type == FieldType::CheckBox {
            // A checkbox's caption is its accessible name, so it needs no separate label.
            return Input::Check(gui::CheckBox::new(parent, gui::CheckBoxOpts {
                text: &escape_mnemonics(label),
                position: gui::dpi(x, y + 2),
                size: gui::dpi(width, ROW_H - 4),
                check_state: if is_checked(&field.value) { co::BST::CHECKED } else { co::BST::UNCHECKED },
                ..Default::default()
            }));
        }
        let _ = gui::Label::new(parent, gui::LabelOpts {
            text: &format!("{}:", escape_mnemonics(label)),
            position: gui::dpi(x, y),
            size: gui::dpi(width, LABEL_H),
            ..Default::default()
        });
        let position = gui::dpi(x, y + LABEL_H + GAP);
        let edit = |read_only: bool| {
            gui::Edit::new(parent, gui::EditOpts {
                text: &field.value,
                position,
                width: gui::dpi_x(width),
                control_style: co::ES::AUTOHSCROLL | co::ES::NOHIDESEL | if read_only { co::ES::READONLY } else { co::ES::NoValue },
                ..Default::default()
            })
        };
        match field.field_type {
            FieldType::Text => Input::Text(edit(false)),
            FieldType::RadioButton | FieldType::ComboBox => {
                // Radio groups render as a choice list, which reads better than separate buttons.
                let choices = choices_of(field);
                let items: Vec<&str> = choices.iter().map(|c| choice_display(c)).collect();
                let combo = gui::ComboBox::new(parent, gui::ComboBoxOpts {
                    position,
                    width: gui::dpi_x(width),
                    items: &items,
                    selected_item: choice_index(&choices, &field.value, true).map(|i| i as u32),
                    ..Default::default()
                });
                Input::Choice(combo, choices)
            }
            FieldType::ListBox => {
                let items: Vec<&str> = field.choices.iter().map(|c| choice_display(c)).collect();
                let list = gui::ListBox::new(parent, gui::ListBoxOpts {
                    position,
                    size: gui::dpi(width, LIST_H),
                    items: &items,
                    ..Default::default()
                });
                Input::List(list, field.choices.clone())
            }
            _ => Input::ReadOnly(edit(true)),
        }
    }

    fn hwnd(&self) -> &w::HWND {
        match self {
            Input::Text(c) | Input::ReadOnly(c) => c.hwnd(),
            Input::Check(c) => c.hwnd(),
            Input::Choice(c, _) => c.hwnd(),
            Input::List(c, _) => c.hwnd(),
        }
    }

    fn hook(&self, ui: &Rc<FormUi>, index: usize) {
        match self {
            Input::Text(c) | Input::ReadOnly(c) => {
                hook_ctrl_s_on!(c, ui);
                hook_focus_on!(c, ui, index);
            }
            Input::Check(c) => {
                hook_ctrl_s_on!(c, ui);
                hook_focus_on!(c, ui, index);
            }
            Input::Choice(c, _) => {
                hook_ctrl_s_on!(c, ui);
                hook_focus_on!(c, ui, index);
            }
            Input::List(c, _) => {
                hook_ctrl_s_on!(c, ui);
                hook_focus_on!(c, ui, index);
            }
        }
    }

    /// The value to write; `None` leaves the field untouched.
    fn value(&self) -> Option<String> {
        match self {
            Input::Text(c) => Some(c.text().unwrap_or_default()),
            Input::ReadOnly(_) => None,
            Input::Check(c) => Some(if c.is_checked() { "Yes" } else { "Off" }.to_string()),
            Input::Choice(c, choices) => choices.get(c.items().selected_index()? as usize).cloned(),
            Input::List(c, choices) => {
                let (index, _) = c.items().iter_selected().ok()?.next()?.ok()?;
                choices.get(index as usize).cloned()
            }
        }
    }
}

struct FormUi {
    wnd: gui::WindowModal,
    status: gui::Label,
    view: gui::WindowControl,
    content: gui::WindowControl,
    rows: Vec<Row>,
    /// Scroll offset of `content` inside `view`, in physical pixels.
    scroll: Cell<i32>,
    src: PathBuf,
    password: Option<String>,
    filename: String,
}

impl FormUi {
    fn on_shown(&self, field_count: usize) {
        for row in &self.rows {
            a11y::set_name(row.input.hwnd().ptr() as isize, &row.label);
            if let (Input::List(list, _), Some(i)) = (&row.input, choice_index(&row.field.choices, &row.field.value, false)) {
                let _ = unsafe { list.hwnd().SendMessage(w::msg::LbSetCurSel { index: Some(i as u32) }) };
            }
        }
        self.scroll_to(0);
        events::announce(format!("Fill Form dialog, {field_count} fields. Tab through fields, then Save or Cancel."));
    }

    fn on_focus(&self, index: usize) {
        let row = &self.rows[index];
        let _ = self.status.hwnd().SetWindowText(&status_text(index, self.rows.len(), &row.group));
        self.ensure_visible(row.input.hwnd());
    }

    fn view_height(&self) -> i32 {
        self.view.hwnd().GetClientRect().map_or(0, |r| r.bottom)
    }

    fn content_height(&self) -> i32 {
        self.content.hwnd().GetClientRect().map_or(0, |r| r.bottom)
    }

    fn scroll_to(&self, pos: i32) {
        let (view_h, content_h) = (self.view_height(), self.content_height());
        let pos = pos.clamp(0, (content_h - view_h).max(0));
        self.scroll.set(pos);
        let _ = self.content.hwnd().SetWindowPos(w::HwndPlace::None, w::POINT::with(0, -pos), w::SIZE::default(), co::SWP::NOSIZE | co::SWP::NOZORDER | co::SWP::NOACTIVATE);
        let mut si = w::SCROLLINFO::default();
        si.fMask = co::SIF::RANGE | co::SIF::PAGE | co::SIF::POS;
        si.nMax = content_h - 1;
        si.nPage = view_h as u32;
        si.nPos = pos;
        self.view.hwnd().SetScrollInfo(co::SBB::VERT, &si, true);
    }

    fn on_scrollbar(&self, request: co::SB_REQ) {
        let (line, page) = (gui::dpi_y(ROW_H), self.view_height());
        let pos = self.scroll.get();
        let target = match request {
            co::SB_REQ::LINEUP => pos - line,
            co::SB_REQ::LINEDOWN => pos + line,
            co::SB_REQ::PAGEUP => pos - page,
            co::SB_REQ::PAGEDOWN => pos + page,
            co::SB_REQ::TOP => 0,
            co::SB_REQ::BOTTOM => i32::MAX,
            co::SB_REQ::THUMBTRACK | co::SB_REQ::THUMBPOSITION => {
                let mut si = w::SCROLLINFO::default();
                si.fMask = co::SIF::TRACKPOS;
                match self.view.hwnd().GetScrollInfo(co::SBB::VERT, &mut si) {
                    Ok(()) => si.nTrackPos,
                    Err(_) => pos,
                }
            }
            _ => pos,
        };
        self.scroll_to(target);
    }

    /// Scrolls so `hwnd` and the label above it are visible.
    fn ensure_visible(&self, hwnd: &w::HWND) {
        let Ok(rc) = hwnd.GetWindowRect().and_then(|rc| self.content.hwnd().ScreenToClientRc(rc)) else { return };
        let (pos, view_h) = (self.scroll.get(), self.view_height());
        let above = gui::dpi_y(LABEL_H + GAP + BOX_TOP);
        if rc.top - above < pos {
            self.scroll_to(rc.top - above);
        } else if rc.bottom + gui::dpi_y(GAP) > pos + view_h {
            self.scroll_to(rc.bottom + gui::dpi_y(GAP) - view_h);
        }
    }

    fn save_to_original(&self) {
        self.save(&self.src, SAVE_FAILED);
    }

    fn save_as(&self) {
        let initial = self.src.with_file_name(&self.filename);
        let filters = [("PDF files (*.pdf)".to_string(), "*.pdf".to_string())];
        let Some(path) = dialog::pick_save_file(self.wnd.hwnd(), "Save Form As", &filters, &initial.to_string_lossy()) else { return };
        let path = with_pdf_extension(PathBuf::from(path));
        self.save(&path, &format!("Could not save form to: {}", path.display()));
    }

    /// Writes the form to `out` and closes the dialog; on failure reports `failure` and stays open.
    fn save(&self, out: &Path, failure: &str) {
        let fields: Vec<FormField> = self.rows.iter().map(|r| r.field.clone()).collect();
        let values: Vec<Option<String>> = self.rows.iter().map(|r| r.input.value()).collect();
        match pdf::save_form(&self.src, self.password.as_deref(), &fields, &values, out) {
            Ok(()) => {
                let output_path = out.display().to_string();
                events::emit(AppEvent::FormSaved {
                    document_hash: cache::file_hash(out),
                    field_count: values.iter().flatten().count(),
                    output_path: output_path.clone(),
                });
                events::announce(format!("Form saved to {output_path}"));
                self.wnd.close();
            }
            Err(e) => {
                log::error!("Saving form to {} failed: {e:#}", out.display());
                events::announce(format!("Save error: {failure}"));
                let _ = self.wnd.hwnd().MessageBox(&format!("{failure}\n\nDetails: {e:#}"), "Save Error", co::MB::OK | co::MB::ICONERROR);
            }
        }
    }
}

/// Orders fields the way a sighted reader scans the page: top to bottom, and
/// fields whose tops fall within a small tolerance form one visual row, read
/// left to right. This makes a two-column Yes/No checklist present as "Yes, No"
/// per row instead of in widget order.
fn reading_order(fields: Vec<FormField>) -> Vec<FormField> {
    // Within-row tops differ by a couple of points; rows are tens apart.
    const ROW_TOLERANCE: f32 = 8.0;
    let mut sorted = fields;
    sorted.sort_by(|a, b| (a.page_index, a.rect[1], a.rect[0]).partial_cmp(&(b.page_index, b.rect[1], b.rect[0])).unwrap_or(std::cmp::Ordering::Equal));
    let mut ordered = Vec::with_capacity(sorted.len());
    let mut row: Vec<FormField> = Vec::new();
    for f in sorted {
        let same_row = row.first().is_some_and(|first| first.page_index == f.page_index && f.rect[1] - first.rect[1] <= ROW_TOLERANCE);
        if !same_row {
            flush_row(&mut row, &mut ordered);
        }
        row.push(f);
    }
    flush_row(&mut row, &mut ordered);
    ordered
}

fn flush_row(row: &mut Vec<FormField>, out: &mut Vec<FormField>) {
    row.sort_by(|a, b| a.rect[0].partial_cmp(&b.rect[0]).unwrap_or(std::cmp::Ordering::Equal));
    out.append(row);
}

/// (group name, fields) runs in reading order, push buttons dropped. The split
/// is positional, so a group that is not contiguous on the page yields more
/// than one box and the dialog still follows the page layout.
fn group_fields(fields: Vec<FormField>) -> Vec<(String, Vec<FormField>)> {
    let fillable = fields.into_iter().filter(|f| f.field_type != FieldType::Button).collect();
    let mut groups: Vec<(String, Vec<FormField>)> = Vec::new();
    for f in reading_order(fillable) {
        let name = f.group.clone().filter(|g| !g.is_empty()).unwrap_or_else(|| OTHER_GROUP.to_string());
        match groups.last_mut() {
            Some((current, run)) if *current == name => run.push(f),
            _ => groups.push((name, vec![f])),
        }
    }
    groups
}

/// The resolved label (AI, else `/TU`), else the field name, else "Field N" for 0-based `index`.
fn display_label(field: &FormField, index: usize) -> String {
    match field.label.as_deref().filter(|l| !l.is_empty()) {
        Some(label) => label.to_string(),
        None if !field.name.is_empty() => field.name.clone(),
        None => format!("Field {}", index + 1),
    }
}

/// The status line for the field at 0-based `index`.
fn status_text(index: usize, total: usize, group: &str) -> String {
    format!("Field {} of {total} \u{2014} {group}", index + 1)
}

/// Choices for a pick-one field; a radio group without listed options is Yes/No.
fn choices_of(field: &FormField) -> Vec<String> {
    if field.choices.is_empty() && field.field_type == FieldType::RadioButton { vec!["Yes".into(), "No".into()] } else { field.choices.clone() }
}

/// How a choice is shown. The blank "unanswered" entry of a flat-form choice
/// gets words: winsafe cannot add an empty item, and a screen reader would say "blank".
fn choice_display(choice: &str) -> &str {
    if choice.trim().is_empty() { "(no answer)" } else { choice }
}

/// Index of `value` in `choices`; else the first entry when `default_first`.
fn choice_index(choices: &[String], value: &str, default_first: bool) -> Option<usize> {
    let found = (!value.is_empty()).then(|| choices.iter().position(|c| c == value)).flatten();
    found.or((default_first && !choices.is_empty()).then_some(0))
}

fn is_checked(value: &str) -> bool {
    CHECKED_VALUES.contains(&value.trim().to_lowercase().as_str())
}

fn row_height(field_type: FieldType) -> i32 {
    match field_type {
        FieldType::CheckBox => ROW_H + GAP,
        FieldType::ListBox => LABEL_H + GAP + LIST_H + GAP,
        _ => LABEL_H + GAP + ROW_H + GAP,
    }
}

fn group_height(fields: &[FormField]) -> i32 {
    BOX_TOP + fields.iter().map(|f| row_height(f.field_type)).sum::<i32>() + BOX_BOTTOM
}

/// A literal `&` in a caption would otherwise underline the next letter and vanish from the name.
fn escape_mnemonics(text: &str) -> String {
    text.replace('&', "&&")
}

fn with_pdf_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some() { path } else { path.with_extension("pdf") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::MarkStyle;

    fn field(name: &str, field_type: FieldType, rect: [f32; 4], group: Option<&str>) -> FormField {
        FormField {
            page_index: 0,
            field_type,
            name: name.into(),
            value: String::new(),
            rect,
            choices: Vec::new(),
            label: None,
            group: group.map(str::to_string),
            synthetic: false,
            choice_rects: Vec::new(),
            mark_style: MarkStyle::Widget,
        }
    }

    fn names(fields: &[FormField]) -> Vec<&str> {
        fields.iter().map(|f| f.name.as_str()).collect()
    }

    #[test]
    fn reading_order_reads_rows_left_to_right_within_tolerance() {
        let fields = vec![
            field("q2_no", FieldType::CheckBox, [300.0, 141.0, 310.0, 151.0], None),
            field("q1_no", FieldType::CheckBox, [300.0, 100.0, 310.0, 110.0], None),
            field("q2_yes", FieldType::CheckBox, [200.0, 139.0, 210.0, 149.0], None),
            field("q1_yes", FieldType::CheckBox, [200.0, 102.0, 210.0, 112.0], None),
            FormField { page_index: 1, ..field("p2", FieldType::Text, [0.0, 0.0, 10.0, 10.0], None) },
        ];
        assert_eq!(names(&reading_order(fields)), ["q1_yes", "q1_no", "q2_yes", "q2_no", "p2"]);
    }

    #[test]
    fn groups_are_consecutive_runs_without_push_buttons() {
        let fields = vec![
            field("name", FieldType::Text, [0.0, 10.0, 1.0, 20.0], Some("Applicant")),
            field("submit", FieldType::Button, [0.0, 15.0, 1.0, 25.0], Some("Applicant")),
            field("email", FieldType::Text, [0.0, 40.0, 1.0, 50.0], Some("Applicant")),
            field("notes", FieldType::Text, [0.0, 70.0, 1.0, 80.0], None),
            field("phone", FieldType::Text, [0.0, 100.0, 1.0, 110.0], Some("Applicant")),
        ];
        let grouped = group_fields(fields);
        let groups: Vec<(String, Vec<&str>)> = grouped.iter().map(|(g, f)| (g.clone(), names(f))).collect();
        assert_eq!(
            groups,
            [
                ("Applicant".to_string(), vec!["name", "email"]),
                ("Other Fields".to_string(), vec!["notes"]),
                ("Applicant".to_string(), vec!["phone"]),
            ]
        );
    }

    #[test]
    fn label_prefers_resolved_label_then_name_then_position() {
        let mut f = field("addr.city", FieldType::Text, [0.0; 4], None);
        assert_eq!(display_label(&f, 2), "addr.city");
        f.label = Some("City of residence".into());
        assert_eq!(display_label(&f, 2), "City of residence");
        f.label = Some(String::new());
        f.name.clear();
        assert_eq!(display_label(&f, 2), "Field 3");
    }

    #[test]
    fn status_line_counts_from_one_and_names_the_group() {
        assert_eq!(status_text(0, 7, "Applicant"), "Field 1 of 7 \u{2014} Applicant");
    }

    #[test]
    fn choices_select_the_value_or_fall_back() {
        let choices: Vec<String> = ["Red", "Green"].map(String::from).to_vec();
        assert_eq!(choice_index(&choices, "Green", true), Some(1));
        assert_eq!(choice_index(&choices, "Blue", true), Some(0));
        assert_eq!(choice_index(&choices, "", false), None);
        let radio = field("colour", FieldType::RadioButton, [0.0; 4], None);
        assert_eq!(choices_of(&radio), ["Yes", "No"]);
        assert_eq!([choice_display(""), choice_display("Yes")], ["(no answer)", "Yes"]);
        assert!(is_checked(" On ") && !is_checked("Off"));
    }
}
