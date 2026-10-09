//! Accessibility plumbing for native Win32 controls.
//!
//! [`set_name`] is the port of wx `SetName` intent: it attaches an accessible
//! name via Dynamic Annotation (`IAccPropServices`), which MSAA and the UIA
//! MSAA proxy both honour, so NVDA, JAWS and Narrator read it. Controls with a
//! caption (buttons, check boxes) or a preceding static label already have a
//! name; use this for trees, lists, rich edits and other unlabelled controls.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::UI::Accessibility::{CLSID_AccPropServices, IAccPropServices, NotifyWinEvent, PROPID_ACC_DESCRIPTION, PROPID_ACC_NAME};
use windows::core::{GUID, HSTRING};

const OBJID_CLIENT: u32 = 0xFFFF_FFFC;
const CHILDID_SELF: u32 = 0;
const EVENT_OBJECT_VALUECHANGE: u32 = 0x800E;
const EVENT_OBJECT_NAMECHANGE: u32 = 0x800C;

fn set_prop(hwnd: isize, prop: GUID, value: &str) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        match CoCreateInstance::<_, IAccPropServices>(&CLSID_AccPropServices, None, CLSCTX_INPROC_SERVER) {
            Ok(svc) => {
                if let Err(e) = svc.SetHwndPropStr(HWND(hwnd as _), OBJID_CLIENT, CHILDID_SELF, prop, &HSTRING::from(value)) {
                    log::warn!("SetHwndPropStr failed: {e}");
                }
            }
            Err(e) => log::warn!("AccPropServices unavailable: {e}"),
        }
    }
}

/// Sets the accessible name screen readers announce for `hwnd`.
pub fn set_name(hwnd: isize, name: &str) {
    set_prop(hwnd, PROPID_ACC_NAME, name);
    unsafe { NotifyWinEvent(EVENT_OBJECT_NAMECHANGE, HWND(hwnd as _), OBJID_CLIENT as i32, CHILDID_SELF as i32) };
}

/// Sets the accessible description (read after name and role).
pub fn set_description(hwnd: isize, description: &str) {
    set_prop(hwnd, PROPID_ACC_DESCRIPTION, description);
}

/// Tells screen readers a control's value changed. Call after replacing the
/// text of a RichEdit; without it NVDA and JAWS report stale text.
pub fn notify_value_change(hwnd: isize) {
    unsafe { NotifyWinEvent(EVENT_OBJECT_VALUECHANGE, HWND(hwnd as _), OBJID_CLIENT as i32, CHILDID_SELF as i32) };
}
