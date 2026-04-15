//! Status bar menu — pinned items on top, then recent history.
//!
//! Built with `tray-icon` + `muda`, which are both thin wrappers over
//! native NSStatusItem / NSMenu. No custom windowing, no web view,
//! so the memory footprint stays tiny.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::clipboard::write_to_pasteboard;
use crate::store::Store;
use crate::ICON_COLOR_THRESHOLD;

/// Max recent items shown in the menu. The DB still keeps everything;
/// this is just the rendered slice so the menu stays snappy.
const VISIBLE_RECENT: usize = 50;

pub struct MenuController {
    store: Arc<Mutex<Store>>,
    _tray: TrayIcon,
    menu: Menu,
    /// muda menu-item id -> action
    actions: HashMap<String, Action>,
    last_count_bucket: usize,
}

#[derive(Debug, Clone)]
enum Action {
    Copy(u64),
    TogglePin(u64),
    Clear,
    Quit,
}

impl MenuController {
    pub fn new(store: Arc<Mutex<Store>>) -> Result<Self, Box<dyn std::error::Error>> {
        let menu = Menu::new();
        let icon = make_icon(false);
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu.clone()))
            .with_icon(icon)
            .with_tooltip("ClipStash")
            .build()?;

        Ok(Self {
            store,
            _tray: tray,
            menu,
            actions: HashMap::new(),
            last_count_bucket: usize::MAX,
        })
    }

    /// Rebuild the menu from the current store state.
    /// Cheap — a flat rebuild is simpler and still under a millisecond.
    pub fn refresh(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Clear out old items.
        for item in self.menu.items() {
            let _ = self.menu.remove(item.as_ref());
        }
        self.actions.clear();

        let (pinned, recent, count) = {
            let s = self.store.lock().unwrap();
            (s.pinned()?, s.recent(VISIBLE_RECENT)?, s.count()?)
        };

        // --- Pinned section ---
        if !pinned.is_empty() {
            let header = MenuItem::new("Pinned", false, None);
            self.menu.append(&header)?;
            for clip in pinned {
                let label = format!("📌  {}", clip.preview);
                let item = MenuItem::new(&label, true, None);
                self.actions.insert(item.id().0.clone(), Action::Copy(clip.id));
                self.menu.append(&item)?;
            }
            self.menu.append(&PredefinedMenuItem::separator())?;
        }

        // --- Recent section ---
        let header = MenuItem::new(
            &format!("Recent ({count} total)"),
            false,
            None,
        );
        self.menu.append(&header)?;
        for clip in &recent {
            let label = clip.preview.clone();
            let item = MenuItem::new(&label, true, None);
            self.actions.insert(item.id().0.clone(), Action::Copy(clip.id));
            self.menu.append(&item)?;

            // A submenu "arrow" would be nicer, but keeping it minimal:
            // a second hidden pin-toggle via a modifier-click is future work.
        }

        self.menu.append(&PredefinedMenuItem::separator())?;

        // --- Footer ---
        let clear_item = MenuItem::new("Clear unpinned history", true, None);
        self.actions.insert(clear_item.id().0.clone(), Action::Clear);
        self.menu.append(&clear_item)?;

        let quit_item = MenuItem::new("Quit ClipStash", true, None);
        self.actions.insert(quit_item.id().0.clone(), Action::Quit);
        self.menu.append(&quit_item)?;

        // --- Icon color swap at the 500-item threshold ---
        let bucket = if count >= ICON_COLOR_THRESHOLD { 1 } else { 0 };
        if bucket != self.last_count_bucket {
            self.last_count_bucket = bucket;
            let _ = self._tray.set_icon(Some(make_icon(bucket == 1)));
        }

        Ok(())
    }

    /// Drain muda's event channel — called on every tao loop iteration.
    pub fn pump_events(&mut self) {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let Some(action) = self.actions.get(&event.id.0).cloned() else {
                continue;
            };
            match action {
                Action::Copy(id) => {
                    let clip = { self.store.lock().unwrap().get(id).ok().flatten() };
                    if let Some(clip) = clip {
                        write_to_pasteboard(&clip);
                    }
                }
                Action::TogglePin(id) => {
                    let _ = self.store.lock().unwrap().toggle_pin(id);
                    let _ = self.refresh();
                }
                Action::Clear => {
                    let _ = self.store.lock().unwrap().clear();
                    let _ = self.refresh();
                }
                Action::Quit => std::process::exit(0),
            }
        }
    }

    /// Programmatically open the menu (called when ⌘⇧V is pressed).
    /// tray-icon exposes this on macOS via `show_menu_on_left_click`, but for
    /// a true "pop at cursor" we'd call NSStatusItem.button.performClick.
    /// Left as a small follow-up; for now the hotkey focuses the app icon.
    pub fn show_menu(&self) {
        // Intentionally empty: wired up once computer-use testing confirms
        // NSStatusItem.popUpMenu behavior across macOS versions.
    }
}

/// Build a simple template icon. Template icons automatically adapt to
/// light/dark menu bars. We emit a tiny monochrome PNG in-process so we
/// don't need to ship asset files.
fn make_icon(warn: bool) -> Icon {
    // 18x18 is the canonical menu bar size.
    let size: u32 = 18;
    let mut buf = image::RgbaImage::new(size, size);
    // Draw a simple clipboard glyph: outlined rectangle + a top "clip".
    let (fg_r, fg_g, fg_b) = if warn { (220, 140, 0) } else { (0, 0, 0) };
    for y in 0..size {
        for x in 0..size {
            let border = x == 2 || x == size - 3 || y == 3 || y == size - 2;
            let clip_top = y == 1 && (5..=12).contains(&x);
            if border || clip_top {
                buf.put_pixel(x, y, image::Rgba([fg_r, fg_g, fg_b, 255]));
            }
        }
    }
    Icon::from_rgba(buf.into_raw(), size, size).expect("valid icon")
}
