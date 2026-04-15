//! Global ⌘⇧V hotkey registration.
//!
//! Uses the `global-hotkey` crate, which wraps Carbon's RegisterEventHotKey
//! under the hood. Zero cost when the key isn't pressed.

use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager,
};
use tao::event_loop::EventLoopProxy;

use crate::AppEvent;

pub fn register_default(
    proxy: EventLoopProxy<AppEvent>,
) -> Result<GlobalHotKeyManager, Box<dyn std::error::Error>> {
    let manager = GlobalHotKeyManager::new()?;
    let hotkey = HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyV);
    manager.register(hotkey)?;

    // Forward hotkey events onto the main tao loop.
    std::thread::Builder::new()
        .name("clipstash-hotkey".into())
        .spawn(move || {
            let rx = GlobalHotKeyEvent::receiver();
            while let Ok(_ev) = rx.recv() {
                let _ = proxy.send_event(AppEvent::HotkeyPressed);
            }
        })?;

    Ok(manager)
}
