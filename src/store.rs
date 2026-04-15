//! Persistent history store backed by `redb`.
//!
//! Why redb? Pure Rust, no C deps (keeps the binary tiny and build fast),
//! MVCC, crash-safe, and very quick for key/value workloads like ours.
//! SQLite is a fine alternative but overkill for a flat list of clips.
//!
//! Schema:
//!   table `clips`   : key = u64 monotonic id   -> value = bincode(Clip)
//!   table `pinned`  : key = u64 clip id        -> value = u64 pin order
//!   table `meta`    : key = &str               -> value = u64 (e.g. "next_id")

use std::path::{Path, PathBuf};

use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

const CLIPS: TableDefinition<u64, Vec<u8>> = TableDefinition::new("clips");
const PINNED: TableDefinition<u64, u64> = TableDefinition::new("pinned");
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");

/// A single clipboard entry. Images are stored as raw PNG bytes; text as UTF-8.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: u64,
    pub kind: ClipKind,
    /// Unix epoch seconds — cheap, stable, sortable.
    pub created_at: i64,
    /// Short preview used in the menu (first ~60 chars for text,
    /// "[Image 1024x768]" for images). Computed once at insert time.
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClipKind {
    Text(String),
    Image { png: Vec<u8>, width: u32, height: u32 },
}

pub struct Store {
    db: Database,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, redb::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let db = Database::create(path)?;
        // Make sure tables exist so later reads don't fail on a fresh DB.
        let wtxn = db.begin_write()?;
        {
            let _ = wtxn.open_table(CLIPS)?;
            let _ = wtxn.open_table(PINNED)?;
            let _ = wtxn.open_table(META)?;
        }
        wtxn.commit()?;
        Ok(Self { db })
    }

    /// Insert a new clip, de-duplicating against the most recent entry
    /// (we don't want a 10-entry history of the same string).
    pub fn insert(&self, kind: ClipKind) -> Result<Clip, redb::Error> {
        let wtxn = self.db.begin_write()?;
        let clip = {
            let mut meta = wtxn.open_table(META)?;
            let mut clips = wtxn.open_table(CLIPS)?;

            // Dedup against the newest existing clip.
            if let Some(entry) = clips.iter()?.next_back() {
                let (_, bytes) = entry?;
                if let Ok(existing) = bincode::deserialize::<Clip>(bytes.value().as_slice()) {
                    if same_content(&existing.kind, &kind) {
                        // Return the existing one without writing.
                        return Ok(existing);
                    }
                }
            }

            let next_id = meta.get("next_id")?.map(|v| v.value()).unwrap_or(1);
            meta.insert("next_id", next_id + 1)?;

            let preview = make_preview(&kind);
            let clip = Clip {
                id: next_id,
                kind,
                created_at: OffsetDateTime::now_utc().unix_timestamp(),
                preview,
            };
            let bytes = bincode::serialize(&clip).expect("serialize");
            clips.insert(next_id, bytes)?;
            clip
        };
        wtxn.commit()?;
        Ok(clip)
    }

    /// Count all clips — used to flip the status bar icon color at 500.
    pub fn count(&self) -> Result<usize, redb::Error> {
        let rtxn = self.db.begin_read()?;
        let clips = rtxn.open_table(CLIPS)?;
        Ok(clips.len()? as usize)
    }

    /// Return the most recent `limit` clips, newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<Clip>, redb::Error> {
        let rtxn = self.db.begin_read()?;
        let clips = rtxn.open_table(CLIPS)?;
        let mut out = Vec::with_capacity(limit);
        for entry in clips.iter()?.rev().take(limit) {
            let (_, v) = entry?;
            if let Ok(clip) = bincode::deserialize::<Clip>(v.value().as_slice()) {
                out.push(clip);
            }
        }
        Ok(out)
    }

    /// All pinned clips, in the order they were pinned.
    pub fn pinned(&self) -> Result<Vec<Clip>, redb::Error> {
        let rtxn = self.db.begin_read()?;
        let pinned = rtxn.open_table(PINNED)?;
        let clips = rtxn.open_table(CLIPS)?;
        let mut entries: Vec<(u64, u64)> = Vec::new();
        for row in pinned.iter()? {
            let (k, v) = row?;
            entries.push((k.value(), v.value()));
        }
        entries.sort_by_key(|(_, order)| *order);
        let mut out = Vec::with_capacity(entries.len());
        for (id, _) in entries {
            if let Some(v) = clips.get(id)? {
                if let Ok(clip) = bincode::deserialize::<Clip>(v.value().as_slice()) {
                    out.push(clip);
                }
            }
        }
        Ok(out)
    }

    pub fn toggle_pin(&self, id: u64) -> Result<(), redb::Error> {
        let wtxn = self.db.begin_write()?;
        {
            let mut pinned = wtxn.open_table(PINNED)?;
            if pinned.get(id)?.is_some() {
                pinned.remove(id)?;
            } else {
                let next_order = pinned.len()? + 1;
                pinned.insert(id, next_order)?;
            }
        }
        wtxn.commit()?;
        Ok(())
    }

    pub fn get(&self, id: u64) -> Result<Option<Clip>, redb::Error> {
        let rtxn = self.db.begin_read()?;
        let clips = rtxn.open_table(CLIPS)?;
        Ok(clips
            .get(id)?
            .and_then(|v| bincode::deserialize::<Clip>(v.value().as_slice()).ok()))
    }

    pub fn clear(&self) -> Result<(), redb::Error> {
        let wtxn = self.db.begin_write()?;
        {
            let mut clips = wtxn.open_table(CLIPS)?;
            // retain pinned clips only
            let pinned = wtxn.open_table(PINNED)?;
            let keep: std::collections::HashSet<u64> =
                pinned.iter()?.filter_map(|r| r.ok().map(|(k, _)| k.value())).collect();
            let ids: Vec<u64> = clips
                .iter()?
                .filter_map(|r| r.ok().map(|(k, _)| k.value()))
                .filter(|id| !keep.contains(id))
                .collect();
            for id in ids {
                clips.remove(id)?;
            }
        }
        wtxn.commit()?;
        Ok(())
    }
}

fn same_content(a: &ClipKind, b: &ClipKind) -> bool {
    match (a, b) {
        (ClipKind::Text(x), ClipKind::Text(y)) => x == y,
        (ClipKind::Image { png: x, .. }, ClipKind::Image { png: y, .. }) => x == y,
        _ => false,
    }
}

fn make_preview(kind: &ClipKind) -> String {
    match kind {
        ClipKind::Text(s) => {
            let trimmed: String = s.chars().take(60).collect();
            trimmed.replace('\n', " ")
        }
        ClipKind::Image { width, height, .. } => format!("[Image {width}x{height}]"),
    }
}

/// Default database location: ~/Library/Application Support/ClipStash/history.redb
pub fn default_db_path() -> Result<PathBuf, std::io::Error> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set"))?;
    let mut p = PathBuf::from(home);
    p.push("Library/Application Support/ClipStash/history.redb");
    Ok(p)
}
