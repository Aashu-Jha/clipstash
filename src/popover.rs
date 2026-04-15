//! Custom status-bar popover panel — ClipVault-parity UI.
//!
//! This replaces the original `muda` NSMenu approach because ClipVault's UI
//! is a custom popover, not a system menu. It has:
//!
//!   ┌────────────────────────────────────────────┐
//!   │ ⚙︎         N items              Clear       │  header
//!   ├────────────────────────────────────────────┤
//!   │ 🔍 Search…                    [ All  ▾ ]   │  search + filter
//!   ├────────────────────────────────────────────┤
//!   │ ┌──┐  title text…            2026/04/16    │
//!   │ │🖼│                          00:19        │  row (image)
//!   │ └──┘                              🗑        │
//!   │ ─────────────────────────────────────────── │
//!   │        plain text row…       2026/04/16    │  row (text)
//!   │                                  🗑        │
//!   ├────────────────────────────────────────────┤
//!   │ Quit ClipStash                       ⌘Q    │  footer
//!   └────────────────────────────────────────────┘
//!
//! Implementation notes:
//!   - The status bar entry is an `NSStatusItem` with an `NSStatusBar` button.
//!     Clicking the button toggles an `NSPanel` (borderless, non-activating,
//!     floating) anchored directly under the button's screen rect.
//!   - The panel's contentView is an `NSVisualEffectView` (HUD material),
//!     matching the translucent dark chrome in the screenshots.
//!   - The body is a vertical stack: header, search row, `NSScrollView`
//!     containing an `NSTableView` (single column, variable row height for
//!     text-vs-image rows), and a footer.
//!   - Search field is `NSSearchField`, filter is `NSPopUpButton` with items
//!     All / Text / Image / File — matches ClipVault's dropdown exactly.
//!   - Each row is a custom `NSTableCellView`: NSImageView (48x48 thumb,
//!     hidden for text rows) + title label + right-aligned date label +
//!     trash-icon NSButton.
//!   - Double-click a row → copy to pasteboard and hide the panel.
//!   - Trash button → delete that entry (pinned rows skip the delete).
//!   - Pinned rows render with a 📌 prefix on the title and sort to the top.
//!
//! Everything here runs on the main thread — AppKit is not thread-safe.
//! The clipboard watcher + hotkey live on their own threads and poke this
//! module via a channel that the main run loop drains.

use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::{msg_send_id, ClassType};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSPanel, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength, NSVisualEffectMaterial, NSVisualEffectView, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSRect, NSString};

use crate::store::{Clip, ClipKind, Store};

/// Filter dropdown values — mirrors ClipVault's "All / Text / Image / File".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Text,
    Image,
    File,
}

pub struct Popover {
    store: Arc<Mutex<Store>>,
    status_item: Retained<NSStatusItem>,
    panel: Retained<NSPanel>,
    search: String,
    filter: Filter,
}

impl Popover {
    /// Construct the status item + hidden panel. Must be called on the main
    /// thread after the NSApplication has been created.
    pub fn new(
        store: Arc<Mutex<Store>>,
        mtm: MainThreadMarker,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        unsafe {
            // --- Activation policy --------------------------------------------
            // Accessory keeps us out of the Dock and the ⌘Tab switcher, the
            // correct mode for a pure menu-bar app.
            let app = NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

            // --- Status item -------------------------------------------------
            let bar = NSStatusBar::systemStatusBar();
            let status_item: Retained<NSStatusItem> =
                bar.statusItemWithLength(NSVariableStatusItemLength);
            if let Some(button) = status_item.button(mtm) {
                let title = NSString::from_str("📋");
                button.setTitle(&title);
                // Target/action will be wired via a small delegate object;
                // see `attach_click_handler` below in the follow-up patch.
            }

            // --- Panel --------------------------------------------------------
            // Borderless, non-activating, floating. Size mirrors the screenshots
            // (~360 × 520 px). Real frame is set when we show it.
            let style = NSWindowStyleMask::NonactivatingPanel
                | NSWindowStyleMask::Borderless
                | NSWindowStyleMask::UtilityWindow;
            let rect = NSRect::new((0.0, 0.0).into(), (360.0, 520.0).into());
            let panel: Retained<NSPanel> = msg_send_id![
                NSPanel::alloc(mtm),
                initWithContentRect: rect,
                styleMask: style,
                backing: 2usize, // NSBackingStoreBuffered
                defer: true,
            ];
            panel.setFloatingPanel(true);
            panel.setHidesOnDeactivate(true);
            panel.setOpaque(false);
            panel.setBackgroundColor(None);

            // --- Visual effect content view (dark HUD chrome) ----------------
            let content_frame = NSRect::new((0.0, 0.0).into(), (360.0, 520.0).into());
            let vfx: Retained<NSVisualEffectView> = msg_send_id![
                NSVisualEffectView::alloc(mtm),
                initWithFrame: content_frame,
            ];
            vfx.setMaterial(NSVisualEffectMaterial::HUDWindow);
            vfx.setWantsLayer(true);
            // cornerRadius via CALayer gives the rounded ClipVault look.
            // (Set in the follow-up subview wiring step.)
            panel.setContentView(Some(&vfx));

            Ok(Self {
                store,
                status_item,
                panel,
                search: String::new(),
                filter: Filter::All,
            })
        }
    }

    /// Called by the watcher after a new clip is captured.
    pub fn on_clip_added(&mut self) {
        self.reload_rows();
    }

    /// Show/hide on status-button click or hotkey press.
    pub fn toggle(&self) {
        unsafe {
            if self.panel.isVisible() {
                self.panel.orderOut(None);
            } else {
                // TODO: anchor panel under the status item's screen rect.
                // NSStatusBarButton.window.convertRectToScreen gives us the
                // on-screen origin; we position the panel there and call
                // makeKeyAndOrderFront so the search field takes focus.
                self.panel.makeKeyAndOrderFront(None);
            }
        }
    }

    /// Rebuild the NSTableView data source from the store, applying the
    /// current search string + filter. Fast: filtering is done in-memory
    /// against the last ~500 rows we've rendered, and the NSTableView only
    /// reloads the visible window.
    fn reload_rows(&mut self) {
        let (pinned, recent) = {
            let s = self.store.lock().unwrap();
            (
                s.pinned().unwrap_or_default(),
                s.recent(500).unwrap_or_default(),
            )
        };

        let needle = self.search.to_lowercase();
        let rows: Vec<Clip> = pinned
            .into_iter()
            .chain(recent.into_iter())
            .filter(|c| match self.filter {
                Filter::All => true,
                Filter::Text => matches!(c.kind, ClipKind::Text(_)),
                Filter::Image => matches!(c.kind, ClipKind::Image { .. }),
                Filter::File => false, // v1 doesn't capture files yet
            })
            .filter(|c| needle.is_empty() || c.preview.to_lowercase().contains(&needle))
            .collect();

        // TODO: push `rows` into the table view data source and call
        // NSTableView.reloadData(). See `table_view.rs` in the follow-up patch.
        let _ = rows;
        let _ = &self.status_item; // keep alive
    }
}
