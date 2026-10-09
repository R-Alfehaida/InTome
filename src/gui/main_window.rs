//! The main window: TOC sidebar, one tab per open document, find bar,
//! progress slider, status bar, the full menu bar and its accelerators, and
//! every menu handler. Port of prism/gui/main_window.py.
//!
//! winsafe creates controls only with their parent, so the document tabs are
//! a tab strip plus one raw RichEdit per document ([`ContentView`]), shown
//! one at a time under the strip. Work that finishes on another thread
//! reaches the window through `events::run_on_ui` and [`with_main`].

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use winsafe::{self as w, co, gui, msg, prelude::*};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{IDC_SIZEWE, LoadCursorW, PostMessageW, SetCursor};

use crate::ai::client::GeminiClient;
use crate::ai::processing::{PROCESSING_MANAGER, StartOutcome};
use crate::ai::provider::is_ocr_available;
use crate::ai::providers::tesseract;
use crate::annotation::manager::AnnotationManager;
use crate::config;
use crate::constants::{APP_NAME, APP_VERSION, MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH, resource_path};
use crate::document::formats::archive;
use crate::document::{Capabilities, Document, FormField, OpenError, OpenOptions, SnapshotDocument, cache, registry};
use crate::events::{self, AppEvent, announce};
use crate::gui::content_view::{self, ContentView, ViewTarget};
use crate::gui::dialog::{DialogBuilder, IDCANCEL, IDOK};
use crate::gui::error_dialog::{show_error, wrapped_lines};
use crate::gui::find_bar::FindBar;
use crate::gui::progress_bar::ProgressBar;
use crate::gui::toc_tree::TocTree;
use crate::gui::{
    annotation_dialogs, batch_dialog, element_list_dialog, export_dialog, form_dialog, preferences, shortcuts_dialog, updater_ui,
    voice_profiles,
};
use crate::navigation::element_nav;
use crate::tts::reader::{ReadingController, ReadingState};

// Document loading runs on a worker thread. The progress dialog only appears
// if the load is still running after this delay, so small files open with no flash.
const LOAD_PROGRESS_DELAY_MS: u32 = 400;
const LOAD_TIMER: usize = 1;
const FIND_TIMER: usize = 2;
const FIND_DEBOUNCE_MS: u32 = 300;
const RECENT_BASE: u16 = 9000;
const RECENT_MAX: u16 = 50;
const DEFER_MSG: u32 = 0x8000 + 1; // WM_APP + 1
const SASH: i32 = 5;
const PBM_SETMARQUEE: u32 = 0x0400 + 10;
const LB_SETCURSEL: u32 = 0x0186;
const LB_GETCURSEL: u32 = 0x0188;

/// A raw message winsafe has no wrapper for.
fn send(hwnd: &w::HWND, msg_id: u32, wparam: usize, lparam: isize) -> isize {
    unsafe { hwnd.SendMessage(msg::Wm { msg_id: co::WM::from_raw(msg_id), wparam, lparam }) }
}

/// Menu and accelerator command IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Cmd {
    Open = 100,
    Export,
    BatchConvert,
    CloseTab,
    Exit,
    Find,
    Sidebar,
    ElementList,
    Preferences,
    QuickBookmark,
    NamedBookmark,
    AddNote,
    SaveQuote,
    ListBookmarks,
    ListNotes,
    ListQuotes,
    ExportAnnotations,
    TtsRead,
    TtsPause,
    TtsStop,
    VoiceProfiles,
    RunOcr,
    CancelOcr,
    LabelFields,
    FillForm,
    ClearCache,
    UserGuide,
    Shortcuts,
    CheckUpdates,
    About,
    // Keyboard only.
    FindNext,
    FindPrev,
    FocusNext,
    FocusPrev,
    NextTab,
    PrevTab,
    Escape,
    SkipForward,
    SkipBackward,
}

const ALL_CMDS: [Cmd; 39] = {
    use Cmd::*;
    [
        Open, Export, BatchConvert, CloseTab, Exit, Find, Sidebar, ElementList, Preferences, QuickBookmark, NamedBookmark, AddNote, SaveQuote,
        ListBookmarks, ListNotes, ListQuotes, ExportAnnotations, TtsRead, TtsPause, TtsStop, VoiceProfiles, RunOcr, CancelOcr, LabelFields,
        FillForm, ClearCache, UserGuide, Shortcuts, CheckUpdates, About, FindNext, FindPrev, FocusNext, FocusPrev, NextTab, PrevTab, Escape,
        SkipForward, SkipBackward,
    ]
};

const VK_TAB: u16 = 0x09;
const VK_ESCAPE: u16 = 0x1B;
const VK_LEFT: u16 = 0x25;
const VK_RIGHT: u16 = 0x27;
const VK_F1: u16 = 0x70;
const VK_OEM_COMMA: u16 = 0xBC;
const VK_OEM_2: u16 = 0xBF;

const fn f(n: u16) -> u16 {
    VK_F1 + n - 1
}

/// A key chord: virtual key plus modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    pub vk: u16,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

const fn key(vk: u16) -> Shortcut {
    Shortcut { vk, ctrl: false, shift: false, alt: false }
}
const fn shift(vk: u16) -> Shortcut {
    Shortcut { vk, ctrl: false, shift: true, alt: false }
}
const fn ctrl(vk: u16) -> Shortcut {
    Shortcut { vk, ctrl: true, shift: false, alt: false }
}
const fn ctrl_shift(vk: u16) -> Shortcut {
    Shortcut { vk, ctrl: true, shift: true, alt: false }
}
const fn alt(vk: u16) -> Shortcut {
    Shortcut { vk, ctrl: false, shift: false, alt: true }
}

/// Every keyboard shortcut. Menu items show theirs after a tab.
pub const SHORTCUTS: &[(Cmd, Shortcut)] = &[
    (Cmd::Open, ctrl(b'O' as u16)),
    (Cmd::Export, ctrl_shift(b'S' as u16)),
    (Cmd::BatchConvert, ctrl_shift(b'P' as u16)),
    (Cmd::CloseTab, ctrl(b'W' as u16)),
    (Cmd::Find, ctrl(b'F' as u16)),
    (Cmd::Sidebar, key(f(9))),
    (Cmd::ElementList, ctrl_shift(b'E' as u16)),
    (Cmd::Preferences, ctrl(VK_OEM_COMMA)),
    (Cmd::QuickBookmark, ctrl(b'B' as u16)),
    (Cmd::NamedBookmark, ctrl_shift(b'B' as u16)),
    (Cmd::AddNote, ctrl(b'M' as u16)),
    (Cmd::SaveQuote, ctrl_shift(b'H' as u16)),
    (Cmd::ListBookmarks, ctrl_shift(f(2))),
    (Cmd::ListNotes, ctrl_shift(b'M' as u16)),
    (Cmd::ListQuotes, ctrl_shift(b'Q' as u16)),
    (Cmd::TtsRead, key(f(5))),
    (Cmd::TtsPause, shift(f(5))),
    (Cmd::TtsStop, key(f(7))),
    (Cmd::VoiceProfiles, ctrl_shift(b'V' as u16)),
    (Cmd::RunOcr, ctrl_shift(b'O' as u16)),
    (Cmd::LabelFields, ctrl_shift(b'L' as u16)),
    (Cmd::FillForm, ctrl_shift(b'F' as u16)),
    (Cmd::UserGuide, key(f(1))),
    (Cmd::Shortcuts, ctrl(VK_OEM_2)),
    (Cmd::FindNext, key(f(3))),
    (Cmd::FindPrev, shift(f(3))),
    (Cmd::FocusNext, key(f(6))),
    (Cmd::FocusPrev, shift(f(6))),
    (Cmd::NextTab, ctrl(VK_TAB)),
    (Cmd::PrevTab, ctrl_shift(VK_TAB)),
    (Cmd::Escape, key(VK_ESCAPE)),
    (Cmd::SkipForward, alt(VK_RIGHT)),
    (Cmd::SkipBackward, alt(VK_LEFT)),
];

/// "Ctrl+Shift+S", as menus display it.
pub fn shortcut_text(s: Shortcut) -> String {
    let name = match s.vk {
        v if (VK_F1..VK_F1 + 24).contains(&v) => format!("F{}", v - VK_F1 + 1),
        VK_OEM_COMMA => ",".into(),
        VK_OEM_2 => "/".into(),
        VK_TAB => "Tab".into(),
        VK_ESCAPE => "Esc".into(),
        VK_LEFT => "Left".into(),
        VK_RIGHT => "Right".into(),
        v => char::from(v as u8).to_string(),
    };
    let mods = [(s.ctrl, "Ctrl+"), (s.shift, "Shift+"), (s.alt, "Alt+")];
    mods.iter().filter(|(on, _)| *on).map(|(_, m)| *m).collect::<String>() + &name
}

/// The accelerator table entries for `shortcuts`.
pub fn accelerators(shortcuts: &[(Cmd, Shortcut)]) -> Vec<w::ACCEL> {
    shortcuts
        .iter()
        .map(|&(cmd, s)| {
            let mut flags = co::ACCELF::VIRTKEY;
            for (on, flag) in [(s.ctrl, co::ACCELF::CONTROL), (s.shift, co::ACCELF::SHIFT), (s.alt, co::ACCELF::ALT)] {
                if on {
                    flags |= flag;
                }
            }
            w::ACCEL { fVirt: flags, key: unsafe { co::VK::from_raw(s.vk) }, cmd: cmd as u16 }
        })
        .collect()
}

enum M {
    Item(Cmd, &'static str),
    Sep,
    Sub(&'static str, &'static [M]),
    Recent,
}

const MENUS: &[(&str, &[M])] = {
    use Cmd::*;
    use M::{Item, Recent, Sep, Sub};
    &[
        (
            "&File",
            &[
                Item(Open, "&Open"),
                Recent,
                Sep,
                Item(Export, "&Export..."),
                Item(BatchConvert, "&Batch Convert"),
                Sep,
                Item(CloseTab, "Close &Tab"),
                Sep,
                Item(Exit, "E&xit\tAlt+F4"),
            ],
        ),
        (
            "&Edit",
            &[
                Item(Find, "&Find"),
                Sep,
                Sub("&View Options", &[Item(Sidebar, "&Sidebar"), Item(ElementList, "Element &List")]),
                Sep,
                Item(Preferences, "&Preferences"),
            ],
        ),
        (
            "&Reference",
            &[
                Item(QuickBookmark, "Quick &Bookmark"),
                Item(NamedBookmark, "&Named Bookmark"),
                Item(AddNote, "Add &Note"),
                Item(SaveQuote, "Save &Quote"),
                Sep,
                Item(ListBookmarks, "&List Bookmarks"),
                Item(ListNotes, "List N&otes"),
                Item(ListQuotes, "List &Quotes"),
                Sep,
                Item(ExportAnnotations, "&Export Annotations..."),
            ],
        ),
        (
            "Te&xt-to-Speech",
            &[Item(TtsRead, "&Read"), Item(TtsPause, "Pause/Resu&me"), Item(TtsStop, "S&top Reading"), Sep, Item(VoiceProfiles, "&Voice Profiles")],
        ),
        (
            "&Tools",
            &[
                Item(RunOcr, "Run &OCR"),
                Item(CancelOcr, "Cancel O&CR"),
                Sep,
                Item(LabelFields, "Label / Scan Form &Fields"),
                Item(FillForm, "Fill &Form"),
                Sep,
                Item(ClearCache, "Clear &Cache"),
            ],
        ),
        (
            "&Help",
            &[Item(UserGuide, "User &Guide"), Item(Shortcuts, "&Keyboard Shortcuts"), Sep, Item(CheckUpdates, "Check for &Updates..."), Item(About, "&About")],
        ),
    ]
};

/// Menu text: the label plus its shortcut after a tab.
fn menu_text(cmd: Cmd, label: &str) -> String {
    match SHORTCUTS.iter().find(|(c, _)| *c == cmd) {
        Some(&(_, s)) if !label.contains('\t') => format!("{label}\t{}", shortcut_text(s)),
        _ => label.to_string(),
    }
}

fn append(menu: &w::HMENU, items: &[M], recent: &mut Option<w::HMENU>) {
    for item in items {
        let _ = match item {
            M::Item(cmd, label) => menu.AppendMenu(co::MF::STRING, w::IdMenu::Id(*cmd as u16), w::BmpPtrStr::from_str(&menu_text(*cmd, label))),
            M::Sep => menu.AppendMenu(co::MF::SEPARATOR, w::IdMenu::None, w::BmpPtrStr::None),
            M::Sub(text, sub) => {
                let popup = w::HMENU::CreatePopupMenu().expect("CreatePopupMenu");
                append(&popup, sub, recent);
                menu.AppendMenu(co::MF::POPUP, w::IdMenu::Menu(&popup), w::BmpPtrStr::from_str(text))
            }
            M::Recent => {
                let popup = w::HMENU::CreatePopupMenu().expect("CreatePopupMenu");
                let r = menu.AppendMenu(co::MF::POPUP, w::IdMenu::Menu(&popup), w::BmpPtrStr::from_str("&Recent Files"));
                *recent = Some(popup);
                r
            }
        };
    }
}

/// The menu bar and its Recent Files submenu.
fn build_menu() -> (w::HMENU, w::HMENU) {
    let bar = w::HMENU::CreateMenu().expect("CreateMenu");
    let mut recent = None;
    for (title, items) in MENUS {
        let popup = w::HMENU::CreatePopupMenu().expect("CreatePopupMenu");
        append(&popup, items, &mut recent);
        let _ = bar.AppendMenu(co::MF::POPUP, w::IdMenu::Menu(&popup), w::BmpPtrStr::from_str(title));
    }
    (bar, recent.expect("File menu has Recent Files"))
}

thread_local! {
    static MAIN: RefCell<Option<MainWindow>> = const { RefCell::new(None) };
    static DEFERRED: RefCell<Vec<Box<dyn FnOnce()>>> = const { RefCell::new(Vec::new()) };
}

/// Runs `f` with the main window (UI thread only). No-op after it closed.
pub fn with_main(f: impl FnOnce(&MainWindow)) {
    if let Some(m) = MAIN.with(|m| m.borrow().clone()) {
        f(&m);
    }
}

/// Runs `f` from the message loop after the current message. Background job
/// callbacks arrive inside a cross-thread `SendMessage`, so a modal dialog
/// opened there would hold the job's thread until it closed.
fn defer(f: impl FnOnce() + 'static) {
    DEFERRED.with(|d| d.borrow_mut().push(Box::new(f)));
    with_main(|m| unsafe {
        let _ = PostMessageW(Some(HWND(m.0.wnd.hwnd().ptr())), DEFER_MSG, WPARAM(0), LPARAM(0));
    });
}

type Pending = Box<dyn FnOnce(&MainWindow)>;

#[derive(Default)]
struct Load {
    /// Identifies the current load; a worker whose token no longer matches
    /// (cancelled or superseded) only discards its result.
    token: u64,
    busy: bool,
    filename: String,
    dialog: Option<gui::WindowModal>,
    /// A result that arrived while the progress dialog was up, presented once it closes.
    pending: Option<Pending>,
}

/// What to do once a flat-form scan finishes.
#[derive(Clone, Copy)]
enum AfterScan {
    Announce,
    OpenDialog,
}

struct Ui {
    wnd: gui::WindowMain,
    toc: TocTree,
    tabs: gui::Tab,
    find: FindBar,
    progress: ProgressBar,
    status: gui::StatusBar,
    menu: w::HMENU,
    recent: w::HMENU,
    views: RefCell<Vec<Rc<ContentView>>>,
    active: Cell<Option<usize>>,
    sidebar: Cell<bool>,
    sidebar_width: Cell<i32>,
    dragging: Cell<bool>,
    reader: ReadingController,
    load: RefCell<Load>,
}

#[derive(Clone)]
pub struct MainWindow(Rc<Ui>);

impl MainWindow {
    pub fn new() -> anyhow::Result<Self> {
        let (menu, recent) = build_menu();
        let accel = w::HACCEL::CreateAcceleratorTable(&accelerators(SHORTCUTS))?;
        let menu_copy = unsafe { menu.raw_copy() };
        let wnd = gui::WindowMain::new(gui::WindowMainOpts {
            title: APP_NAME,
            size: gui::dpi(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT - 60),
            style: co::WS::CAPTION
                | co::WS::SYSMENU
                | co::WS::CLIPCHILDREN
                | co::WS::BORDER
                | co::WS::VISIBLE
                | co::WS::SIZEBOX
                | co::WS::MINIMIZEBOX
                | co::WS::MAXIMIZEBOX,
            menu,
            accel_table: Some(accel),
            ..Default::default()
        });
        // Creation order is tab order: TOC, tabs, (reading controls are moved
        // in after the tabs), find bar, progress, status bar.
        let toc = TocTree::new(&wnd);
        let tabs = gui::Tab::new(&wnd, gui::TabOpts { size: gui::dpi(400, 26), ..Default::default() });
        let find = FindBar::new(&wnd);
        let progress = ProgressBar::new(&wnd);
        let status = gui::StatusBar::new(&wnd, &[gui::SbPart::Proportional(2), gui::SbPart::Proportional(1), gui::SbPart::Proportional(1)]);
        let cfg = config::get().window;
        let ui = Rc::new(Ui {
            wnd,
            toc,
            tabs,
            find,
            progress,
            status,
            menu: menu_copy,
            recent,
            views: RefCell::default(),
            active: Cell::new(None),
            sidebar: Cell::new(true),
            sidebar_width: Cell::new(if cfg.sidebar_width > 0 { cfg.sidebar_width } else { crate::constants::SIDEBAR_DEFAULT_WIDTH }),
            dragging: Cell::new(false),
            reader: ReadingController::new(),
            load: RefCell::default(),
        });
        let main = Self(ui);
        main.events();
        MAIN.with(|m| *m.borrow_mut() = Some(main.clone()));
        Ok(main)
    }

    pub fn wnd(&self) -> &gui::WindowMain {
        &self.0.wnd
    }

    /// Shows the window and runs the message loop until it closes.
    pub fn run(&self) -> anyhow::Result<i32> {
        let show = if config::get().window.maximized { co::SW::SHOWMAXIMIZED } else { co::SW::SHOW };
        let r = self.0.wnd.run_main(Some(show));
        MAIN.with(|m| m.borrow_mut().take());
        r.map_err(|e| anyhow::anyhow!("{e}"))
    }

    fn hwnd(&self) -> &w::HWND {
        self.0.wnd.hwnd()
    }

    fn events(&self) {
        let ui = &self.0;
        let m = self.clone();
        ui.wnd.on().wm_create(move |_| {
            m.on_create();
            Ok(0)
        });
        let m = self.clone();
        ui.wnd.on().wm_size(move |_| {
            m.layout();
            Ok(())
        });
        ui.wnd.on().wm_get_min_max_info(|p| {
            let (x, y) = gui::dpi(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT);
            p.info.ptMinTrackSize = w::POINT::with(x, y);
            Ok(())
        });
        let m = self.clone();
        ui.wnd.on().wm_close(move || {
            m.on_close();
            Ok(())
        });
        for cmd in ALL_CMDS {
            let m = self.clone();
            ui.wnd.on().wm_command_acc_menu(cmd as u16, move || {
                m.command(cmd);
                Ok(())
            });
        }
        for i in 0..RECENT_MAX {
            let m = self.clone();
            ui.wnd.on().wm_command_acc_menu(RECENT_BASE + i, move || {
                if let Some(path) = config::get().recent_files.get(i as usize) {
                    m.open_file(Path::new(path));
                }
                Ok(())
            });
        }
        // Outside a dialog, IsDialogMessage turns Enter into IDOK before the
        // focused control sees it.
        let m = self.clone();
        ui.wnd.on().wm_command_acc_menu(IDOK, move || {
            let focus = w::HWND::GetFocus();
            if focus.as_ref() == Some(m.0.toc.tree.hwnd()) {
                if let (Some(page), Some(v)) = (m.0.toc.selected_page(), m.active_view()) {
                    v.go_to_page(page);
                    v.focus();
                }
            } else if m.0.find.is_shown() && focus.is_some_and(|f| m.0.find.contains(&f)) {
                m.find_next(true);
            }
            Ok(())
        });
        ui.wnd.on().wm(unsafe { co::WM::from_raw(DEFER_MSG) }, |_| {
            for f in DEFERRED.with(|d| std::mem::take(&mut *d.borrow_mut())) {
                f();
            }
            Ok(0)
        });
        ui.wnd.on().wm_notify(content_view::CONTROL_ID, w::NmhdrCode::from(crate::platform::win32::richedit::EN_SELCHANGE), |p| {
            let sc = unsafe { p.cast_nmhdr::<crate::platform::win32::richedit::SelChange>() };
            if let Some(v) = ContentView::find(sc.hwnd_from) {
                let (a, b) = sc.range();
                v.on_sel_change(a, b);
            }
            Ok(0)
        });
        let m = self.clone();
        ui.tabs.on().tcn_sel_change(move || {
            if let Some(i) = unsafe { m.0.tabs.hwnd().SendMessage(msg::TcmGetCurSel {}) } {
                m.on_tab_changed(i as usize);
            }
            Ok(())
        });
        let m = self.clone();
        ui.toc.tree.on().tvn_sel_changed(move |p| {
            // Programmatic selection and item deletion report TVC_UNKNOWN (0).
            if p.action != 0
                && let Some(page) = m.0.toc.page_of(p.itemNew.hItem.ptr() as isize)
                && let Some(v) = m.active_view()
            {
                v.go_to_page(page);
            }
            Ok(())
        });
        let m = self.clone();
        ui.progress.slider.on().wm_h_scroll(move |_| {
            if let Some(v) = m.active_view() {
                v.go_to_page(m.0.progress.requested_page());
            }
            Ok(())
        });
        let m = self.clone();
        ui.find.input.on().en_change(move || {
            let _ = m.hwnd().SetTimer(FIND_TIMER, FIND_DEBOUNCE_MS, None);
            Ok(())
        });
        let m = self.clone();
        ui.wnd.on().wm_timer(FIND_TIMER, move || {
            let _ = m.hwnd().KillTimer(FIND_TIMER);
            m.find_next(false);
            Ok(())
        });
        for check in [&ui.find.case, &ui.find.wrap] {
            let m = self.clone();
            check.on().bn_clicked(move || {
                let _ = m.hwnd().KillTimer(FIND_TIMER);
                m.find_next(false);
                Ok(())
            });
        }
        let m = self.clone();
        ui.find.next.on().bn_clicked(move || {
            m.find_next(true);
            Ok(())
        });
        let m = self.clone();
        ui.find.prev.on().bn_clicked(move || {
            m.find_prev();
            Ok(())
        });
        let m = self.clone();
        ui.find.close.on().bn_clicked(move || {
            m.close_find();
            Ok(())
        });
        let m = self.clone();
        ui.wnd.on().wm_timer(LOAD_TIMER, move || {
            m.show_load_progress();
            Ok(())
        });
        self.sash_events();
    }

    /// Mouse resizing of the sidebar by dragging the gap beside it (mouse only;
    /// the width is otherwise kept from the settings).
    fn sash_events(&self) {
        let ui = &self.0;
        let m = self.clone();
        ui.wnd.on().wm_l_button_down(move |p| {
            if m.over_sash(p.coords.x) {
                m.0.dragging.set(true);
                unsafe { SetCapture(HWND(m.hwnd().ptr())) };
            }
            Ok(())
        });
        let m = self.clone();
        ui.wnd.on().wm_mouse_move(move |p| {
            if m.0.dragging.get() {
                m.0.sidebar_width.set(p.coords.x.max(gui::dpi_x(80)));
                m.layout();
            }
            Ok(())
        });
        let m = self.clone();
        ui.wnd.on().wm_l_button_up(move |_| {
            if m.0.dragging.replace(false) {
                let _ = unsafe { ReleaseCapture() };
            }
            Ok(())
        });
        let m = self.clone();
        ui.wnd.on().wm_set_cursor(move |p| {
            let over = p.hwnd == *m.hwnd() && w::GetCursorPos().and_then(|pt| m.hwnd().ScreenToClient(pt)).is_ok_and(|pt| m.over_sash(pt.x));
            if over {
                unsafe {
                    if let Ok(c) = LoadCursorW(None, IDC_SIZEWE) {
                        SetCursor(Some(c));
                    }
                }
            }
            Ok(over)
        });
    }

    fn over_sash(&self, x: i32) -> bool {
        let sw = self.0.sidebar_width.get();
        self.0.sidebar.get() && x >= sw && x < sw + gui::dpi_x(SASH)
    }

    fn on_create(&self) {
        let ui = &self.0;
        crate::speech::set_main_window(self.hwnd().ptr() as isize);
        ui.toc.name_control();
        ui.find.name_controls();
        ui.progress.name_control();
        crate::platform::win32::a11y::set_name(ui.tabs.hwnd().ptr() as isize, "Open documents");
        let _ = ui.status.parts().get(2).set_text("Ready");
        let _ = ui.menu.CheckMenuItem(w::IdPos::Id(Cmd::Sidebar as u16), true);
        let _ = ui.menu.EnableMenuItem(w::IdPos::Id(Cmd::CancelOcr as u16), false);
        self.restore_geometry();
        self.rebuild_recent_menu();
        self.subscribe();
        self.layout();
        if let Some(path) = std::env::args().nth(1).map(PathBuf::from).filter(|p| p.is_file()) {
            self.open_file(&path);
        }
    }

    fn subscribe(&self) {
        let m = self.clone();
        events::subscribe(move |e| match e {
            AppEvent::PageChanged { page_num, page_count } => m.on_page_changed(*page_num, *page_count),
            AppEvent::OcrStarted { .. } => m.set_ocr_running(true, "OCR in progress..."),
            AppEvent::OcrCompleted { .. } => m.set_ocr_running(false, "Ready"),
            // Page 0 means the job never started (no provider); no OcrCompleted follows.
            AppEvent::OcrError { page_num: 0, .. } => m.set_ocr_running(false, "Ready"),
            AppEvent::OcrPageCompleted { page_num, total_pages, .. } => m.set_status(2, &format!("OCR: page {page_num}/{total_pages}")),
            AppEvent::TtsStarted | AppEvent::TtsResumed => m.set_status(2, "Reading..."),
            AppEvent::TtsStopped => m.set_status(2, "Ready"),
            AppEvent::TtsPaused => m.set_status(2, "Paused"),
            AppEvent::ConfigChanged { key } => m.on_config_changed(key),
            _ => {}
        });
    }

    fn set_status(&self, part: u32, text: &str) {
        let _ = self.0.status.parts().get(part).set_text(text);
    }

    fn set_ocr_running(&self, running: bool, status: &str) {
        let _ = self.0.menu.EnableMenuItem(w::IdPos::Id(Cmd::CancelOcr as u16), running);
        let _ = self.0.menu.EnableMenuItem(w::IdPos::Id(Cmd::RunOcr as u16), !running);
        self.set_status(2, status);
    }

    fn on_config_changed(&self, key: &str) {
        if key.starts_with("display") {
            for v in self.0.views.borrow().iter() {
                v.apply_font();
            }
        } else if key == "general.continuous_reading" || key == "general" {
            events::emit(if config::get().general.continuous_reading {
                AppEvent::ContinuousReadingStarted
            } else {
                AppEvent::ContinuousReadingStopped
            });
        } else if key == "recent_files" {
            self.rebuild_recent_menu();
        }
    }

    // Layout

    fn layout(&self) {
        let ui = &self.0;
        let Ok(rc) = self.hwnd().GetClientRect() else { return };
        let (width, height) = (rc.right, rc.bottom);
        let status_h = ui.status.hwnd().GetWindowRect().map_or(0, |r| r.bottom - r.top);
        let row = gui::dpi_y(30);
        let pad = gui::dpi_x(4);
        let mut bottom = height - status_h - row;
        ui.progress.layout(pad, bottom + gui::dpi_y(3), width - 2 * pad, row - gui::dpi_y(6));
        if ui.find.is_shown() {
            bottom -= row;
            ui.find.layout(pad, bottom + gui::dpi_y(3), width - 2 * pad);
        }
        let place = |h: &w::HWND, x: i32, y: i32, cx: i32, cy: i32| {
            let _ = h.SetWindowPos(w::HwndPlace::None, w::POINT::with(x, y), w::SIZE::with(cx.max(0), cy.max(0)), co::SWP::NOZORDER | co::SWP::NOACTIVATE);
        };
        let mut x = 0;
        if ui.sidebar.get() {
            let sw = ui.sidebar_width.get().clamp(gui::dpi_x(80), (width - gui::dpi_x(200)).max(gui::dpi_x(80)));
            ui.sidebar_width.set(sw);
            place(ui.toc.tree.hwnd(), 0, 0, sw, bottom);
            x = sw + gui::dpi_x(SASH);
        }
        let tab_count = ui.views.borrow().len();
        let tab_h = if tab_count == 0 {
            0
        } else {
            let mut r = w::RECT::default();
            let _ = unsafe { ui.tabs.hwnd().SendMessage(msg::TcmGetItemRect { index: 0, rect: &mut r }) };
            r.bottom + gui::dpi_y(2)
        };
        ui.tabs.hwnd().ShowWindow(if tab_count == 0 { co::SW::HIDE } else { co::SW::SHOW });
        place(ui.tabs.hwnd(), x, 0, width - x, tab_h);
        if let Some(v) = self.active_view() {
            let h = unsafe { w::HWND::from_ptr(v.edit.0 as _) };
            place(&h, x, tab_h, width - x, bottom - tab_h);
        }
    }

    // Tabs and views

    pub fn active_view(&self) -> Option<Rc<ContentView>> {
        self.0.active.get().and_then(|i| self.0.views.borrow().get(i).cloned())
    }

    /// Shows view `index` and hides the others.
    fn activate(&self, index: usize) {
        self.0.active.set(Some(index));
        for (i, v) in self.0.views.borrow().iter().enumerate() {
            unsafe { w::HWND::from_ptr(v.edit.0 as _) }.ShowWindow(if i == index { co::SW::SHOW } else { co::SW::HIDE });
        }
        unsafe {
            let _ = self.0.tabs.hwnd().SendMessage(msg::TcmSetCurSel { index: index as u32 });
        }
        self.layout();
    }

    fn sync_toc(&self, v: &ContentView) {
        let (title, toc, use_toc, count) = v.with_doc(|d| {
            let meta = d.metadata();
            let title = if meta.title.is_empty() { "Document".to_string() } else { meta.title };
            (title, d.toc().to_vec(), !d.toc().is_empty() && d.capabilities().contains(Capabilities::TOC), d.page_count())
        });
        self.0.toc.populate(&title, &toc, use_toc, count);
        self.0.toc.select_page(v.current_page());
    }

    fn on_tab_changed(&self, index: usize) {
        let Some(v) = self.0.views.borrow().get(index).cloned() else { return };
        self.activate(index);
        self.sync_toc(&v);
        self.set_status(0, &v.filename);
        self.show_page_state(v.current_page() + 1, v.page_count());
        events::emit(AppEvent::TabSwitched { tab_index: index, filename: v.filename.clone() });
        announce(v.filename.clone());
    }

    fn switch_tab(&self, step: isize) {
        let count = self.0.views.borrow().len();
        if count < 2 {
            return;
        }
        let cur = self.0.active.get().unwrap_or(0) as isize;
        self.on_tab_changed((cur + step).rem_euclid(count as isize) as usize);
        // The previous tab's control had focus and is now hidden.
        if let Some(v) = self.active_view() {
            v.focus();
        }
    }

    fn close_tab(&self) {
        let Some(index) = self.0.active.get() else { return };
        self.0.reader.stop();
        let v = self.0.views.borrow_mut().remove(index);
        unsafe {
            let _ = self.0.tabs.hwnd().SendMessage(msg::TcmDeleteItem { index: index as u32 });
        }
        v.close();
        events::emit(AppEvent::DocumentClosed { filename: v.filename.clone() });
        let remaining = self.0.views.borrow().len();
        if remaining == 0 {
            self.0.active.set(None);
            self.0.toc.clear();
            self.set_status(0, "");
            self.set_status(1, "");
            let _ = self.hwnd().SetWindowText(APP_NAME);
            self.layout();
            // The closed control had focus; keep the keyboard somewhere useful.
            let _ = if self.0.sidebar.get() { self.0.toc.tree.focus() } else { self.0.progress.slider.focus() };
        } else {
            let next = index.min(remaining - 1);
            self.activate(next);
            if let Some(v) = self.active_view() {
                self.sync_toc(&v);
                self.show_page_state(v.current_page() + 1, v.page_count());
                v.focus();
            }
        }
    }

    // Pages

    fn show_page_state(&self, page_num: usize, page_count: usize) {
        self.set_status(1, &format!("Page {page_num} of {page_count}"));
        self.0.progress.update(page_num, page_count);
    }

    fn on_page_changed(&self, page_num: usize, page_count: usize) {
        self.show_page_state(page_num, page_count);
        self.0.toc.select_page(page_num.saturating_sub(1));
        // A continuous-reading turn stays silent: an interrupting announcement
        // would stop the screen reader's say-all at every page boundary.
        if !self.active_view().is_some_and(|v| v.is_advancing()) {
            announce(format!("Page {page_num} of {page_count}"));
        }
    }

    // Keyboard

    fn command(&self, cmd: Cmd) {
        match cmd {
            Cmd::Open => self.on_open(),
            Cmd::Export => self.on_export(),
            Cmd::BatchConvert => batch_dialog::show(self.wnd()),
            Cmd::CloseTab => self.close_tab(),
            Cmd::Exit => self.0.wnd.close(),
            Cmd::Find => self.open_find(),
            Cmd::Sidebar => self.toggle_sidebar(),
            Cmd::ElementList => self.on_element_list(),
            Cmd::Preferences => {
                preferences::show(self.wnd());
            }
            Cmd::QuickBookmark => self.quick_bookmark(),
            Cmd::NamedBookmark => self.named_bookmark(),
            Cmd::AddNote => self.add_note(),
            Cmd::SaveQuote => self.save_quote(),
            Cmd::ListBookmarks => self.list_bookmarks(),
            Cmd::ListNotes => self.list_notes(),
            Cmd::ListQuotes => self.list_quotes(),
            Cmd::ExportAnnotations => self.export_annotations(),
            Cmd::TtsRead => {
                if let Some(v) = self.active_view() {
                    self.0.reader.start(Arc::new(ViewTarget(v.edit.0)), config::get().tts.start_from == "cursor");
                }
            }
            Cmd::TtsPause => match self.0.reader.state() {
                ReadingState::Playing => self.0.reader.pause(),
                ReadingState::Paused => self.0.reader.resume(),
                ReadingState::Stopped => {}
            },
            Cmd::TtsStop => self.0.reader.stop(),
            Cmd::SkipForward => self.0.reader.skip_forward(),
            Cmd::SkipBackward => self.0.reader.skip_backward(),
            Cmd::VoiceProfiles => voice_profiles::show(self.wnd()),
            Cmd::RunOcr => self.on_run_ocr(),
            Cmd::CancelOcr => {
                PROCESSING_MANAGER.cancel();
                announce("Cancelling OCR...");
            }
            Cmd::LabelFields => self.on_label_fields(),
            Cmd::FillForm => self.on_fill_form(),
            Cmd::ClearCache => self.on_clear_cache(),
            Cmd::UserGuide => self.on_user_guide(),
            Cmd::Shortcuts => shortcuts_dialog::show(self.wnd()),
            Cmd::CheckUpdates => updater_ui::check_for_updates(self.wnd(), false),
            Cmd::About => self.on_about(),
            Cmd::FindNext => {
                if self.0.find.is_shown() {
                    self.find_next(true);
                }
            }
            Cmd::FindPrev => {
                if self.0.find.is_shown() {
                    self.find_prev();
                }
            }
            Cmd::FocusNext => self.cycle_focus(1),
            Cmd::FocusPrev => self.cycle_focus(-1),
            Cmd::NextTab => self.switch_tab(1),
            Cmd::PrevTab => self.switch_tab(-1),
            Cmd::Escape => {
                if self.0.find.is_shown() {
                    self.close_find();
                } else if let Some(v) = self.active_view() {
                    v.focus();
                    announce("Content");
                }
            }
        }
    }

    /// F6 / Shift+F6: table of contents, content, progress (skipping the
    /// hidden sidebar).
    fn cycle_focus(&self, step: isize) {
        const NAMES: [&str; 3] = ["Table of Contents", "Content", "Progress"];
        let ui = &self.0;
        let focus = w::HWND::GetFocus();
        let current = focus.map_or(-1, |f| {
            if f == *ui.toc.tree.hwnd() {
                0
            } else if f == *ui.tabs.hwnd() || ContentView::find(f.ptr() as isize).is_some() {
                1
            } else if f == *ui.progress.slider.hwnd() {
                2
            } else {
                -1
            }
        });
        let mut next = (current + step).rem_euclid(3);
        if !ui.sidebar.get() && next == 0 {
            next = (next + step).rem_euclid(3);
        }
        match next {
            0 => {
                let _ = ui.toc.tree.focus();
            }
            1 => {
                if let Some(v) = self.active_view() {
                    v.focus();
                }
            }
            _ => {
                let _ = ui.progress.slider.focus();
            }
        }
        announce(NAMES[next as usize]);
    }

    fn toggle_sidebar(&self) {
        let shown = !self.0.sidebar.get();
        self.0.sidebar.set(shown);
        self.0.toc.tree.hwnd().ShowWindow(if shown { co::SW::SHOW } else { co::SW::HIDE });
        let _ = self.0.menu.CheckMenuItem(w::IdPos::Id(Cmd::Sidebar as u16), shown);
        self.layout();
        announce(if shown { "Sidebar shown" } else { "Sidebar hidden" });
    }

    // Find bar

    fn open_find(&self) {
        self.0.find.set_shown(true);
        self.layout();
        self.0.find.focus_input();
    }

    fn close_find(&self) {
        let _ = self.hwnd().KillTimer(FIND_TIMER);
        self.0.find.set_shown(false);
        self.layout();
        if let Some(v) = self.active_view() {
            v.focus();
        }
    }

    /// Re-runs the search when the query or an option changed. False when nothing matches.
    fn ensure_search(&self, v: &ContentView) -> Option<String> {
        // A search now supersedes the pending live search for the same text.
        let _ = self.hwnd().KillTimer(FIND_TIMER);
        let find = &self.0.find;
        let query = find.query();
        if query.is_empty() {
            return None;
        }
        if find.has_query_changed() && v.execute_search(&query, find.case_sensitive(), find.wrap_around()) == 0 {
            find.set_current_match(0, 0);
            announce(format!("No matches for {query}"));
            return None;
        }
        Some(query)
    }

    /// `jump` moves focus to the match (Enter, F3, Next); live search while
    /// typing leaves focus in the query box.
    fn find_next(&self, jump: bool) {
        let Some(v) = self.active_view() else { return };
        if self.ensure_search(&v).is_none() {
            return;
        }
        let focus = w::HWND::GetFocus();
        v.next_match();
        if !jump && let Some(f) = focus {
            f.SetFocus();
        }
        let (idx, total) = v.match_info();
        self.0.find.set_current_match(idx, total);
    }

    fn find_prev(&self) {
        let Some(v) = self.active_view() else { return };
        if self.ensure_search(&v).is_none() {
            return;
        }
        v.prev_match();
        let (idx, total) = v.match_info();
        self.0.find.set_current_match(idx, total);
    }

    fn on_element_list(&self) {
        let Some(v) = self.active_view() else { return };
        let elements = v.with_doc(|d| element_nav::get_all_elements(d, None));
        if let Some(chosen) = element_list_dialog::show(self.wnd(), elements) {
            v.go_to_element(chosen.page_index, &chosen.element);
        }
    }

    // Opening documents

    fn on_open(&self) {
        let start = config::get()
            .recent_files
            .first()
            .and_then(|p| Path::new(p).parent().map(|d| d.to_path_buf()))
            .filter(|d| d.is_dir())
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
            .unwrap_or_default();
        if let Some(path) = pick_open_file(self.hwnd(), &registry::file_filters(), &start) {
            self.open_file(Path::new(&path));
        }
    }

    /// Opens `path`; parsing runs on a worker thread so a large or
    /// pathological file never locks the UI.
    pub fn open_file(&self, path: &Path) {
        if self.0.load.borrow().busy {
            announce("A document is already loading. Please wait.");
            return;
        }
        // Recent Files must reopen from any working folder.
        let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let path = absolute.as_path();
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let mut member = None;
        if ext == "zip" || ext == "rar" {
            match archive::list_documents(path) {
                Ok(docs) if docs.len() > 1 => match choose_member(self.wnd(), &docs) {
                    Some(m) => member = Some(m),
                    None => return,
                },
                Ok(_) => {}
                Err(e) => return self.show_load_error(path, e),
            }
        }
        self.start_load(path.to_path_buf(), member, None, false);
    }

    fn start_load(&self, path: PathBuf, member: Option<String>, password: Option<String>, attempted: bool) {
        let token = {
            let mut load = self.0.load.borrow_mut();
            load.token += 1;
            load.busy = true;
            load.filename = file_name(&path);
            load.token
        };
        let _ = self.hwnd().SetTimer(LOAD_TIMER, LOAD_PROGRESS_DELAY_MS, None);
        let cfg = config::get();
        let opts = OpenOptions { password, pdf_reading_mode: cfg.document.pdf_reading_mode, page_size: cfg.general.page_size };
        let spawned = std::thread::Builder::new().name("doc-load-worker".into()).spawn(move || {
            let result = match &member {
                Some(m) => archive::open_member(&path, m, &opts),
                None => registry::open_document(&path, &opts),
            };
            events::run_on_ui(move || with_main(|m| m.on_loaded(token, path, member, attempted, result)));
        });
        if let Err(e) = spawned {
            self.0.load.borrow_mut().busy = false;
            log::error!("cannot start the document loader: {e}");
        }
    }

    fn on_loaded(&self, token: u64, path: PathBuf, member: Option<String>, attempted: bool, result: Result<Box<dyn Document>, OpenError>) {
        let dialog = {
            let mut load = self.0.load.borrow_mut();
            if token != load.token {
                return; // cancelled or superseded: drop the result
            }
            let finish: Pending = Box::new(move |m| m.finish_load(path, member, attempted, result));
            match load.dialog.clone() {
                Some(d) => {
                    load.pending = Some(finish);
                    Some(d)
                }
                None => {
                    load.busy = false;
                    drop(load);
                    let _ = self.hwnd().KillTimer(LOAD_TIMER);
                    finish(self);
                    None
                }
            }
        };
        if let Some(d) = dialog {
            d.close();
        }
    }

    /// The deferred, cancellable "Opening Document" dialog.
    fn show_load_progress(&self) {
        let _ = self.hwnd().KillTimer(LOAD_TIMER);
        let filename = {
            let load = self.0.load.borrow();
            if !load.busy || load.dialog.is_some() {
                return;
            }
            load.filename.clone()
        };
        let d = DialogBuilder::new("Opening Document", 360);
        d.text(&format!("Loading {filename}..."));
        let bar = d.column().progress();
        d.buttons(&[("Cancel", IDCANCEL)]);
        d.wnd.on().wm_show_window(move |p| {
            if p.being_shown {
                // Marquee: the parse cannot report progress.
                let h = bar.hwnd();
                let _ = unsafe { h.SetWindowLongPtr(co::GWLP::STYLE, h.GetWindowLongPtr(co::GWLP::STYLE) | 0x08) };
                send(h, PBM_SETMARQUEE, 1, 30);
            }
            Ok(())
        });
        self.0.load.borrow_mut().dialog = Some(d.wnd.clone());
        d.run(self.wnd());
        let pending = {
            let mut load = self.0.load.borrow_mut();
            load.dialog = None;
            match load.pending.take() {
                Some(p) => {
                    load.busy = false;
                    Some(p)
                }
                None if load.busy => {
                    // The parse cannot be interrupted; its late result is discarded.
                    load.token += 1;
                    load.busy = false;
                    None
                }
                None => None,
            }
        };
        match pending {
            Some(finish) => finish(self),
            None => announce("Document loading cancelled."),
        }
    }

    /// `attempted`: a password was already supplied, so a password error now means it was wrong.
    fn finish_load(&self, path: PathBuf, member: Option<String>, attempted: bool, result: Result<Box<dyn Document>, OpenError>) {
        match result {
            Ok(doc) => self.present(doc, &path),
            Err(OpenError::PasswordRequired | OpenError::WrongPassword) if !attempted => {
                if let Some(pw) = prompt_password(self.wnd(), &file_name(&path)) {
                    self.start_load(path, member, Some(pw), true);
                }
            }
            Err(e) => self.show_load_error(&path, e),
        }
    }

    fn show_load_error(&self, path: &Path, e: OpenError) {
        let name = file_name(path);
        let cannot = format!("Cannot open file: {name}");
        match &e {
            OpenError::PasswordRequired | OpenError::WrongPassword => show_error(
                self.wnd(),
                "Password Error",
                "The password you entered is incorrect.",
                None,
                &["Try entering the password again.", "Check if Caps Lock is on."],
            ),
            OpenError::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied => {
                show_error(self.wnd(), "Permission Error", &cannot, Some(&e.to_string()), &["Check the file permissions."])
            }
            OpenError::Unsupported(_) => show_error(
                self.wnd(),
                "Unsupported Format",
                &e.to_string(),
                None,
                &["Try opening a different file format.", "Check if the required library is installed."],
            ),
            OpenError::Unavailable(_) => {
                show_error(self.wnd(), "Missing Library", &e.to_string(), None, &["Install the program or component this format needs."])
            }
            _ => {
                log::error!("failed to open {}: {e}", path.display());
                show_error(self.wnd(), "File Error", &cannot, Some(&e.to_string()), &[]);
            }
        }
    }

    /// Builds the tab for a loaded document and wires it up.
    fn present(&self, doc: Box<dyn Document>, path: &Path) {
        let filename = file_name(path);
        let meta = doc.metadata();
        let caps = doc.capabilities();
        let title = if meta.title.is_empty() { filename.clone() } else { meta.title.clone() };
        let view = match ContentView::new(self.hwnd().ptr() as isize, doc, filename.clone()) {
            Ok(v) => v,
            Err(e) => {
                log::error!("failed to present {}: {e:#}", path.display());
                return show_error(self.wnd(), "File Error", &format!("Cannot open file: {filename}"), Some(&format!("{e:#}")), &[]);
            }
        };
        // New windows go last in tab order; the reading control belongs right after the tabs.
        let edit = unsafe { w::HWND::from_ptr(view.edit.0 as _) };
        let _ = edit.SetWindowPos(w::HwndPlace::Hwnd(unsafe { self.0.tabs.hwnd().raw_copy() }), w::POINT::default(), w::SIZE::default(), co::SWP::NOMOVE | co::SWP::NOSIZE);

        let index = self.0.views.borrow().len();
        let mut text = w::WString::from_str(&title.chars().take(40).collect::<String>());
        let mut item = w::TCITEM::default();
        item.mask = co::TCIF::TEXT;
        item.set_pszText(Some(&mut text));
        unsafe {
            let _ = self.0.tabs.hwnd().SendMessage(msg::TcmInsertItem { index: index as u32, item: &item });
        }
        self.0.views.borrow_mut().push(view.clone());
        self.activate(index);
        self.sync_toc(&view);
        view.show_current_page();
        let page_count = view.page_count();

        let _ = self.hwnd().SetWindowText(&if meta.author.is_empty() {
            format!("{title} - {APP_NAME}")
        } else {
            format!("{title} by {} - {APP_NAME}", meta.author)
        });
        config::update("recent_files", |s| s.add_recent_file(&path.to_string_lossy()));
        self.set_status(0, &if meta.format.is_empty() { filename.clone() } else { format!("{filename} [{}]", meta.format) });

        events::emit(AppEvent::DocumentOpened { filename: filename.clone(), page_count });
        events::emit(AppEvent::DocumentLoaded { path: path.to_string_lossy().into_owned(), filename: filename.clone() });

        let mut msg = format!("Opened {title}, {page_count} pages");
        if caps.contains(Capabilities::NEEDS_OCR) {
            msg += ". This document may need OCR processing.";
        }
        if caps.contains(Capabilities::FILLABLE_FORM) {
            let n = view.with_doc(|d| d.form_fields().len());
            msg += &format!(". {n} fillable form fields detected.");
        } else if caps.contains(Capabilities::FLAT_FORM) && !view.with_doc(|d| d.as_pdf().is_some_and(|p| p.flat_form_already_filled())) {
            msg += ". This looks like a printed form with blanks to fill in. Press Control+Shift+L to scan it for fillable blanks.";
        }
        announce(msg);
        // Land in the reading area so the document reads without tabbing first.
        view.focus();

        // Flat forms are not offered on open: the announcement points to
        // Ctrl+Shift+L, which keeps OCR's prompt unobstructed.
        if caps.contains(Capabilities::NEEDS_OCR) && is_ocr_available(None) {
            let text = "This document appears to be scanned or image-based and may have little or no selectable text.\n\nWould you like to run AI-powered OCR to extract the text?";
            if self.ask(text, "Run OCR?", false) {
                self.trigger_ocr(&view);
            }
        }
    }

    fn rebuild_recent_menu(&self) {
        let recent = &self.0.recent;
        while recent.GetMenuItemCount().unwrap_or(0) > 0 {
            if recent.DeleteMenu(w::IdPos::Pos(0)).is_err() {
                break;
            }
        }
        for (i, path) in config::get().recent_files.iter().take(RECENT_MAX as usize).enumerate() {
            let _ = recent.AppendMenu(co::MF::STRING, w::IdMenu::Id(RECENT_BASE + i as u16), w::BmpPtrStr::from_str(path));
        }
    }

    // OCR and forms

    fn on_run_ocr(&self) {
        match self.active_view() {
            Some(v) => self.trigger_ocr(&v),
            None => announce("No document open"),
        }
    }

    fn trigger_ocr(&self, v: &Rc<ContentView>) {
        if !is_ocr_available(None) {
            return show_error(
                self.wnd(),
                "OCR Not Available",
                "No OCR backend is configured or installed.",
                None,
                &[
                    "Add a Gemini API key in Preferences > AI Providers, or",
                    "Install a local engine (Windows OCR language pack or Tesseract), then",
                    "Select a backend under Preferences > AI Providers.",
                ],
            );
        }
        let mode = config::get().gemini.default_mode;
        let hwnd = v.edit.0;
        let (pdf, path, count, needs_ocr, mut pages) = v.with_doc(|d| {
            let pdf = d.as_pdf().map(|p| p.path().to_path_buf());
            (pdf, d.path().to_path_buf(), d.page_count(), d.capabilities().contains(Capabilities::NEEDS_OCR), d.ocr_page_indices())
        });
        let outcome = if let Some(pdf_path) = pdf {
            if pages.is_empty() {
                pages = (0..count).collect();
            }
            PROCESSING_MANAGER.process_pdf(
                pdf_path,
                pages,
                v.filename.clone(),
                mode,
                Some(Box::new(move |index, result| {
                    if let Some(v) = ContentView::find(hwnd) {
                        v.update_ocr_page(index, result);
                    }
                })),
                None,
                Some(Box::new(move |_, _| {
                    if let Some(v) = ContentView::find(hwnd) {
                        v.show_current_page();
                    }
                })),
            )
        } else if needs_ocr {
            PROCESSING_MANAGER.process_image(
                path,
                v.filename.clone(),
                mode,
                Some(Box::new(move |result| {
                    if let Some(v) = ContentView::find(hwnd) {
                        v.update_ocr_page(0, result);
                        v.show_current_page();
                    }
                })),
                // An image job that fails sends no OcrCompleted.
                Some(Box::new(|_| with_main(|m| m.set_ocr_running(false, "Ready")))),
            )
        } else {
            return announce("This document does not need OCR.");
        };
        self.check_started(outcome);
    }

    /// Reports a job that did not start. True when it started.
    fn check_started(&self, outcome: StartOutcome) -> bool {
        match outcome {
            StartOutcome::Started => true,
            StartOutcome::AlreadyRunning => {
                announce("Another OCR or form job is already running.");
                false
            }
            StartOutcome::TesseractMissing => {
                announce(tesseract::MISSING_ANNOUNCEMENT);
                let message = tesseract::missing_dialog_message();
                let d = DialogBuilder::new(tesseract::MISSING_DIALOG_TITLE, 460);
                d.column().text(&message, wrapped_lines(&message));
                let download = d.buttons(&[("OK", IDOK), ("&Download Tesseract", 0)]).pop().unwrap();
                let (wnd, owner) = (d.wnd.clone(), unsafe { self.hwnd().raw_copy() });
                download.on().bn_clicked(move || {
                    let _ = owner.ShellExecute("open", tesseract::DOWNLOAD_URL, None, None, co::SW::SHOWNORMAL);
                    wnd.close();
                    Ok(())
                });
                d.run(self.wnd());
                false
            }
        }
    }

    fn on_label_fields(&self) {
        let Some(v) = self.active_view() else { return };
        let caps = v.with_doc(|d| d.capabilities());
        if !caps.contains(Capabilities::FILLABLE_FORM) {
            // A flat form: scan for blanks, announce, and leave filling to Ctrl+Shift+F.
            if caps.contains(Capabilities::FLAT_FORM) {
                self.run_flat_form_scan(&v, true, AfterScan::Announce);
            } else {
                announce("No form fields in this document");
            }
            return;
        }
        if !GeminiClient::is_available() {
            return show_error(
                self.wnd(),
                "AI Not Available",
                "Gemini AI is not configured for form field labeling.",
                None,
                &["Configure your API key in Preferences > AI Providers."],
            );
        }
        let fields = v.with_doc(|d| d.form_fields());
        self.label_fields(&v, fields, true, false);
    }

    /// Sends `fields` for AI labelling; `force` regenerates cached labels.
    /// `then_fill` opens the form dialog afterwards.
    fn label_fields(&self, v: &ContentView, fields: Vec<FormField>, force: bool, then_fill: bool) {
        let Some(pdf_path) = v.with_doc(|d| d.as_pdf().map(|p| p.path().to_path_buf())) else { return };
        let mut by_page: BTreeMap<usize, Vec<FormField>> = BTreeMap::new();
        for f in fields {
            by_page.entry(f.page_index).or_default().push(f);
        }
        let hwnd = v.edit.0;
        let outcome = PROCESSING_MANAGER.label_pdf_form_fields(
            pdf_path,
            by_page,
            v.filename.clone(),
            config::get().gemini.default_mode,
            force,
            Some(Box::new(move |results| {
                if then_fill {
                    announce("Labels applied. Opening form dialog.");
                    defer(move || with_main(|m| m.open_form_dialog(hwnd)));
                } else {
                    let count: usize = results.values().map(Vec::len).sum();
                    announce(format!("Form field labels applied to {count} fields"));
                }
            })),
            None,
        );
        self.check_started(outcome);
    }

    fn on_fill_form(&self) {
        let Some(v) = self.active_view() else { return announce("No document open") };
        let caps = v.with_doc(|d| d.capabilities());
        if !caps.contains(Capabilities::FILLABLE_FORM) {
            if !caps.contains(Capabilities::FLAT_FORM) {
                return announce("No form fields in this document");
            }
            // Open straight away when already scanned; else offer to scan first.
            if v.with_doc(|d| d.as_pdf().is_some_and(|p| !p.flat_form_fields().is_empty())) {
                self.open_flat_form_dialog(&v);
            } else if self.ask(
                "This looks like a printed form with blanks, but it has not been scanned yet.\n\nWould you like to scan it for fillable blanks now?\n(You can also scan it via Tools > Label / Scan Form Fields, Ctrl+Shift+L.)",
                "Scan This Form?",
                false,
            ) {
                self.run_flat_form_scan(&v, false, AfterScan::OpenDialog);
            }
            return;
        }
        let fields = v.with_doc(|d| d.form_fields());
        if fields.is_empty() {
            return announce("No form fields found");
        }
        let unlabeled: Vec<FormField> = fields.iter().filter(|f| f.label.is_none()).cloned().collect();
        if !unlabeled.is_empty()
            && GeminiClient::is_available()
            && self.ask(
                &format!(
                    "This form has {} of {} fields without an author-provided label.\n\nWould you like to use AI to generate accessible labels for them?\n(You can also label them later via Tools > Label Form Fields)",
                    unlabeled.len(),
                    fields.len()
                ),
                "Label Form Fields?",
                false,
            )
        {
            // Only fields without an author label (/TU) go to the AI.
            return self.label_fields(&v, unlabeled, false, true);
        }
        self.open_form_dialog(v.edit.0);
    }

    fn open_form_dialog(&self, hwnd: isize) {
        let Some(v) = ContentView::find(hwnd) else { return };
        let filename = v.filename.clone();
        v.with_doc(|d| {
            let fields = d.form_fields();
            if let Some(pdf) = d.as_pdf() {
                log::debug!("opening the form dialog for {filename}: {} fields", fields.len());
                form_dialog::show(self.wnd(), pdf, fields, &filename);
            }
        });
    }

    fn open_flat_form_dialog(&self, v: &ContentView) {
        let filename = v.filename.clone();
        v.with_doc(|d| {
            let Some(pdf) = d.as_pdf() else { return };
            let fields = pdf.flat_form_fields();
            if fields.is_empty() {
                announce("No fillable blanks found in this form.");
            } else {
                form_dialog::show(self.wnd(), pdf, fields, &filename);
            }
        });
    }

    fn run_flat_form_scan(&self, v: &ContentView, force: bool, after: AfterScan) {
        if !GeminiClient::is_available() {
            return show_error(
                self.wnd(),
                "AI Not Available",
                "Gemini AI is needed to find the blanks on this form.",
                None,
                &["Configure your API key in Preferences > AI Providers."],
            );
        }
        let (pdf_path, pages) = v.with_doc(|d| match d.as_pdf() {
            Some(p) => (Some(p.path().to_path_buf()), p.flat_form_candidate_pages().unwrap_or_default()),
            None => (None, Vec::new()),
        });
        let Some(pdf_path) = pdf_path.filter(|_| !pages.is_empty()) else {
            return announce("No fillable blanks found in this form.");
        };
        let hwnd = v.edit.0;
        let outcome = PROCESSING_MANAGER.discover_flat_form_fields(
            pdf_path,
            pages,
            v.filename.clone(),
            config::get().gemini.default_mode,
            force,
            Some(Box::new(move |_| {
                defer(move || {
                    let Some(v) = ContentView::find(hwnd) else { return };
                    let n = v.with_doc(|d| d.as_pdf().map_or(0, |p| p.flat_form_fields().len()));
                    match after {
                        AfterScan::Announce if n > 0 => announce(format!("Found {n} fillable blanks. Press Control+Shift+F to fill in the form.")),
                        AfterScan::Announce => announce("No fillable blanks found in this form."),
                        AfterScan::OpenDialog if n > 0 => with_main(|m| m.open_flat_form_dialog(&v)),
                        AfterScan::OpenDialog => {}
                    }
                })
            })),
            None,
        );
        self.check_started(outcome);
    }

    // Annotations

    /// (view, document hash, page, caret), or announces "No document open".
    fn annotation_context(&self) -> Option<(Rc<ContentView>, String, usize, usize)> {
        let Some(v) = self.active_view() else {
            announce("No document open");
            return None;
        };
        let hash = v.document_hash();
        let (page, caret) = (v.current_page(), v.caret());
        Some((v, hash, page, caret))
    }

    fn quick_bookmark(&self) {
        let Some((_, hash, page, caret)) = self.annotation_context() else { return };
        report(AnnotationManager::global().add_bookmark(&hash, page, caret, ""), "save the bookmark");
    }

    fn named_bookmark(&self) {
        let Some((_, hash, page, caret)) = self.annotation_context() else { return };
        let d = DialogBuilder::new("Named Bookmark", 360);
        let name = d.edit("Bookmark &name:", "");
        d.ok_cancel("OK");
        if let Some(name) = text_on_ok(&d, name, self.wnd())
            && !name.trim().is_empty()
        {
            report(AnnotationManager::global().add_bookmark(&hash, page, caret, name.trim()), "save the bookmark");
        }
    }

    fn add_note(&self) {
        let Some((_, hash, page, caret)) = self.annotation_context() else { return };
        if let Some(text) = annotation_dialogs::add_note(self.wnd(), page + 1) {
            report(AnnotationManager::global().add_note(&hash, page, caret, &text), "save the note");
        }
    }

    fn save_quote(&self) {
        let Some((v, hash, page, _)) = self.annotation_context() else { return };
        let selected = v.selected_text();
        if selected.is_empty() {
            return announce("No text selected");
        }
        let (start, end) = v.selection();
        if let Some(tags) = annotation_dialogs::save_quote(self.wnd(), &selected, page + 1) {
            report(AnnotationManager::global().save_quote(&hash, page, start, end, &selected, &tags), "save the quote");
        }
    }

    fn list_bookmarks(&self) {
        let Some((v, hash, ..)) = self.annotation_context() else { return };
        let mgr = AnnotationManager::global();
        let Some(items) = report(mgr.get_bookmarks(&hash), "read the bookmarks") else { return };
        let r = annotation_dialogs::list_bookmarks(self.wnd(), items);
        for id in r.deleted {
            report(mgr.remove_bookmark(id), "delete a bookmark");
        }
        if let Some(p) = r.go_to_page {
            v.go_to_page(p);
        }
    }

    fn list_notes(&self) {
        let Some((v, hash, ..)) = self.annotation_context() else { return };
        let mgr = AnnotationManager::global();
        let Some(items) = report(mgr.get_notes(&hash), "read the notes") else { return };
        let r = annotation_dialogs::list_notes(self.wnd(), items);
        for (id, text) in &r.edits {
            report(mgr.update_note(*id, text), "update a note");
        }
        for id in r.deleted {
            report(mgr.remove_note(id), "delete a note");
        }
        if let Some(p) = r.go_to_page {
            v.go_to_page(p);
        }
    }

    fn list_quotes(&self) {
        let Some((v, hash, ..)) = self.annotation_context() else { return };
        let mgr = AnnotationManager::global();
        let Some(items) = report(mgr.get_quotes(&hash), "read the quotes") else { return };
        let r = annotation_dialogs::list_quotes(self.wnd(), items);
        for id in r.deleted {
            report(mgr.remove_quote(id), "delete a quote");
        }
        if let Some(p) = r.go_to_page {
            v.go_to_page(p);
        }
    }

    fn export_annotations(&self) {
        let Some((v, hash, ..)) = self.annotation_context() else { return };
        let title = v.with_doc(|d| d.metadata().title);
        let title = if title.is_empty() { v.filename.clone() } else { title };
        let mgr = AnnotationManager::global();
        let html = config::get().annotations.default_export_format == "html";
        let output = if html { mgr.export_to_html(&hash, &title) } else { mgr.export_to_text(&hash, &title) };
        let Some(output) = report(output, "export the annotations") else { return };
        let (filter, name) = if html { (("HTML files (*.html)", "*.html"), "annotations.html") } else { (("Text files (*.txt)", "*.txt"), "annotations.txt") };
        let filters = [(filter.0.to_string(), filter.1.to_string())];
        let Some(path) = crate::gui::dialog::pick_save_file(self.hwnd(), "Export Annotations", &filters, name) else { return };
        match std::fs::write(&path, output) {
            Ok(()) => announce(format!("Annotations exported to {path}")),
            Err(e) => show_error(self.wnd(), "Export Error", "The annotations could not be written.", Some(&e.to_string()), &[]),
        }
    }

    // Misc

    fn on_export(&self) {
        let Some(v) = self.active_view() else { return announce("No document open") };
        let snapshot = v.with_doc(SnapshotDocument::of);
        export_dialog::show(self.wnd(), snapshot, &v.filename);
    }

    fn on_clear_cache(&self) {
        let stats = cache::cache_stats();
        let text = format!(
            "Clear all cached document data?\n\nCache size: {:.1} MB, {} entries.",
            stats.size_bytes as f64 / (1024.0 * 1024.0),
            stats.entry_count
        );
        if self.ask(&text, "Clear Cache", true) {
            cache::clear_cache();
            announce("Cache cleared");
        }
    }

    fn on_user_guide(&self) {
        let guide = resource_path("user_guide.md");
        if guide.is_file() {
            self.open_file(&guide);
        } else {
            show_error(
                self.wnd(),
                "User Guide Not Found",
                "The user guide file could not be located.",
                None,
                &["Reinstall the application to restore missing files."],
            );
        }
    }

    fn on_about(&self) {
        let engines: Vec<&str> = crate::tts::engines::available_engines().iter().map(|e| e.name).collect();
        let text = format!(
            "{APP_NAME} v{APP_VERSION}\n\nAccessibility-first document reader.\n\nDocument formats: {}\nTTS engines: {}",
            registry::available_extensions().len(),
            if engines.is_empty() { "None".to_string() } else { engines.join(", ") }
        );
        let _ = self.hwnd().MessageBox(&text, &format!("About {APP_NAME}"), co::MB::OK | co::MB::ICONINFORMATION);
    }

    /// Yes/No question; `default_no` makes No the default button. A task
    /// dialog, unlike a Yes/No message box, closes on Escape (as No).
    fn ask(&self, text: &str, caption: &str, default_no: bool) -> bool {
        let answer = w::TaskDialogIndirect(&w::TASKDIALOGCONFIG {
            hwnd_parent: Some(self.hwnd()),
            flags: co::TDF::ALLOW_DIALOG_CANCELLATION,
            common_buttons: co::TDCBF::YES | co::TDCBF::NO,
            window_title: Some(caption),
            content: Some(text),
            default_button_id: if default_no { co::DLGID::NO.raw() } else { co::DLGID::YES.raw() },
            ..Default::default()
        });
        matches!(answer, Ok((co::DLGID::YES, ..)))
    }

    // Geometry

    fn restore_geometry(&self) {
        let cfg = config::get().window;
        if cfg.width <= 0 || cfg.height <= 0 || (cfg.x == -1 && cfg.y == -1) {
            return;
        }
        // Only where a monitor still shows it.
        let visible = w::HMONITOR::MonitorFromPoint(w::POINT::with(cfg.x + 50, cfg.y + 50), co::MONITOR::DEFAULTTONULL) != w::HMONITOR::NULL;
        if visible {
            let _ = self.hwnd().SetWindowPos(w::HwndPlace::None, w::POINT::with(cfg.x, cfg.y), w::SIZE::with(cfg.width, cfg.height), co::SWP::NOZORDER);
        }
    }

    fn save_geometry(&self) {
        let maximized = self.hwnd().IsZoomed();
        let rect = self.hwnd().GetWindowRect().ok();
        let sidebar = self.0.sidebar.get().then(|| self.0.sidebar_width.get());
        config::update("window", |s| {
            s.window.maximized = maximized;
            if let (false, Some(r)) = (maximized, rect) {
                s.window.x = r.left;
                s.window.y = r.top;
                s.window.width = r.right - r.left;
                s.window.height = r.bottom - r.top;
            }
            if let Some(sw) = sidebar {
                s.window.sidebar_width = sw;
            }
        });
    }

    fn on_close(&self) {
        self.save_geometry();
        self.0.reader.stop();
        for v in self.0.views.borrow_mut().drain(..) {
            v.close();
        }
        let _ = self.hwnd().DestroyWindow();
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Logs and announces an annotation failure; the value on success.
fn report<T>(result: anyhow::Result<T>, what: &str) -> Option<T> {
    result
        .map_err(|e| {
            log::error!("could not {what}: {e:#}");
            announce(format!("Could not {what}."));
        })
        .ok()
}

/// Single-file open dialog starting in `folder`.
fn pick_open_file(owner: &w::HWND, filters: &[(String, String)], folder: &Path) -> Option<String> {
    let run = || -> w::HrResult<Option<String>> {
        let _com = w::CoInitializeEx(co::COINIT::APARTMENTTHREADED)?;
        let fd = w::CoCreateInstance::<w::IFileOpenDialog>(&co::CLSID::FileOpenDialog, None::<&w::IUnknown>, co::CLSCTX::INPROC_SERVER)?;
        fd.SetOptions(fd.GetOptions()? | co::FOS::FILEMUSTEXIST | co::FOS::FORCEFILESYSTEM)?;
        fd.SetTitle("Open Document")?;
        fd.SetFileTypes(filters)?;
        if let Ok(item) = w::SHCreateItemFromParsingName::<w::IShellItem>(&folder.to_string_lossy(), None::<&w::IBindCtx>) {
            fd.SetFolder(&item)?;
        }
        if !fd.Show(owner)? {
            return Ok(None);
        }
        Ok(Some(fd.GetResult()?.GetDisplayName(co::SIGDN::FILESYSPATH)?))
    };
    run().unwrap_or_else(|e| {
        log::error!("open dialog failed: {e}");
        None
    })
}

fn prompt_password(parent: &impl GuiParent, filename: &str) -> Option<String> {
    let d = DialogBuilder::new("Password Required", 380);
    d.column().text(&format!("The file '{filename}' is password-protected.\nPlease enter the password:"), 2);
    let pw = d.edit_with("&Password:", "", co::ES::PASSWORD);
    d.ok_cancel("OK");
    text_on_ok(&d, pw, parent)
}

/// The edit's text when the dialog closes through OK. Read in the OK
/// handler: once `run` returns, the control is destroyed.
fn text_on_ok(d: &DialogBuilder, edit: gui::Edit, parent: &impl GuiParent) -> Option<String> {
    let text = Rc::new(RefCell::new(None));
    let t = text.clone();
    d.on_ok(move || {
        *t.borrow_mut() = Some(edit.text().unwrap_or_default());
        true
    });
    d.run(parent);
    text.take()
}

/// Prism's "Select Document" choice for an archive holding several documents.
fn choose_member(parent: &impl GuiParent, docs: &[String]) -> Option<String> {
    let d = DialogBuilder::new("Select Document", 420);
    d.column().text(&format!("This archive contains {} documents.\nSelect one to open:", docs.len()), 2);
    let items: Vec<&str> = docs.iter().map(String::as_str).collect();
    let list = d.list_box("&Documents:", &items, 200);
    d.ok_cancel("OK");
    let l = list.clone();
    d.wnd.on().wm_show_window(move |p| {
        if p.being_shown {
            send(l.hwnd(), LB_SETCURSEL, 0, 0);
        }
        Ok(())
    });
    let chosen = Rc::new(Cell::new(None));
    let (c, l) = (chosen.clone(), list.clone());
    d.on_ok(move || {
        c.set(u32::try_from(send(l.hwnd(), LB_GETCURSEL, 0, 0)).ok());
        true
    });
    d.run(parent);
    chosen.get().and_then(|i: u32| docs.get(i as usize).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_text_matches_menu_labels() {
        assert_eq!(shortcut_text(ctrl_shift(b'S' as u16)), "Ctrl+Shift+S");
        assert_eq!(shortcut_text(shift(f(5))), "Shift+F5");
        assert_eq!(shortcut_text(ctrl(VK_OEM_COMMA)), "Ctrl+,");
        assert_eq!(shortcut_text(ctrl_shift(f(2))), "Ctrl+Shift+F2");
        assert_eq!(menu_text(Cmd::Shortcuts, "&Keyboard Shortcuts"), "&Keyboard Shortcuts\tCtrl+/");
        assert_eq!(menu_text(Cmd::Exit, "E&xit\tAlt+F4"), "E&xit\tAlt+F4");
    }

    #[test]
    fn accelerator_table_carries_modifiers_and_commands() {
        let table = accelerators(&[(Cmd::Export, ctrl_shift(b'S' as u16)), (Cmd::SkipBackward, alt(VK_LEFT))]);
        assert_eq!(table[0].fVirt, co::ACCELF::VIRTKEY | co::ACCELF::CONTROL | co::ACCELF::SHIFT);
        assert_eq!(table[0].key, unsafe { co::VK::from_raw(b'S' as u16) });
        assert_eq!(table[0].cmd, Cmd::Export as u16);
        assert_eq!(table[1].fVirt, co::ACCELF::VIRTKEY | co::ACCELF::ALT);
        assert_eq!(table[1].cmd, Cmd::SkipBackward as u16);
    }

    #[test]
    fn shortcuts_are_unique_and_command_ids_are_free() {
        for (i, (_, a)) in SHORTCUTS.iter().enumerate() {
            assert!(SHORTCUTS[i + 1..].iter().all(|(_, b)| b != a), "duplicate shortcut {}", shortcut_text(*a));
        }
        let ids: Vec<u16> = ALL_CMDS.iter().map(|c| *c as u16).collect();
        assert!(ids.iter().all(|&id| id != IDOK && id != IDCANCEL && !(RECENT_BASE..RECENT_BASE + RECENT_MAX).contains(&id)));
    }
}
