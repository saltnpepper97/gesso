// Author: Dustin Pilgrim
// License: MIT

use anyhow::{Context, Result};
use eventline as el;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::spec::{Mode, Rgb, Spec};

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

fn frame_path(surface_index: usize, w: u32, h: u32) -> PathBuf {
    frames_dir().join(format!("si{surface_index}_w{w}_h{h}.xrgb"))
}

fn last_applied_path() -> PathBuf {
    base_cache_dir().join("last_applied.json")
}

fn last_image_path() -> PathBuf {
    base_cache_dir().join("last_image.json")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ImageKey {
    pub path: PathBuf,
    pub mode: Mode,
    pub colour: Rgb,

    pub size: u64,
    pub mtime_secs: u64,
    pub mtime_nanos: u32,
}

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
                _ => anyhow::bail!("compute_image_key called on non-image spec"),
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

pub fn write_last_image_key(key: &ImageKey) -> Result<()> {
    el::scope!(
        "gesso.cache.write_last_image_key",
        success = "saved",
        failure = "failed",
        aborted = "aborted",
        {
            let bytes = serde_json::to_vec_pretty(key).context("serialize last_image")?;
            let path = last_image_path();

            el::debug!(
                "saving image_key path={path} size={size}",
                path = key.path.display().to_string(),
                size = key.size as i64
            );

            atomic_write(&path, &bytes)?;
            Ok::<(), anyhow::Error>(())
        }
    )
}

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
                Err(_) => {
                    el::debug!("no cached image key found");
                    return Ok::<Option<ImageKey>, anyhow::Error>(None);
                }
            };
            
            let key: ImageKey = serde_json::from_slice(&data).context("parse last_image")?;
            
            el::debug!(
                "loaded image_key path={path} size={size}",
                path = key.path.display().to_string(),
                size = key.size as i64
            );

            Ok::<Option<ImageKey>, anyhow::Error>(Some(key))
        }
    )
}

pub fn cached_image_matches(spec: &Spec) -> Result<bool> {
    el::scope!(
        "gesso.cache.cached_image_matches",
        success = "checked",
        failure = "failed",
        aborted = "aborted",
        {
            let Some(saved) = read_last_image_key()? else { 
                el::debug!("no saved key - cache miss");
                return Ok::<bool, anyhow::Error>(false);
            };
            
            let current = compute_image_key(spec)?;
            let matches = saved == current;
            
            el::info!(
                "cache_check matches={matches} path={path}",
                matches = matches,
                path = current.path.display().to_string()
            );

            Ok::<bool, anyhow::Error>(matches)
        }
    )
}

/* ---------- frame blobs ---------- */

pub fn load_last_frame(surface_index: usize, w: u32, h: u32) -> Result<Option<Arc<[u32]>>> {
    el::scope!(
        "gesso.cache.load_last_frame",
        success = "loaded",
        failure = "failed",
        aborted = "aborted",
        {
            let p = frame_path(surface_index, w, h);
            let data = match fs::read(&p) {
                Ok(d) => d,
                Err(_) => {
                    el::debug!(
                        "no cached frame si={si} w={w} h={h}",
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
                    "frame size mismatch si={si} got={got} want={want}",
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
                "loaded frame si={si} w={w} h={h} pixels={pixels}",
                si = surface_index as i64,
                w = w as i64,
                h = h as i64,
                pixels = out.len() as i64
            );

            Ok::<Option<Arc<[u32]>>, anyhow::Error>(Some(out.into()))
        }
    )
}

pub fn store_last_frame(surface_index: usize, w: u32, h: u32, frame: &Arc<[u32]>) -> Result<()> {
    el::scope!(
        "gesso.cache.store_last_frame",
        success = "stored",
        failure = "failed",
        aborted = "aborted",
        {
            fs::create_dir_all(frames_dir()).context("create frames dir")?;
            let p = frame_path(surface_index, w, h);

            let mut bytes = Vec::with_capacity(frame.len() * 4);
            for &px in frame.iter() {
                bytes.extend_from_slice(&px.to_ne_bytes());
            }

            el::info!(
                "storing frame si={si} w={w} h={h} pixels={pixels} bytes={bytes}",
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
