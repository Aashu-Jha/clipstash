//! ClipStash — a featherweight macOS menu bar clipboard manager.
//!
//! Wires four modules together on the main thread:
//!   clipboard  — NSPasteboard watcher (250ms changeCount poll)
//!   store      — redb-backed persistent history
//!   popover    — NSStatusItem + custom NSPanel UI
//!   hotkey     — ⌘⇧V global hotkey

mod clipboard;
mod hotkey;
mod popover;
mod store;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use objc2_foundation::MainThreadMarker;
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};

use crate::clipboard::ClipboardWatcher;
use crate::popover::Popover;
use crate::store::Store;

pub const ICON_COLOR_THRESHOLD: usize = 500;
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy)]
pub enum AppEvent {
    ClipboardChanged,
    HotkeyPressed,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let db_path = store::default_db_path()?;
    let store = Arc::new(Mutex::new(Store::open(&db_path)?));
    log::info!("opened store at {:?}", db_path);

    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();

    // Safe: tao runs its event loop on the main thread on macOS, and `main`
    // itself is the main thread.
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let mut popover = Popover::new(Arc::clone(&store), mtm)?;
    popover.on_clip_added();

    let proxy = event_loop.create_proxy();
    let watcher = ClipboardWatcher::spawn(Arc::clone(&store), proxy.clone(), POLL_INTERVAL);
    std::mem::forget(watcher);

    let _hotkey = hotkey::register_default(proxy.clone())?;

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::UserEvent(ev) = event {
            match ev {
                AppEvent::ClipboardChanged => popover.on_clip_added(),
                AppEvent::HotkeyPressed => popover.toggle(),
            }
        }
    });
}
