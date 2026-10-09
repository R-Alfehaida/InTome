//! Screen-reader output (port of prism/speech.py over accessible_output2's Auto).
//!
//! Backends in Auto's order of preference: NVDA (controller client DLL), JAWS
//! (COM `FreedomSci.JawsApi`), then a UI Automation notification event, which
//! Narrator and other UIA-aware readers honour. The first active backend wins.
//! No call ever panics or blocks on a missing screen reader.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, DISPATCH_METHOD, DISPPARAMS, IDispatch,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    NotificationKind_Other, NotificationProcessing_All, NotificationProcessing_ImportantMostRecent, UiaClientsAreListening,
    UiaHostProviderFromHwnd, UiaRaiseNotificationEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::{BSTR, GUID, PCWSTR, w};

use crate::events::{self, AppEvent};

/// Which backend handled a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Nvda,
    Jaws,
    Uia,
    /// No screen reader is listening; the text was dropped.
    None,
}

/// Subscribes to [`AppEvent::Announce`]. Call on the UI thread at startup.
pub fn init() -> events::SubscriptionId {
    events::subscribe(|e| {
        if let AppEvent::Announce { message, interrupt } = e {
            output(message, *interrupt);
        }
    })
}

/// Tells the UIA fallback which window raises notifications. Without it the
/// foreground window is used when it belongs to this process.
pub fn set_main_window(hwnd: isize) {
    MAIN_HWND.store(hwnd, Ordering::Relaxed);
}

/// Sends `text` to the active screen reader's speech.
pub fn speak(text: &str, interrupt: bool) -> Backend {
    if text.is_empty() {
        return Backend::None;
    }
    if let Some(nvda) = nvda_running() {
        if interrupt {
            unsafe { (nvda.cancel_speech)() };
        }
        unsafe { (nvda.speak_text)(wide(text).as_ptr()) };
        return Backend::Nvda;
    }
    if jaws_running() && jaws_call(Jaws::say, text, interrupt) {
        return Backend::Jaws;
    }
    if uia_notify(text, interrupt) { Backend::Uia } else { Backend::None }
}

/// Sends `text` to a connected braille display (NVDA and JAWS only; UIA has no braille channel).
pub fn braille(text: &str) -> Backend {
    if text.is_empty() {
        return Backend::None;
    }
    if let Some(nvda) = nvda_running() {
        unsafe { (nvda.braille_message)(wide(text).as_ptr()) };
        return Backend::Nvda;
    }
    if jaws_running() && jaws_call(Jaws::braille, text, false) {
        return Backend::Jaws;
    }
    Backend::None
}

/// Speaks and brailles `text`.
pub fn output(text: &str, interrupt: bool) -> Backend {
    let spoken = speak(text, interrupt);
    braille(text);
    spoken
}

/// `nvdaController_testIfRunning`: `Some(0)` means NVDA is running, any other
/// value is an RPC error code; `None` means the controller DLL could not be loaded.
pub fn nvda_status() -> Option<u32> {
    nvda().map(|n| unsafe { (n.test_if_running)() })
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------
// NVDA
// ---------------------------------------------------------------------------

struct Nvda {
    test_if_running: unsafe extern "system" fn() -> u32,
    speak_text: unsafe extern "system" fn(*const u16) -> u32,
    cancel_speech: unsafe extern "system" fn() -> u32,
    braille_message: unsafe extern "system" fn(*const u16) -> u32,
}

fn nvda() -> Option<&'static Nvda> {
    static NVDA: OnceLock<Option<Nvda>> = OnceLock::new();
    NVDA.get_or_init(load_nvda).as_ref()
}

fn nvda_running() -> Option<&'static Nvda> {
    nvda().filter(|n| unsafe { (n.test_if_running)() } == 0)
}

fn load_nvda() -> Option<Nvda> {
    const DLL: &str = "nvdaControllerClient64.dll";
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    // Next to the exe (installed, cargo run); the parent covers cargo's examples/ and deps/ dirs.
    let candidates = [exe_dir.join(DLL), exe_dir.join("..").join(DLL)];
    let path = candidates.iter().find(|p| p.is_file())?;
    unsafe {
        let wpath = wide(&path.to_string_lossy());
        let lib = match LoadLibraryW(PCWSTR(wpath.as_ptr())) {
            Ok(lib) => lib,
            Err(e) => {
                log::warn!("cannot load {}: {e}", path.display());
                return None;
            }
        };
        macro_rules! sym {
            ($name:expr) => {
                match GetProcAddress(lib, windows::core::s!($name)) {
                    Some(f) => std::mem::transmute(f),
                    None => {
                        log::warn!("{} missing from {DLL}", $name);
                        return None;
                    }
                }
            };
        }
        Some(Nvda {
            test_if_running: sym!("nvdaController_testIfRunning"),
            speak_text: sym!("nvdaController_speakText"),
            cancel_speech: sym!("nvdaController_cancelSpeech"),
            braille_message: sym!("nvdaController_brailleMessage"),
        })
    }
}

// ---------------------------------------------------------------------------
// JAWS
// ---------------------------------------------------------------------------

struct Jaws {
    disp: IDispatch,
    say_string: i32,
    run_function: i32,
}

thread_local! {
    // COM proxies are apartment-bound, so the object lives per calling thread (the UI thread in practice).
    static JAWS: RefCell<Option<Jaws>> = const { RefCell::new(None) };
}

/// JAWS's hidden UI window; checking it avoids launching the JAWS API server when JAWS is not running.
fn jaws_running() -> bool {
    unsafe { FindWindowW(w!("JFWUI2"), w!("JAWS")) }.is_ok_and(|h| !h.is_invalid())
}

impl Jaws {
    fn connect() -> windows::core::Result<Self> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let clsid = windows::Win32::System::Com::CLSIDFromProgID(w!("FreedomSci.JawsApi"))?;
            let disp: IDispatch = CoCreateInstance(&clsid, None, CLSCTX_LOCAL_SERVER)?;
            let id = |name: PCWSTR| -> windows::core::Result<i32> {
                let mut id = 0;
                disp.GetIDsOfNames(&GUID::zeroed(), &name, 1, 0x0400, &mut id)?;
                Ok(id)
            };
            Ok(Self { say_string: id(w!("SayString"))?, run_function: id(w!("RunFunction"))?, disp })
        }
    }

    fn invoke(&self, dispid: i32, mut args: Vec<VARIANT>) -> windows::core::Result<()> {
        args.reverse(); // DISPPARAMS wants right-to-left
        let params = DISPPARAMS { rgvarg: args.as_mut_ptr(), rgdispidNamedArgs: std::ptr::null_mut(), cArgs: args.len() as u32, cNamedArgs: 0 };
        let mut result = VARIANT::default();
        unsafe { self.disp.Invoke(dispid, &GUID::zeroed(), 0x0400, DISPATCH_METHOD, &params, Some(&mut result), None, None) }
    }

    fn say(&self, text: &str, interrupt: bool) -> windows::core::Result<()> {
        // The leading spaces keep JAWS from clipping the first syllable, as accessible_output2 does.
        self.invoke(self.say_string, vec![format!("      {text}").as_str().into(), interrupt.into()])
    }

    fn braille(&self, text: &str, _: bool) -> windows::core::Result<()> {
        // JAWS does not understand escaped double quotes in the function string.
        let text = text.replace('"', "'");
        self.invoke(self.run_function, vec![format!("BrailleString(\"{text}\")").as_str().into()])
    }
}

/// Runs `f` against the cached JAWS object, connecting on first use. A failure drops the object so the next call reconnects.
fn jaws_call(f: fn(&Jaws, &str, bool) -> windows::core::Result<()>, text: &str, flag: bool) -> bool {
    JAWS.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            match Jaws::connect() {
                Ok(j) => *slot = Some(j),
                Err(e) => {
                    log::debug!("JAWS API unavailable: {e}");
                    return false;
                }
            }
        }
        match f(slot.as_ref().unwrap(), text, flag) {
            Ok(()) => true,
            Err(e) => {
                log::debug!("JAWS call failed: {e}");
                *slot = None;
                false
            }
        }
    })
}

// ---------------------------------------------------------------------------
// UI Automation notification fallback
// ---------------------------------------------------------------------------

static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

fn target_window() -> Option<HWND> {
    let main = MAIN_HWND.load(Ordering::Relaxed);
    if main != 0 {
        return Some(HWND(main as _));
    }
    let fg = unsafe { GetForegroundWindow() };
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
    (!fg.is_invalid() && pid == unsafe { GetCurrentProcessId() }).then_some(fg)
}

/// Raises a UIA notification event on the window's host provider. Returns false when nobody listens.
pub fn uia_notify(text: &str, interrupt: bool) -> bool {
    if !unsafe { UiaClientsAreListening() }.as_bool() {
        return false;
    }
    let Some(hwnd) = target_window() else { return false };
    let processing = if interrupt { NotificationProcessing_ImportantMostRecent } else { NotificationProcessing_All };
    let result = unsafe {
        UiaHostProviderFromHwnd(hwnd).and_then(|p| UiaRaiseNotificationEvent(&p, NotificationKind_Other, processing, &BSTR::from(text), &BSTR::from("InTome.Announce")))
    };
    if let Err(e) = &result {
        log::debug!("UIA notification failed: {e}");
    }
    result.is_ok()
}
