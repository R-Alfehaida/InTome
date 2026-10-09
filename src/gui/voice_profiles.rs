//! Voice Profiles dialog (port of prism/tts/gui.py): pick, add, edit and
//! delete voice profiles, set the active one, and preview the chosen voice.
//! OK saves the controls into the selected profile, makes it active, and
//! writes the global TTS settings.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use winsafe::{self as w, co, gui, prelude::*};

use crate::cancel::CancelToken;
use crate::config;
use crate::events;
use crate::gui::dialog::{self, DialogBuilder};
use crate::tts::engine::TtsEngine;
use crate::tts::engines::{EngineInfo, available_engines, create_engine};
use crate::tts::profiles::{ProfileManager, VoiceProfile};

const PREVIEW_TEXT: &str = "This is a preview of the selected voice.";
const PREVIEW_TIMER: usize = 1;
const PARAGRAPH_MAX: u32 = 5000;
const PAGE_MAX: u32 = 7000;
const PREVIEW_LABEL: &str = "Previe&w";
const STOP_LABEL: &str = "Stop Previe&w";

pub fn show(parent: &impl GuiParent) {
    // Engines are COM objects. Make this thread an STA before an engine picks
    // MTA for it, which would break the shell's file dialogs later.
    let _com = w::CoInitializeEx(co::COINIT::APARTMENTTHREADED);
    let manager = ProfileManager::new();
    let engines = available_engines();
    let profiles = manager.get_profiles();
    let active = manager.get_active_profile();
    let tts = config::get().tts;

    let d = DialogBuilder::new("Voice Profiles", 450);
    let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
    let profile = d.combo("&Profile:", &names, profiles.iter().position(|p| p.name == active.name).map(|i| i as u32));
    let engine_names: Vec<&str> = engines.iter().map(|e| e.name).collect();
    let engine_index = engine_index(&engines, &active.engine);
    let engine = d.combo("&Engine:", &engine_names, engine_index.map(|i| i as u32));
    let voices = engine_index.map(|i| voice_names(engines[i].key)).unwrap_or_default();
    let voice_items: Vec<&str> = voices.iter().map(String::as_str).collect();
    let voice = d.combo("&Voice:", &voice_items, voice_index(&voices, &active.voice));
    let rate = d.trackbar("&Rate:", 0, 100, active.rate.min(100));
    let volume = d.trackbar("V&olume:", 0, 100, active.volume.min(100));
    let col = d.column();
    let paragraph = col.spin("&Paragraph pause (ms):", active.paragraph_pause_ms.min(PARAGRAPH_MAX as u64) as u32, 0, PARAGRAPH_MAX);
    let page = col.spin("Pa&ge pause (ms):", active.page_pause_ms.min(PAGE_MAX as u64) as u32, 0, PAGE_MAX);
    let highlight = d.check("&Highlight spoken text", tts.highlight_spoken_text);
    let page_numbers = d.check("&Speak page numbers", tts.speak_page_numbers);
    let announce = d.check("A&nnounce section endings", tts.announce_sections);
    let mut buttons = col.buttons(&["&New Profile", "&Delete Profile", PREVIEW_LABEL], 120);
    d.ok_cancel("OK");

    let ui = Rc::new(VoiceUi {
        wnd: d.wnd.clone(),
        manager: RefCell::new(manager),
        engines,
        profile,
        engine,
        voice,
        rate,
        volume,
        paragraph,
        page,
        highlight,
        page_numbers,
        announce,
        preview_btn: buttons.pop().unwrap(),
        delete_btn: buttons.pop().unwrap(),
        preview: RefCell::new(None),
    });
    let new_btn = buttons.pop().unwrap();

    let u = ui.clone();
    d.wnd.on().wm_show_window(move |p| {
        if p.being_shown {
            u.delete_btn.hwnd().EnableWindow(!active.builtin);
        }
        Ok(())
    });
    let u = ui.clone();
    ui.profile.on().cbn_sel_change(move || {
        if let Some(p) = u.selected_name().and_then(|n| u.manager.borrow().get_profile(&n)) {
            u.apply_profile(&p);
        }
        Ok(())
    });
    let u = ui.clone();
    ui.engine.on().cbn_sel_change(move || {
        u.fill_voices("");
        Ok(())
    });
    let u = ui.clone();
    new_btn.on().bn_clicked(move || {
        u.new_profile();
        Ok(())
    });
    let u = ui.clone();
    ui.delete_btn.on().bn_clicked(move || {
        u.delete_profile();
        Ok(())
    });
    let u = ui.clone();
    ui.preview_btn.on().bn_clicked(move || {
        if u.preview.borrow().is_some() { u.stop_preview() } else { u.start_preview() }
        Ok(())
    });
    let u = ui.clone();
    d.wnd.on().wm_timer(PREVIEW_TIMER, move || {
        u.drain_preview();
        Ok(())
    });
    // Closing never waits for the preview thread: stop() returns at once and the thread ends on its own.
    let u = ui.clone();
    d.wnd.on().wm_destroy(move || {
        u.stop_preview();
        Ok(())
    });
    let u = ui.clone();
    d.on_ok(move || {
        u.save();
        true
    });
    d.run(parent);
}

/// A running preview: its engine (once created) for stop, and its result channel.
/// A new preview gets a new channel, so a stopped one can never report into it.
struct Preview {
    cancel: CancelToken,
    engine: Arc<Mutex<Option<Arc<dyn TtsEngine>>>>,
    done: Receiver<Result<(), String>>,
}

struct VoiceUi {
    wnd: gui::WindowModal,
    manager: RefCell<ProfileManager>,
    engines: Vec<EngineInfo>,
    profile: gui::ComboBox,
    engine: gui::ComboBox,
    voice: gui::ComboBox,
    rate: gui::Trackbar,
    volume: gui::Trackbar,
    paragraph: gui::Edit,
    page: gui::Edit,
    highlight: gui::CheckBox,
    page_numbers: gui::CheckBox,
    announce: gui::CheckBox,
    delete_btn: gui::Button,
    preview_btn: gui::Button,
    preview: RefCell<Option<Preview>>,
}

impl VoiceUi {
    fn selected_name(&self) -> Option<String> {
        self.profile.items().selected_text().ok().flatten()
    }

    fn engine_key(&self) -> &'static str {
        self.engine.items().selected_index().and_then(|i| self.engines.get(i as usize)).map_or("sapi", |e| e.key)
    }

    fn voice_name(&self) -> String {
        self.voice.items().selected_text().ok().flatten().unwrap_or_default()
    }

    /// Refills the voice list for the selected engine, selecting `preferred` if present.
    fn fill_voices(&self, preferred: &str) {
        let voices = voice_names(self.engine_key());
        let items = self.voice.items();
        items.delete_all();
        let _ = items.add(&voices);
        items.select(voice_index(&voices, preferred));
    }

    fn apply_profile(&self, p: &VoiceProfile) {
        self.engine.items().select(engine_index(&self.engines, &p.engine).map(|i| i as u32));
        self.fill_voices(&p.voice);
        self.rate.set_pos(p.rate.min(100));
        self.volume.set_pos(p.volume.min(100));
        let _ = self.paragraph.hwnd().SetWindowText(&p.paragraph_pause_ms.to_string());
        let _ = self.page.hwnd().SetWindowText(&p.page_pause_ms.to_string());
        let tts = config::get().tts;
        self.highlight.set_check(tts.highlight_spoken_text);
        self.page_numbers.set_check(tts.speak_page_numbers);
        self.announce.set_check(tts.announce_sections);
        self.delete_btn.hwnd().EnableWindow(!p.builtin);
    }

    fn profile_from_controls(&self, name: &str, builtin: bool) -> VoiceProfile {
        VoiceProfile {
            name: name.to_string(),
            engine: self.engine_key().to_string(),
            voice: self.voice_name(),
            rate: self.rate.pos(),
            volume: self.volume.pos(),
            paragraph_pause_ms: dialog::spin_value(&self.paragraph, 0, PARAGRAPH_MAX, 300) as u64,
            page_pause_ms: dialog::spin_value(&self.page, 0, PAGE_MAX, 500) as u64,
            builtin,
        }
    }

    fn fill_profiles(&self, select: &str) {
        let names: Vec<String> = self.manager.borrow().get_profiles().into_iter().map(|p| p.name).collect();
        let items = self.profile.items();
        items.delete_all();
        let _ = items.add(&names);
        items.select(names.iter().position(|n| n == select).map(|i| i as u32));
    }

    fn new_profile(&self) {
        let Some(name) = ask_profile_name(&self.wnd) else { return };
        if self.manager.borrow().get_profile(&name).is_some() {
            let _ = self.wnd.hwnd().MessageBox(&format!("A profile named '{name}' already exists."), "Duplicate Name", co::MB::OK | co::MB::ICONWARNING);
            return;
        }
        let profile = self.profile_from_controls(&name, false);
        self.manager.borrow_mut().add_profile(profile);
        self.fill_profiles(&name);
        self.delete_btn.hwnd().EnableWindow(true);
        events::announce(format!("Profile {name} added"));
    }

    fn delete_profile(&self) {
        let Some(name) = self.selected_name() else { return };
        if self.manager.borrow().get_profile(&name).is_none_or(|p| p.builtin) {
            let _ = self.wnd.hwnd().MessageBox("Built-in profiles cannot be deleted.", "Cannot Delete", co::MB::OK | co::MB::ICONINFORMATION);
            return;
        }
        if self.manager.borrow_mut().remove_profile(&name) {
            let active = self.manager.borrow().get_active_profile();
            self.fill_profiles(&active.name);
            self.apply_profile(&active);
            events::announce(format!("Profile {name} deleted"));
            // The focused Delete button may now be disabled; keep focus on something usable.
            self.profile.hwnd().SetFocus();
        }
    }

    fn save(&self) {
        if let Some(name) = self.selected_name() {
            let builtin = self.manager.borrow().get_profile(&name).is_some_and(|p| p.builtin);
            let profile = self.profile_from_controls(&name, builtin);
            let mut manager = self.manager.borrow_mut();
            manager.update_profile(profile);
            manager.set_active_profile(&name);
        }
        let draft = self.profile_from_controls("", false);
        let (highlight, page_numbers, announce) = (self.highlight.is_checked(), self.page_numbers.is_checked(), self.announce.is_checked());
        config::update("tts", |s| {
            s.tts.highlight_spoken_text = highlight;
            s.tts.speak_page_numbers = page_numbers;
            s.tts.announce_sections = announce;
            s.tts.engine = draft.engine;
            s.tts.voice = draft.voice;
            s.tts.rate = draft.rate;
            s.tts.volume = draft.volume;
            s.tts.paragraph_pause_ms = draft.paragraph_pause_ms;
            s.tts.page_pause_ms = draft.page_pause_ms;
        });
        events::announce("Voice settings saved");
    }

    fn start_preview(&self) {
        let draft = self.profile_from_controls("", false);
        let (tx, done) = mpsc::channel();
        let preview = Preview { cancel: CancelToken::new(), engine: Arc::new(Mutex::new(None)), done };
        let (cancel, slot) = (preview.cancel.clone(), preview.engine.clone());
        std::thread::spawn(move || {
            let run = || -> anyhow::Result<()> {
                let engine = create_engine(&draft.engine)?;
                {
                    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
                    if cancel.is_cancelled() {
                        return Ok(());
                    }
                    *slot = Some(engine.clone());
                }
                if !draft.voice.is_empty() {
                    engine.set_voice(&draft.voice);
                }
                engine.set_rate(draft.rate);
                engine.set_volume(draft.volume);
                engine.speak(PREVIEW_TEXT)
            };
            let _ = tx.send(run().map_err(|e| format!("{e:#}")));
        });
        *self.preview.borrow_mut() = Some(preview);
        let _ = self.preview_btn.hwnd().SetWindowText(STOP_LABEL);
        if let Err(e) = self.wnd.hwnd().SetTimer(PREVIEW_TIMER, 100, None) {
            log::error!("voice preview timer failed: {e}");
        }
    }

    /// Stops a running preview without waiting for its thread.
    fn stop_preview(&self) {
        let Some(preview) = self.preview.borrow_mut().take() else { return };
        preview.cancel.cancel();
        if let Some(engine) = preview.engine.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            // A SAPI purge can block for a few hundred milliseconds; keep it off the UI thread.
            std::thread::spawn(move || engine.stop());
        }
        self.preview_finished();
    }

    fn drain_preview(&self) {
        let result = match self.preview.borrow().as_ref().map(|p| p.done.try_recv()) {
            Some(Ok(result)) => result,
            _ => return,
        };
        self.preview.borrow_mut().take();
        self.preview_finished();
        if let Err(e) = result {
            events::announce(format!("Voice preview failed: {e}"));
            let _ = self.wnd.hwnd().MessageBox(&format!("Could not preview the voice:\n\n{e}"), "Preview Failed", co::MB::OK | co::MB::ICONERROR);
        }
    }

    fn preview_finished(&self) {
        let _ = self.wnd.hwnd().KillTimer(PREVIEW_TIMER);
        let _ = self.preview_btn.hwnd().SetWindowText(PREVIEW_LABEL);
    }
}

/// Asks for a new profile's name; `None` when cancelled or blank.
fn ask_profile_name(parent: &impl GuiParent) -> Option<String> {
    let d = DialogBuilder::new("New Voice Profile", 360);
    d.text("Enter a name for the new profile:");
    let edit = d.edit("&Name:", "");
    d.ok_cancel("OK");
    let name = Rc::new(RefCell::new(String::new()));
    let n = name.clone();
    d.on_ok(move || {
        *n.borrow_mut() = edit.text().unwrap_or_default().trim().to_string();
        true
    });
    d.run(parent).then(|| name.take()).filter(|n| !n.is_empty())
}

fn engine_index(engines: &[EngineInfo], key: &str) -> Option<usize> {
    engines.iter().position(|e| e.key == key).or((!engines.is_empty()).then_some(0))
}

/// Voice names an engine offers; empty when it cannot start.
fn voice_names(key: &str) -> Vec<String> {
    match create_engine(key) {
        Ok(engine) => engine.voices().into_iter().map(|v| v.name).filter(|n| !n.is_empty()).collect(),
        Err(e) => {
            log::warn!("cannot list voices for {key}: {e:#}");
            Vec::new()
        }
    }
}

/// `preferred` if listed, else the first voice.
fn voice_index(voices: &[String], preferred: &str) -> Option<u32> {
    voices.iter().position(|v| v == preferred).or((!voices.is_empty()).then_some(0)).map(|i| i as u32)
}
