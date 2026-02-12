// Author: Dustin Pilgrim
// License: MIT

use anyhow::{bail, Context, Result};
use eventline as el;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::spec::{Mode, Rgb, Spec};

const MAX_CACHED_IMAGES: usize = 5;

/* ---------- paths ---------- */

fn base_cache_dir() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_CACHE_HOME") {
        PathBuf::from(d).join("gesso")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".cache").join("gesso")
    } else {
        std::env::temp_dir().join("gesso-cache")
    }
}

fn frames_dir() -> PathBuf {
    base_cache_dir().join("frames")
}

fn entry_dir(entry_id: u64) -> PathBuf {
    frames_dir().join(entry_id.to_string())
}

fn frame_path(entry_id: u64, surface_index: usize, w: u32, h: u32) -> PathBuf {
    entry_dir(entry_id).join(format!("si{surface_index}_w{w}_h{h}.xrgb"))
}

fn cache_index_path() -> PathBuf {
    base_cache_dir().join("cache_index.json")
}

fn last_applied_path() -> PathBuf {
    base_cache_dir().join("last_applied.json")
}

// legacy single-slot (kept so older installs don't explode; not used by new multi-cache)
fn last_image_path() -> PathBuf {
    base_cache_dir().join("last_image.json")
}

// used only so callers can do: cached_image_matches(spec) -> load_last_frame(...)
fn last_match_path() -> PathBuf {
    base_cache_dir().join("last_match.json")
}

/* ---------- types ---------- */

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ImageKey {
    pub path: PathBuf,
    pub mode: Mode,
    pub colour: Rgb,

    pub size: u64,
    pub mtime_secs: u64,
    pub mtime_nanos: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CacheIndex {
    // Most-recent-first
    entries: Vec<CacheEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CacheEntry {
    id: u64,
    key: ImageKey,
    created_secs: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct LastMatch {
    id: u64,
}

/* ---------- io helpers ---------- */

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    el::scope!(
        "gesso.cache.atomic_write",
        success = "written",
        failure = "failed",
        aborted = "aborted",
        {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).context("create cache dir")?;
            }

            let tmp = path.with_extension("tmp");
            {
                let mut f = fs::File::create(&tmp).context("create tmp")?;
                f.write_all(bytes).context("write tmp")?;
                let _ = f.sync_all();
            }
            fs::rename(&tmp, path).context("rename tmp")?;

            el::debug!(
                "wrote path={path} bytes={bytes}",
                path = path.display().to_string(),
                bytes = bytes.len() as i64
            );

            Ok::<(), anyhow::Error>(())
        }
    )
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn new_entry_id() -> u64 {
    // nanos gives better uniqueness if you spam changes
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

fn read_cache_index() -> Result<CacheIndex> {
    let p = cache_index_path();
    let data = match fs::read(&p) {
        Ok(d) => d,
        Err(_) => return Ok(CacheIndex { entries: vec![] }),
    };
    let idx: CacheIndex = serde_json::from_slice(&data).context("parse cache_index")?;
    Ok(idx)
}

fn write_cache_index(idx: &CacheIndex) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(idx).context("serialize cache_index")?;
    atomic_write(&cache_index_path(), &bytes)
}

fn write_last_match_id(id: u64) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(&LastMatch { id }).context("serialize last_match")?;
    atomic_write(&last_match_path(), &bytes)
}

fn prune_index_and_frames(idx: &mut CacheIndex) {
    while idx.entries.len() > MAX_CACHED_IMAGES {
        if let Some(old) = idx.entries.pop() {
            let dir = entry_dir(old.id);
            let _ = fs::remove_dir_all(&dir);
            el::info!(
                "evicted cache_entry id={id} dir={dir}",
                id = old.id as i64,
                dir = dir.display().to_string()
            );
        }
    }
}

/* ---------- last_applied spec ---------- */

pub fn write_last_applied(spec: &Spec) -> Result<()> {
    el::scope!(
        "gesso.cache.write_last_applied",
        success = "saved",
        failure = "failed",
        aborted = "aborted",
        {
            let bytes = serde_json::to_vec_pretty(spec).context("serialize last_applied")?;
            let path = last_applied_path();

            el::debug!(
                "saving spec kind={kind}",
                kind = match spec {
                    Spec::Image { .. } => "image",
                    Spec::Colour { .. } => "colour",
                }
            );

            atomic_write(&path, &bytes)?;
            Ok::<(), anyhow::Error>(())
        }
    )
}

pub fn read_last_applied() -> Result<Option<Spec>> {
    el::scope!(
        "gesso.cache.read_last_applied",
        success = "loaded",
        failure = "failed",
        aborted = "aborted",
        {
            let p = last_applied_path();
            let data = match fs::read(&p) {
                Ok(d) => d,
                Err(_) => {
                    el::debug!("no cached spec found");
                    return Ok::<Option<Spec>, anyhow::Error>(None);
                }
            };

            let spec: Spec = serde_json::from_slice(&data).context("parse last_applied")?;

            el::debug!(
                "loaded spec kind={kind}",
                kind = match &spec {
                    Spec::Image { .. } => "image",
                    Spec::Colour { .. } => "colour",
                }
            );

            Ok::<Option<Spec>, anyhow::Error>(Some(spec))
        }
    )
}

/* ---------- image key ---------- */

fn file_times(path: &Path) -> Result<(u64, u32)> {
    let md = fs::metadata(path).with_context(|| format!("metadata: {}", path.display()))?;
    let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let dur = mtime.duration_since(UNIX_EPOCH).unwrap_or_default();
    Ok((dur.as_secs(), dur.subsec_nanos()))
}

pub fn compute_image_key(spec: &Spec) -> Result<ImageKey> {
    el::scope!(
        "gesso.cache.compute_image_key",
        success = "computed",
        failure = "failed",
        aborted = "aborted",
        {
            let (path, mode, colour) = match spec {
                Spec::Image { path, mode, colour, .. } => (path, *mode, *colour),
                _ => bail!("compute_image_key called on non-image spec"),
            };

            let expanded = crate::path::expand_user_path(path)?;
            let md = fs::metadata(&expanded).with_context(|| format!("metadata: {}", expanded.display()))?;
            let (secs, nanos) = file_times(&expanded)?;

            el::debug!(
                "computed path={path} size={size} mode={mode:?}",
                path = expanded.display().to_string(),
                size = md.len() as i64,
            );

            Ok::<ImageKey, anyhow::Error>(ImageKey {
                path: expanded,
                mode,
                colour,
                size: md.len(),
                mtime_secs: secs,
                mtime_nanos: nanos,
            })
        }
    )
}

/* ---------- multi-image cache index ---------- */

/// Record (or refresh) an image key in the MRU list, returning the entry id to write frames under.
///
/// Call this *before* storing frames for a newly-rendered image.
pub fn record_cached_image(spec: &Spec) -> Result<u64> {
    el::scope!(
        "gesso.cache.record_cached_image",
        success = "recorded",
        failure = "failed",
        aborted = "aborted",
        {
            let key = compute_image_key(spec)?;
            let mut idx = read_cache_index()?;

            // existing? move-to-front
            if let Some(pos) = idx.entries.iter().position(|e| e.key == key) {
                let mut e = idx.entries.remove(pos);
                let id = e.id;
                // refresh created time (optional; helps debugging)
                e.created_secs = now_secs();
                idx.entries.insert(0, e);
                prune_index_and_frames(&mut idx);
                write_cache_index(&idx)?;
                el::info!("cache_mru_refresh id={id}", id = id as i64);
                return Ok(id);
            }

            // new entry
            let id = new_entry_id();
            let created_secs = now_secs();
            idx.entries.insert(0, CacheEntry { id, key, created_secs });
            prune_index_and_frames(&mut idx);
            write_cache_index(&idx)?;

            el::info!("cache_mru_insert id={id}", id = id as i64);
            Ok::<u64, anyhow::Error>(id)
        }
    )
}

/// Find the cache entry id for this spec (if present). Also writes `last_match.json`
/// so older call sites can do `cached_image_matches()` then `load_last_frame()`.
pub fn find_cached_entry_id(spec: &Spec) -> Result<Option<u64>> {
    el::scope!(
        "gesso.cache.find_cached_entry_id",
        success = "found",
        failure = "failed",
        aborted = "aborted",
        {
            let key = compute_image_key(spec)?;
            let idx = read_cache_index()?;

            if let Some(e) = idx.entries.iter().find(|e| e.key == key) {
                let _ = write_last_match_id(e.id);
                return Ok::<Option<u64>, anyhow::Error>(Some(e.id));
            }

            Ok::<Option<u64>, anyhow::Error>(None)
        }
    )
}

/* ---------- legacy API compatibility (kept) ---------- */

/// Legacy: writes `last_image.json` (single slot). Not used by the new multi-cache flow.
/// Keep this until you finish updating call sites.
pub fn write_last_image_key(key: &ImageKey) -> Result<()> {
    el::scope!(
        "gesso.cache.write_last_image_key",
        success = "saved",
        failure = "failed",
        aborted = "aborted",
        {
            let bytes = serde_json::to_vec_pretty(key).context("serialize last_image")?;
            atomic_write(&last_image_path(), &bytes)?;
            Ok::<(), anyhow::Error>(())
        }
    )
}

/// Legacy: reads `last_image.json` (single slot). Not used by the new multi-cache flow.
pub fn read_last_image_key() -> Result<Option<ImageKey>> {
    el::scope!(
        "gesso.cache.read_last_image_key",
        success = "loaded",
        failure = "failed",
        aborted = "aborted",
        {
            let p = last_image_path();
            let data = match fs::read(&p) {
                Ok(d) => d,
                Err(_) => return Ok::<Option<ImageKey>, anyhow::Error>(None),
            };
            let key: ImageKey = serde_json::from_slice(&data).context("parse last_image")?;
            Ok::<Option<ImageKey>, anyhow::Error>(Some(key))
        }
    )
}

/// Updated behavior: checks the *multi-cache* and, if found, records the matching entry id
/// into `last_match.json` so `load_last_frame()` can work without changing call sites.
pub fn cached_image_matches(spec: &Spec) -> Result<bool> {
    el::scope!(
        "gesso.cache.cached_image_matches",
        success = "checked",
        failure = "failed",
        aborted = "aborted",
        {
            let hit = find_cached_entry_id(spec)?.is_some();
            el::info!("cache_check matches={matches}", matches = hit);
            Ok::<bool, anyhow::Error>(hit)
        }
    )
}

/* ---------- frame blobs (multi-cache) ---------- */

pub fn load_frame(entry_id: u64, surface_index: usize, w: u32, h: u32) -> Result<Option<Arc<[u32]>>> {
    el::scope!(
        "gesso.cache.load_frame",
        success = "loaded",
        failure = "failed",
        aborted = "aborted",
        {
            let p = frame_path(entry_id, surface_index, w, h);
            let data = match fs::read(&p) {
                Ok(d) => d,
                Err(_) => {
                    el::debug!(
                        "no cached frame id={id} si={si} w={w} h={h}",
                        id = entry_id as i64,
                        si = surface_index as i64,
                        w = w as i64,
                        h = h as i64
                    );
                    return Ok::<Option<Arc<[u32]>>, anyhow::Error>(None);
                }
            };

            let want_bytes = (w as usize) * (h as usize) * 4;
            if data.len() != want_bytes {
                el::warn!(
                    "frame size mismatch id={id} si={si} got={got} want={want}",
                    id = entry_id as i64,
                    si = surface_index as i64,
                    got = data.len() as i64,
                    want = want_bytes as i64
                );
                return Ok::<Option<Arc<[u32]>>, anyhow::Error>(None);
            }

            let mut out = Vec::<u32>::with_capacity(want_bytes / 4);
            for chunk in data.chunks_exact(4) {
                out.push(u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }

            el::info!(
                "loaded frame id={id} si={si} w={w} h={h} pixels={pixels}",
                id = entry_id as i64,
                si = surface_index as i64,
                w = w as i64,
                h = h as i64,
                pixels = out.len() as i64
            );

            Ok::<Option<Arc<[u32]>>, anyhow::Error>(Some(out.into()))
        }
    )
}

pub fn store_frame(entry_id: u64, surface_index: usize, w: u32, h: u32, frame: &Arc<[u32]>) -> Result<()> {
    el::scope!(
        "gesso.cache.store_frame",
        success = "stored",
        failure = "failed",
        aborted = "aborted",
        {
            let p = frame_path(entry_id, surface_index, w, h);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).context("create entry frames dir")?;
            }

            let mut bytes = Vec::with_capacity(frame.len() * 4);
            for &px in frame.iter() {
                bytes.extend_from_slice(&px.to_ne_bytes());
            }

            el::info!(
                "storing frame id={id} si={si} w={w} h={h} pixels={pixels} bytes={bytes}",
                id = entry_id as i64,
                si = surface_index as i64,
                w = w as i64,
                h = h as i64,
                pixels = frame.len() as i64,
                bytes = bytes.len() as i64
            );

            atomic_write(&p, &bytes)?;
            Ok::<(), anyhow::Error>(())
        }
    )
}
