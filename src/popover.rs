//! Status-bar item + custom popover panel (ClipVault-parity).
//!
//! Layout:
//!
//!   ┌──────────────────────────────────────────┐
//!   │ ⚙︎       N items               Clear     │  header   (32pt)
//!   ├──────────────────────────────────────────┤
//!   │ 🔍 Search…                 [ All  ▾ ]    │  search   (32pt)
//!   ├──────────────────────────────────────────┤
//!   │  preview text…               2026/04/16  │
//!   │                                  00:19   │  NSTableView
//!   │  …                                       │
//!   ├──────────────────────────────────────────┤
//!   │  Quit ClipStash                    ⌘Q    │  footer   (28pt)
//!   └──────────────────────────────────────────┘
//!
//! All AppKit, no web view, no extra timers. The clipboard watcher is the
//! only periodic source in the process — popover reloads are event-driven
//! (new clip, hotkey press, user interaction inside the panel).

// Many objc2 0.3 methods are safe, but we keep `unsafe {}` around the
// AppKit construction blocks for clarity and so the phase-2-expand work
// below doesn't require threading more unsafety through the file.
#![allow(unused_unsafe, dead_code, deprecated)]

use std::sync::{Arc, Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSButton, NSColor,
    NSControlTextEditingDelegate, NSPanel, NSScrollView, NSSearchField, NSStatusBar, NSStatusItem,
    NSTableColumn, NSTableView, NSTableViewDataSource, NSTableViewDelegate, NSTextField,
    NSVariableStatusItemLength, NSView, NSVisualEffectMaterial, NSVisualEffectView,
    NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSSize, NSString,
};
use time::OffsetDateTime;

use crate::clipboard::write_to_pasteboard;
use crate::store::{Clip, ClipKind, Store};

// ---------------------------------------------------------------------------
// Filter dropdown
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Text,
    Image,
}

// ---------------------------------------------------------------------------
// Shared mutable popover state
//
// The delegate class and the Popover handle both talk to this. Everything
// inside is only touched on the main thread, so a plain Mutex is enough to
// satisfy Rust's aliasing rules without ever actually contending.
// ---------------------------------------------------------------------------

struct PopoverState {
    store: Arc<Mutex<Store>>,
    status_item: Retained<NSStatusItem>,
    panel: Retained<NSPanel>,
    table: Retained<NSTableView>,
    search_field: Retained<NSSearchField>,
    count_label: Retained<NSTextField>,
    rows: Vec<Clip>,
    search: String,
    filter: Filter,
    mtm: MainThreadMarker,
}

// SAFETY: every access goes through the main thread (AppKit callbacks and
// the tao event loop both run there). We take a Mutex only so Rust lets us
// mutate through `&self`.
unsafe impl Send for PopoverState {}

static STATE: OnceLock<Mutex<PopoverState>> = OnceLock::new();

fn with_state<R>(f: impl FnOnce(&mut PopoverState) -> R) -> Option<R> {
    let mtx = STATE.get()?;
    let mut guard = mtx.lock().ok()?;
    Some(f(&mut guard))
}

// ---------------------------------------------------------------------------
// Delegate class — data source + target/action sink
// ---------------------------------------------------------------------------

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ClipStashDelegate"]
    #[derive(Debug)]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    // NSTableViewDelegate refines NSControlTextEditingDelegate, so we have
    // to declare conformance (empty — we don't actually edit table cells).
    unsafe impl NSControlTextEditingDelegate for Delegate {}

    unsafe impl NSTableViewDataSource for Delegate {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _tv: &NSTableView) -> NSInteger {
            with_state(|s| s.rows.len() as NSInteger).unwrap_or(0)
        }
    }

    unsafe impl NSTableViewDelegate for Delegate {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            _tv: &NSTableView,
            _col: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            let mtm = MainThreadMarker::from(self);
            with_state(|s| {
                let clip = s.rows.get(row as usize)?;
                Some(build_row_view(mtm, clip))
            })
            .flatten()
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_changed(&self, notif: &NSNotification) {
            unsafe {
                let obj = notif.object();
                let Some(obj) = obj else { return };
                let tv: *const NSTableView = (&*obj as *const AnyObject).cast();
                let row = (*tv).selectedRow();
                if row < 0 {
                    return;
                }
                let clip = with_state(|s| s.rows.get(row as usize).cloned()).flatten();
                if let Some(clip) = clip {
                    write_to_pasteboard(&clip);
                    hide_panel();
                }
                (*tv).deselectAll(None);
            }
        }
    }

    impl Delegate {
        #[unsafe(method(onStatusClick:))]
        fn on_status_click(&self, _sender: Option<&AnyObject>) {
            toggle_panel();
        }

        #[unsafe(method(onSearchChanged:))]
        fn on_search_changed(&self, _sender: Option<&AnyObject>) {
            with_state(|s| {
                let text = s.search_field.stringValue();
                s.search = text.to_string();
            });
            reload_rows();
        }

        #[unsafe(method(onClear:))]
        fn on_clear(&self, _sender: Option<&AnyObject>) {
            with_state(|s| {
                if let Ok(store) = s.store.lock() {
                    let _ = store.clear();
                }
            });
            reload_rows();
        }

        #[unsafe(method(onQuit:))]
        fn on_quit(&self, _sender: Option<&AnyObject>) {
            std::process::exit(0);
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

// ---------------------------------------------------------------------------
// Public handle used by main.rs
// ---------------------------------------------------------------------------

pub struct Popover {
    _delegate: Retained<Delegate>,
}

impl Popover {
    pub fn new(
        store: Arc<Mutex<Store>>,
        mtm: MainThreadMarker,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let delegate = Delegate::new(mtm);

        // --- Status item ---------------------------------------------------
        let bar = NSStatusBar::systemStatusBar();
        let status_item = bar.statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = status_item.button(mtm) {
            button.setTitle(&NSString::from_str("📋"));
            unsafe {
                button.setTarget(Some(&*delegate));
                button.setAction(Some(sel!(onStatusClick:)));
            }
        }

        // --- Panel ---------------------------------------------------------
        let panel_size = NSSize::new(360.0, 520.0);
        let panel_rect = NSRect::new(NSPoint::new(0.0, 0.0), panel_size);
        let style = NSWindowStyleMask::NonactivatingPanel
            | NSWindowStyleMask::Borderless
            | NSWindowStyleMask::UtilityWindow;
        let panel: Retained<NSPanel> = unsafe {
            let alloc = NSPanel::alloc(mtm);
            msg_send![
                alloc,
                initWithContentRect: panel_rect,
                styleMask: style,
                backing: NSBackingStoreType::Buffered,
                defer: true,
            ]
        };
        panel.setFloatingPanel(true);
        panel.setHidesOnDeactivate(true);
        unsafe {
            panel.setOpaque(false);
            panel.setBackgroundColor(Some(&NSColor::clearColor()));
            panel.setLevel(objc2_app_kit::NSFloatingWindowLevel);
        }

        // --- Content: NSVisualEffectView (dark HUD chrome) ----------------
        let vfx: Retained<NSVisualEffectView> = unsafe {
            let alloc = NSVisualEffectView::alloc(mtm);
            msg_send![alloc, initWithFrame: panel_rect]
        };
        unsafe {
            vfx.setMaterial(NSVisualEffectMaterial::HUDWindow);
            vfx.setWantsLayer(true);
            // TODO: round corners via CALayer once we wire in objc2-quartz-core.
        }
        panel.setContentView(Some(&vfx));

        // --- Header --------------------------------------------------------
        let header_h = 32.0;
        let search_h = 32.0;
        let footer_h = 28.0;
        let width = panel_size.width;
        let height = panel_size.height;

        let count_label = make_label(
            mtm,
            NSRect::new(
                NSPoint::new(0.0, height - header_h + 6.0),
                NSSize::new(width, 20.0),
            ),
            "0 items",
            true,
        );
        unsafe {
            count_label.setAlignment(objc2_app_kit::NSTextAlignment::Center);
        }
        vfx.addSubview(&count_label);

        let clear_btn = make_text_button(
            mtm,
            NSRect::new(
                NSPoint::new(width - 60.0, height - header_h + 2.0),
                NSSize::new(52.0, 24.0),
            ),
            "Clear",
            &delegate,
            sel!(onClear:),
        );
        vfx.addSubview(&clear_btn);

        // --- Search field --------------------------------------------------
        let search_y = height - header_h - search_h + 4.0;
        let search_field: Retained<NSSearchField> = unsafe {
            let alloc = NSSearchField::alloc(mtm);
            let rect = NSRect::new(NSPoint::new(8.0, search_y), NSSize::new(width - 16.0, 24.0));
            let sf: Retained<NSSearchField> = msg_send![alloc, initWithFrame: rect];
            sf.setPlaceholderString(Some(&NSString::from_str("Search…")));
            // Live search via target/action rather than the delegate protocol —
            // NSSearchField fires its action on every keystroke.
            sf.setTarget(Some(&*delegate));
            sf.setAction(Some(sel!(onSearchChanged:)));
            sf
        };
        vfx.addSubview(&search_field);

        // --- Table + scroll view ------------------------------------------
        let table_y = footer_h;
        let table_h = height - header_h - search_h - footer_h;
        let scroll_rect = NSRect::new(NSPoint::new(0.0, table_y), NSSize::new(width, table_h));
        let scroll: Retained<NSScrollView> = unsafe {
            let alloc = NSScrollView::alloc(mtm);
            let s: Retained<NSScrollView> = msg_send![alloc, initWithFrame: scroll_rect];
            s.setHasVerticalScroller(true);
            s.setDrawsBackground(false);
            s
        };

        let table: Retained<NSTableView> = unsafe {
            let alloc = NSTableView::alloc(mtm);
            let t: Retained<NSTableView> =
                msg_send![alloc, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, table_h))];
            t.setHeaderView(None);
            t.setRowHeight(44.0);
            t.setBackgroundColor(&NSColor::clearColor());
            t.setAllowsMultipleSelection(false);
            t.setIntercellSpacing(NSSize::new(0.0, 0.0));

            let col: Retained<NSTableColumn> = {
                let alloc = NSTableColumn::alloc(mtm);
                let identifier = NSString::from_str("clip");
                msg_send![alloc, initWithIdentifier: &*identifier]
            };
            col.setWidth(width);
            t.addTableColumn(&col);

            t.setDataSource(Some(ProtocolObject::from_ref(&*delegate)));
            t.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            t
        };
        unsafe {
            scroll.setDocumentView(Some(&table));
        }
        vfx.addSubview(&scroll);

        // --- Footer --------------------------------------------------------
        let quit_btn = make_text_button(
            mtm,
            NSRect::new(NSPoint::new(8.0, 4.0), NSSize::new(width - 16.0, 22.0)),
            "Quit ClipStash   ⌘Q",
            &delegate,
            sel!(onQuit:),
        );
        vfx.addSubview(&quit_btn);

        // --- Install shared state -----------------------------------------
        let initial = PopoverState {
            store,
            status_item,
            panel,
            table,
            search_field,
            count_label,
            rows: Vec::new(),
            search: String::new(),
            filter: Filter::All,
            mtm,
        };
        STATE
            .set(Mutex::new(initial))
            .map_err(|_| "popover already initialised")?;

        reload_rows();

        Ok(Self {
            _delegate: delegate,
        })
    }

    pub fn on_clip_added(&mut self) {
        reload_rows();
    }

    pub fn toggle(&self) {
        toggle_panel();
    }
}

// ---------------------------------------------------------------------------
// Free helpers that look up the shared state
// ---------------------------------------------------------------------------

fn toggle_panel() {
    with_state(|s| unsafe {
        if s.panel.isVisible() {
            s.panel.orderOut(None);
            return;
        }
        anchor_under_status_item(s);
        s.panel.makeKeyAndOrderFront(None);
        NSApplication::sharedApplication(s.mtm).activateIgnoringOtherApps(true);
    });
}

fn hide_panel() {
    with_state(|s| unsafe { s.panel.orderOut(None) });
}

unsafe fn anchor_under_status_item(s: &PopoverState) {
    let Some(button) = s.status_item.button(s.mtm) else {
        return;
    };
    let Some(window) = button.window() else {
        return;
    };
    let button_frame = button.frame();
    let on_screen = window.convertRectToScreen(button_frame);
    let panel_frame = s.panel.frame();
    // Position panel so its top edge sits just under the status button,
    // horizontally centered on the button.
    let origin = NSPoint::new(
        on_screen.origin.x + (on_screen.size.width - panel_frame.size.width) / 2.0,
        on_screen.origin.y - panel_frame.size.height - 4.0,
    );
    s.panel.setFrameOrigin(origin);
}

fn reload_rows() {
    with_state(|s| {
        let (pinned, recent, count) = {
            let store = s.store.lock().unwrap();
            (
                store.pinned().unwrap_or_default(),
                store.recent(500).unwrap_or_default(),
                store.count().unwrap_or(0),
            )
        };
        let needle = s.search.to_lowercase();
        let filter = s.filter;
        s.rows = pinned
            .into_iter()
            .chain(recent.into_iter())
            .filter(|c| match filter {
                Filter::All => true,
                Filter::Text => matches!(c.kind, ClipKind::Text(_)),
                Filter::Image => matches!(c.kind, ClipKind::Image { .. }),
            })
            .filter(|c| needle.is_empty() || c.preview.to_lowercase().contains(&needle))
            .collect();
        unsafe {
            s.count_label
                .setStringValue(&NSString::from_str(&format!("{count} items")));
            s.table.reloadData();
        }
    });
}

// ---------------------------------------------------------------------------
// View construction helpers
// ---------------------------------------------------------------------------

fn make_label(
    mtm: MainThreadMarker,
    frame: NSRect,
    text: &str,
    bold: bool,
) -> Retained<NSTextField> {
    unsafe {
        let alloc = NSTextField::alloc(mtm);
        let tf: Retained<NSTextField> = msg_send![alloc, initWithFrame: frame];
        tf.setStringValue(&NSString::from_str(text));
        tf.setBezeled(false);
        tf.setDrawsBackground(false);
        tf.setEditable(false);
        tf.setSelectable(false);
        tf.setTextColor(Some(&NSColor::labelColor()));
        if bold {
            tf.setFont(Some(&objc2_app_kit::NSFont::boldSystemFontOfSize(12.0)));
        } else {
            tf.setFont(Some(&objc2_app_kit::NSFont::systemFontOfSize(12.0)));
        }
        tf
    }
}

fn make_text_button(
    mtm: MainThreadMarker,
    frame: NSRect,
    title: &str,
    target: &Delegate,
    action: Sel,
) -> Retained<NSButton> {
    unsafe {
        let alloc = NSButton::alloc(mtm);
        let btn: Retained<NSButton> = msg_send![alloc, initWithFrame: frame];
        btn.setTitle(&NSString::from_str(title));
        btn.setBordered(false);
        btn.setTarget(Some(&**target as &AnyObject));
        btn.setAction(Some(action));
        btn
    }
}

fn build_row_view(mtm: MainThreadMarker, clip: &Clip) -> Retained<NSView> {
    let width = 360.0;
    let row_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, 44.0));
    let view: Retained<NSView> = unsafe {
        let alloc = NSView::alloc(mtm);
        msg_send![alloc, initWithFrame: row_rect]
    };

    let title = make_label(
        mtm,
        NSRect::new(NSPoint::new(12.0, 20.0), NSSize::new(width - 100.0, 18.0)),
        &clip.preview,
        false,
    );
    let date = make_label(
        mtm,
        NSRect::new(NSPoint::new(width - 90.0, 20.0), NSSize::new(80.0, 16.0)),
        &format_ts(clip.created_at),
        false,
    );
    unsafe {
        date.setAlignment(objc2_app_kit::NSTextAlignment::Right);
        date.setTextColor(Some(&NSColor::secondaryLabelColor()));
        view.addSubview(&title);
        view.addSubview(&date);
    }
    view
}

fn format_ts(unix: i64) -> String {
    OffsetDateTime::from_unix_timestamp(unix)
        .map(|dt| {
            format!(
                "{:04}/{:02}/{:02} {:02}:{:02}",
                dt.year(),
                u8::from(dt.month()),
                dt.day(),
                dt.hour(),
                dt.minute()
            )
        })
        .unwrap_or_else(|_| "—".into())
}
