//! ClipStash — a featherweight macOS menu bar clipboard manager.
//!
//! Design goals (inspired by how lean Zed feels compared to ClipVault):
//!   - Idle CPU ~0% (no tight loops; 250ms NSPasteboard changeCount poll)
//!   - Idle RAM under ~15 MB
//!   - Pure-Rust embedded DB (redb) for unlimited persistent history
//!   - Native AppKit menu bar via objc2 / tray-icon — no web view
//!   - Global hotkey ⌘⇧V to open the menu
//!
//! Entry point wires together four modules:
//!   clipboard  — watches NSPasteboard for changes
//!   store      — redb-backed persistent history (text + images + pins)
//!   menu       — builds and refreshes the status bar menu
//!   hotkey     — registers ⌘⇧V
//!
//! See each module for implementation details.

mod clipboard;
mod hotkey;
mod popover;
mod store;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};

use crate::clipboard::ClipboardWatcher;
use crate::menu::MenuController;
use crate::store::Store;

/// Threshold at which the status bar icon changes color, per your spec.
pub const ICON_COLOR_THRESHOLD: usize = 500;

/// Poll interval for NSPasteboard.changeCount. 250ms is a sweet spot:
/// imperceptible to the user, negligible CPU (a single integer compare).
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    // --- Storage ------------------------------------------------------------
    // Persist to ~/Library/Application Support/ClipStash/history.redb
    let db_path = store::default_db_path()?;
    let store = Arc::new(Mutex::new(Store::open(&db_path)?));
    log::info!("opened store at {:?}", db_path);

    // --- Event loop ---------------------------------------------------------
    // tao gives us a cross-compatible NSApplication run loop that plays nice
    // with tray-icon, muda, and global-hotkey callbacks.
    let event_loop = EventLoopBuilder::new().build();

    // --- Menu bar -----------------------------------------------------------
    let mut menu_ctrl = MenuController::new(Arc::clone(&store))?;
    menu_ctrl.refresh()?; // initial render

    // --- Clipboard watcher --------------------------------------------------
    // Fires a user event on the tao loop whenever a new clipboard entry
    // is captured, so the menu can refresh without any polling on our side.
    let proxy = event_loop.create_proxy();
    let watcher = ClipboardWatcher::spawn(Arc::clone(&store), proxy.clone(), POLL_INTERVAL);
    std::mem::forget(watcher); // lives for the whole process

    // --- Global hotkey (⌘⇧V) ------------------------------------------------
    let _hotkey = hotkey::register_default(proxy.clone())?;

    // --- Run ----------------------------------------------------------------
    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::UserEvent(app_event) => match app_event {
                AppEvent::ClipboardChanged => {
                    if let Err(e) = menu_ctrl.refresh() {
                        log::warn!("menu refresh failed: {e}");
                    }
                }
                AppEvent::HotkeyPressed => {
                    menu_ctrl.show_menu();
                }
            },
            _ => {
                // Drain tray-icon + muda channels so menu clicks get handled.
                menu_ctrl.pump_events();
            }
        }
    });
}

/// Events dispatched from background threads back to the main loop.
#[derive(Debug, Clone, Copy)]
pub enum AppEvent {
    ClipboardChanged,
    HotkeyPressed,
}
