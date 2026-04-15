//! Status-bar item + popover panel.
//!
//! Phase 1 (current): establishes the `NSStatusItem` with a button, shows a
//! simple `NSMenu` of recent clips so the binary is usable end-to-end.
//!
//! Phase 2 (in progress): replace the menu with a custom `NSPanel` matching
//! ClipVault's popover — header (gear / N items / Clear), search + filter,
//! `NSTableView` with thumbnail rows, footer, anchored under the status item
//! via `convertRectToScreen`. See the roadmap in README.md.

use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::sel;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{MainThreadMarker, NSString};

use crate::clipboard::write_to_pasteboard;
use crate::store::Store;

/// Max entries rendered in the menu. Full history still lives in the DB.
const VISIBLE_RECENT: usize = 50;

/// Filter dropdown values — mirrors ClipVault's "All / Text / Image / File".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Filter {
    All,
    Text,
    Image,
    File,
}

pub struct Popover {
    store: Arc<Mutex<Store>>,
    status_item: Retained<NSStatusItem>,
    mtm: MainThreadMarker,
    #[allow(dead_code)]
    search: String,
    #[allow(dead_code)]
    filter: Filter,
}

impl Popover {
    /// Construct the status item. Must be called on the main thread.
    pub fn new(
        store: Arc<Mutex<Store>>,
        mtm: MainThreadMarker,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Accessory activation policy: out of Dock, out of ⌘Tab.
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let bar = NSStatusBar::systemStatusBar();
        let status_item: Retained<NSStatusItem> =
            bar.statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = status_item.button(mtm) {
            let title = NSString::from_str("📋");
            button.setTitle(&title);
        }

        Ok(Self {
            store,
            status_item,
            mtm,
            search: String::new(),
            filter: Filter::All,
        })
    }

    /// Called by the watcher after a new clip is captured.
    pub fn on_clip_added(&mut self) {
        self.rebuild_menu();
    }

    /// Hotkey handler (Phase 2 will open the custom panel instead).
    pub fn toggle(&self) {
        if let Some(button) = self.status_item.button(self.mtm) {
            // Flash the status button so the user sees the hotkey registered.
            // In phase 2 this is replaced with panel anchoring via
            // `button.window().convertRectToScreen(button.bounds())`.
            unsafe { button.performClick(None) };
        }
    }

    /// Rebuild the NSMenu attached to the status item from current storage.
    fn rebuild_menu(&mut self) {
        let (pinned, recent, count) = {
            let s = self.store.lock().unwrap();
            (
                s.pinned().unwrap_or_default(),
                s.recent(VISIBLE_RECENT).unwrap_or_default(),
                s.count().unwrap_or(0),
            )
        };

        unsafe {
            let menu = NSMenu::new(self.mtm);

            if !pinned.is_empty() {
                append_header(&menu, self.mtm, "Pinned");
                for clip in pinned {
                    append_clip_item(&menu, self.mtm, &format!("📌  {}", clip.preview), clip.id);
                }
                menu.addItem(&NSMenuItem::separatorItem(self.mtm));
            }

            append_header(&menu, self.mtm, &format!("Recent ({count} total)"));
            for clip in recent {
                append_clip_item(&menu, self.mtm, &clip.preview, clip.id);
            }

            menu.addItem(&NSMenuItem::separatorItem(self.mtm));
            append_action_item(&menu, self.mtm, "Clear unpinned history", sel!(clipStashClear:));
            append_action_item(&menu, self.mtm, "Quit ClipStash", sel!(terminate:));

            self.status_item.setMenu(Some(&menu));
        }
    }
}

unsafe fn append_header(menu: &NSMenu, mtm: MainThreadMarker, title: &str) {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(title));
    item.setEnabled(false);
    menu.addItem(&item);
}

unsafe fn append_clip_item(menu: &NSMenu, mtm: MainThreadMarker, title: &str, _id: u64) {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(title));
    // TODO phase 2: wire target/action via a declare_class delegate that looks
    // up the clip id and writes it to the pasteboard.
    menu.addItem(&item);
}

unsafe fn append_action_item(menu: &NSMenu, mtm: MainThreadMarker, title: &str, sel: Sel) {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(title));
    item.setAction(Some(sel));
    menu.addItem(&item);
}

/// Silences "field never read" warnings until the delegate lands.
#[allow(dead_code)]
fn _keep_alive(_: &Store) {
    let _ = write_to_pasteboard;
}
