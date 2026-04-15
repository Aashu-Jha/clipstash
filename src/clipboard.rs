//! NSPasteboard watcher.
//!
//! The ONLY correct way to watch the macOS clipboard is to poll
//! `NSPasteboard.generalPasteboard.changeCount`. This is a single integer
//! compare per tick — effectively free. We ONLY read the actual payload
//! when that counter has changed, which is why this costs ~0% CPU at idle.
//!
//! ClipVault's 99% CPU is almost certainly them reading full pasteboard
//! contents every frame, or running a tight busy loop. We avoid both.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::ClassType;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString};
use objc2_foundation::{NSArray, NSString};
use tao::event_loop::EventLoopProxy;

use crate::store::{ClipKind, Store};
use crate::AppEvent;

pub struct ClipboardWatcher {
    _handle: thread::JoinHandle<()>,
}

impl ClipboardWatcher {
    pub fn spawn(
        store: Arc<Mutex<Store>>,
        proxy: EventLoopProxy<AppEvent>,
        interval: Duration,
    ) -> Self {
        let handle = thread::Builder::new()
            .name("clipstash-watcher".into())
            .spawn(move || watcher_loop(store, proxy, interval))
            .expect("spawn watcher thread");
        Self { _handle: handle }
    }
}

fn watcher_loop(
    store: Arc<Mutex<Store>>,
    proxy: EventLoopProxy<AppEvent>,
    interval: Duration,
) {
    // SAFETY: NSPasteboard.generalPasteboard must be touched on a thread
    // with an autorelease pool. We create a fresh pool per tick.
    let mut last_change: i64 = -1;

    loop {
        // Each poll happens inside its own autorelease pool so memory
        // from Foundation strings/arrays is freed immediately.
        objc2::rc::autoreleasepool(|_| unsafe {
            let pb: Retained<NSPasteboard> = NSPasteboard::generalPasteboard();
            let current = pb.changeCount();

            if current as i64 != last_change {
                last_change = current as i64;

                // Try text first (most common, cheapest path).
                if let Some(text) = read_text(&pb) {
                    if !text.is_empty() {
                        if let Ok(s) = store.lock() {
                            let _ = s.insert(ClipKind::Text(text));
                        }
                        let _ = proxy.send_event(AppEvent::ClipboardChanged);
                        return;
                    }
                }

                // Fall back to image.
                if let Some((png, w, h)) = read_image(&pb) {
                    if let Ok(s) = store.lock() {
                        let _ = s.insert(ClipKind::Image { png, width: w, height: h });
                    }
                    let _ = proxy.send_event(AppEvent::ClipboardChanged);
                }
            }
        });

        thread::sleep(interval);
    }
}

unsafe fn read_text(pb: &NSPasteboard) -> Option<String> {
    let ns_type = NSPasteboardTypeString;
    let value = pb.stringForType(ns_type)?;
    Some(value.to_string())
}

unsafe fn read_image(pb: &NSPasteboard) -> Option<(Vec<u8>, u32, u32)> {
    let png_type = NSPasteboardTypePNG;
    let data = pb.dataForType(png_type)?;
    let bytes: &[u8] = data.bytes();
    // Decode once to extract dimensions for the preview label.
    let img = image::load_from_memory(bytes).ok()?;
    Some((bytes.to_vec(), img.width(), img.height()))
}

/// Write a clip back to the system pasteboard (used when the user
/// picks a history entry from the menu).
pub fn write_to_pasteboard(clip: &crate::store::Clip) {
    objc2::rc::autoreleasepool(|_| unsafe {
        let pb: Retained<NSPasteboard> = NSPasteboard::generalPasteboard();
        pb.clearContents();
        match &clip.kind {
            ClipKind::Text(s) => {
                let ns = NSString::from_str(s);
                let types = NSArray::from_slice(&[NSPasteboardTypeString]);
                pb.declareTypes_owner(&types, None);
                pb.setString_forType(&ns, NSPasteboardTypeString);
            }
            ClipKind::Image { png, .. } => {
                use objc2_foundation::NSData;
                let data = NSData::with_bytes(png);
                let types = NSArray::from_slice(&[NSPasteboardTypePNG]);
                pb.declareTypes_owner(&types, None);
                pb.setData_forType(&data, NSPasteboardTypePNG);
            }
        }
    });
}
