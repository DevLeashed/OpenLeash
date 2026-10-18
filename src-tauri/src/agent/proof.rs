//! Proof clips and before/after visual diffs: evidence that a UI change works.
//!
//! Two things live here, both pure storage and encoding:
//!
//! - **Proof clips.** An agent starts a recording, drives its browser (or the
//!   computer-use tools), then stops it and gets back an animated PNG it can
//!   show with `![…](path)` in its handoff. The recorder *never captures
//!   anything itself*: it only keeps frames the existing tools already took,
//!   after their own permission checks — a browser screenshot after an action
//!   that passed its gate, or the screenshot a `computer`/`screenshot` call
//!   returned. That is the whole security argument: recording is not a new way
//!   to see the screen, so it needs no approval of its own and cannot get round
//!   one. `push` also refuses a frame from the wrong source or the wrong agent,
//!   so a browser clip can never collect desktop captures and one chat's
//!   recording never collects another's.
//! - **Visual diffs.** A baseline screenshot of a URL taken before the agent
//!   edits UI, and an "after" of the same URL, shown side by side in Review.
//!
//! APNG rather than GIF or animated WebP: the `png` crate already in the tree
//! encodes it (no new dependency), WebView2/WebKit render it in an `<img>`, the
//! reply-image pipeline accepts `.png`, and it is lossless — a 256-colour GIF
//! smears exactly the text and thin borders a UI proof is meant to show.
//!
//! Everything is written under `<data dir>/proof/<task id>/`: app-owned, never
//! inside the project (so nothing is ever committed by accident), and removed
//! with the chat.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Most frames one clip keeps. Frames are stored compressed, so memory is
/// roughly this many small PNGs, not this many raw bitmaps.
pub const MAX_FRAMES: usize = 40;
/// Longest edge of a clip frame. Big enough to read a UI, small enough that 40
/// frames fit the inline-image limit.
pub const MAX_EDGE: u32 = 960;
/// A recording older than this stops taking frames. It can still be stopped
/// and saved; it just can't grow forever behind a forgotten `record_start`.
pub const MAX_DURATION: Duration = Duration::from_secs(300);
pub const MIN_FRAME_MS: u64 = 200;
pub const MAX_FRAME_MS: u64 = 5000;
pub const DEFAULT_FRAME_MS: u64 = 1000;
/// The longest a single (deduplicated) frame is held on screen.
const MAX_HOLD_MS: u64 = 10_000;
/// The encoded clip must be showable inline, which caps it at the image limit.
pub const MAX_CLIP_BYTES: u64 = super::tools::MAX_IMAGE_BYTES;
/// URLs one chat's visual diff may hold.
pub const MAX_VISUAL_URLS: usize = 6;
/// Longest edge of a stored visual-diff screenshot.
const VISUAL_MAX_EDGE: u32 = 1600;

/// Where the frames of a recording come from. A recording takes frames from
/// exactly one source, chosen when it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The agent's own headless browser.
    Browser,
    /// The computer-use screenshots (`computer`, `screenshot`).
    Screen,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::Browser => "browser",
            Source::Screen => "screen",
        }
    }
}

struct Frame {
    /// The frame as a (downscaled) PNG; decoded again only when encoding.
    png: Vec<u8>,
    /// Hash of the decoded pixels, so a frame identical to the last one just
    /// holds that one longer instead of costing another frame.
    hash: [u8; 32],
    /// How long it stays on screen, in ms.
    hold_ms: u64,
}

struct Recording {
    source: Source,
    frame_ms: u64,
    started: Instant,
    frames: Vec<Frame>,
    /// Frames refused for the frame/duration caps, so the result can say so.
    dropped: usize,
    title: String,
}

/// Recordings by agent (`<task>:<sub>`, the key the browser sessions and
/// computer-use geometry already use).
fn recordings() -> std::sync::MutexGuard<'static, HashMap<String, Recording>> {
    static R: std::sync::OnceLock<Mutex<HashMap<String, Recording>>> = std::sync::OnceLock::new();
    // Poisoning is not worth propagating: this is a buffer of pictures.
    R.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Start recording for `owner`. Fails if one is already running, rather than
/// silently throwing away the frames it holds.
pub fn start(
    owner: &str,
    source: Source,
    frame_ms: Option<u64>,
    title: &str,
) -> Result<u64, String> {
    let mut map = recordings();
    if let Some(r) = map.get(owner) {
        return Err(format!(
            "A {} recording is already running ({} frame(s)). Stop it first with `record_stop`.",
            r.source.label(),
            r.frames.len()
        ));
    }
    let frame_ms = frame_ms
        .unwrap_or(DEFAULT_FRAME_MS)
        .clamp(MIN_FRAME_MS, MAX_FRAME_MS);
    map.insert(
        owner.to_string(),
        Recording {
            source,
            frame_ms,
            started: Instant::now(),
            frames: Vec::new(),
            dropped: 0,
            title: title.chars().take(80).collect(),
        },
    );
    Ok(frame_ms)
}

/// The source of `owner`'s running recording, if it has one.
pub fn active(owner: &str) -> Option<Source> {
    recordings().get(owner).map(|r| r.source)
}

/// Frames kept so far by `owner`'s recording.
pub fn frame_count(owner: &str) -> usize {
    recordings().get(owner).map_or(0, |r| r.frames.len())
}

/// Add a frame a tool already captured. Returns whether it was kept.
///
/// Refused when `owner` is not recording, or is recording from a different
/// source: the browser recorder must not quietly collect desktop screenshots.
pub fn push(owner: &str, source: Source, png: &[u8]) -> bool {
    // Decode and shrink outside the lock: it is the slow part.
    if active(owner) != Some(source) {
        return false;
    }
    let Some((small, hash)) = prepare(png) else {
        return false;
    };
    let mut map = recordings();
    let Some(r) = map.get_mut(owner).filter(|r| r.source == source) else {
        return false;
    };
    if r.started.elapsed() > MAX_DURATION {
        r.dropped += 1;
        return false;
    }
    if let Some(last) = r.frames.last_mut() {
        if last.hash == hash {
            last.hold_ms = (last.hold_ms + r.frame_ms).min(MAX_HOLD_MS);
            return true;
        }
    }
    if r.frames.len() >= MAX_FRAMES {
        r.dropped += 1;
        return false;
    }
    r.frames.push(Frame {
        png: small,
        hash,
        hold_ms: r.frame_ms,
    });
    true
}

/// Throw away every recording a task's agents have running. A run that ends
/// is not coming back to finish its clip.
pub fn discard_task(task_id: &str) {
    let prefix = format!("{task_id}:");
    recordings().retain(|k, _| k != task_id && !k.starts_with(&prefix));
}

/// A saved clip.
#[derive(Debug, Clone)]
pub struct Clip {
    pub path: PathBuf,
    pub frames: usize,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
    pub bytes: u64,
    pub dropped: usize,
}

impl Clip {
    /// What the agent is told: where it is and how to show it.
    pub fn describe(&self) -> String {
        let path = self.path.display().to_string().replace('\\', "/");
        let mut s = format!(
            "Saved a {}-frame proof clip ({}x{}, {:.1}s, {}) to {path}\nShow it in your reply or handoff with: ![proof clip]({path})",
            self.frames,
            self.width,
            self.height,
            self.duration_ms as f64 / 1000.0,
            super::tools::human_bytes(self.bytes)
        );
        if self.dropped > 0 {
            s.push_str(&format!(
                "\n{} later frame(s) were left out: a clip keeps at most {MAX_FRAMES} frames over {} minutes.",
                self.dropped,
                MAX_DURATION.as_secs() / 60
            ));
        }
        s
    }
}

/// Stop `owner`'s recording and save it as an animated PNG under `dir`.
pub fn stop(owner: &str, dir: &Path) -> Result<Clip, String> {
    let r = recordings()
        .remove(owner)
        .ok_or("Nothing is recording. Start one with `record_start` first.")?;
    if r.frames.is_empty() {
        return Err(
            "The recording has no frames: nothing was captured between start and stop.".into(),
        );
    }
    let frames: Vec<(Vec<u8>, u64)> = r.frames.into_iter().map(|f| (f.png, f.hold_ms)).collect();
    let (bytes, w, h) = encode_bounded(&frames)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let slug = slug(&r.title);
    let name = if slug.is_empty() {
        format!("clip-{stamp}-{}.png", short_id())
    } else {
        format!("clip-{stamp}-{slug}.png")
    };
    let path = dir.join(name);
    std::fs::write(&path, &bytes).map_err(|e| format!("Can't save the clip: {e}"))?;
    Ok(Clip {
        path,
        frames: frames.len(),
        width: w,
        height: h,
        duration_ms: frames.iter().map(|f| f.1).sum(),
        bytes: bytes.len() as u64,
        dropped: r.dropped,
    })
}

/// Decode a captured PNG, shrink it to `MAX_EDGE`, and hash its pixels.
fn prepare(png: &[u8]) -> Option<(Vec<u8>, [u8; 32])> {
    use sha2::{Digest, Sha256};
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    let img = fit(img, MAX_EDGE);
    let hash: [u8; 32] = Sha256::digest(img.as_raw()).into();
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).ok()?;
    Some((buf.into_inner(), hash))
}

/// Downscale so the longest edge is at most `max_edge`; never upscale.
fn fit(img: image::RgbaImage, max_edge: u32) -> image::RgbaImage {
    let (w, h) = img.dimensions();
    if w <= max_edge && h <= max_edge {
        return img;
    }
    let scale = max_edge as f64 / w.max(h) as f64;
    let nw = ((w as f64 * scale).round() as u32).max(1);
    let nh = ((h as f64 * scale).round() as u32).max(1);
    image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle)
}

/// Encode `frames` (PNG bytes, hold in ms), shrinking the whole clip until it
/// fits `MAX_CLIP_BYTES`. Returns the bytes and the canvas size.
pub fn encode_bounded(frames: &[(Vec<u8>, u64)]) -> Result<(Vec<u8>, u32, u32), String> {
    if frames.is_empty() {
        return Err("no frames to encode".into());
    }
    let decoded: Vec<(image::RgbaImage, u64)> = frames
        .iter()
        .take(MAX_FRAMES)
        .map(|(png, ms)| {
            image::load_from_memory(png)
                .map(|i| (i.to_rgba8(), *ms))
                .map_err(|e| format!("bad frame: {e}"))
        })
        .collect::<Result<_, _>>()?;
    // The first frame sets the canvas; every frame is fitted onto it, so a
    // full-page shot or a resized viewport mid-recording can't break the clip.
    let (w0, h0) = decoded[0].0.dimensions();
    let mut edge = w0.max(h0).min(MAX_EDGE);
    for _ in 0..6 {
        let scale = edge as f64 / w0.max(h0) as f64;
        let cw = ((w0 as f64 * scale).round() as u32).max(1);
        let ch = ((h0 as f64 * scale).round() as u32).max(1);
        let bytes = encode_apng(&decoded, cw, ch)?;
        if (bytes.len() as u64) <= MAX_CLIP_BYTES {
            return Ok((bytes, cw, ch));
        }
        edge = (edge as f64 * 0.7) as u32;
        if edge < 64 {
            break;
        }
    }
    Err(format!(
        "The clip is over the {} limit even when shrunk. Record fewer steps.",
        super::tools::human_bytes(MAX_CLIP_BYTES)
    ))
}

/// One APNG, every frame letterboxed onto a `cw`x`ch` canvas.
fn encode_apng(frames: &[(image::RgbaImage, u64)], cw: u32, ch: u32) -> Result<Vec<u8>, String> {
    let err = |e: png::EncodingError| format!("clip encode failed: {e}");
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, cw, ch);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Best);
        // 0 plays = loop forever, which is how a proof clip is watched.
        enc.set_animated(frames.len() as u32, 0).map_err(err)?;
        let mut wr = enc.write_header().map_err(err)?;
        for (img, ms) in frames {
            let canvas = letterbox(img, cw, ch);
            // Delay as a fraction of a second: ms/1000 fits u16 below 65.5s,
            // and holds are capped well under that.
            let ms = (*ms).clamp(1, MAX_HOLD_MS) as u16;
            wr.set_frame_delay(ms, 1000).map_err(err)?;
            wr.write_image_data(canvas.as_raw()).map_err(err)?;
        }
        wr.finish().map_err(err)?;
    }
    Ok(out)
}

/// Fit `img` inside `cw`x`ch`, centred on an opaque white background.
fn letterbox(img: &image::RgbaImage, cw: u32, ch: u32) -> image::RgbaImage {
    let (w, h) = img.dimensions();
    if (w, h) == (cw, ch) {
        return img.clone();
    }
    let scale = (cw as f64 / w as f64).min(ch as f64 / h as f64);
    let nw = ((w as f64 * scale).round() as u32).clamp(1, cw);
    let nh = ((h as f64 * scale).round() as u32).clamp(1, ch);
    let small = image::imageops::resize(img, nw, nh, image::imageops::FilterType::Triangle);
    let mut canvas = image::RgbaImage::from_pixel(cw, ch, image::Rgba([255, 255, 255, 255]));
    image::imageops::overlay(
        &mut canvas,
        &small,
        ((cw - nw) / 2) as i64,
        ((ch - nh) / 2) as i64,
    );
    canvas
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 40 {
            break;
        }
    }
    out.trim_end_matches('-').to_string()
}

fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// A task id as a single safe path component. Task ids are generated hex, but
/// this is a path built from an IPC argument, so it is never trusted to be.
fn safe_component(id: &str) -> String {
    let s: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect();
    if s.is_empty() {
        "_".into()
    } else {
        s
    }
}

/// Where a task's clips and visual-diff screenshots live.
pub fn task_dir(task_id: &str) -> PathBuf {
    super::store::data_dir()
        .join("proof")
        .join(safe_component(task_id))
}

/// Delete everything a task stored here (the chat was deleted).
pub fn forget_task(task_id: &str) {
    discard_task(task_id);
    let _ = std::fs::remove_dir_all(task_dir(task_id));
}

// ───────────────────────────── visual diff ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualEntry {
    pub url: String,
    /// Absolute path of the baseline PNG.
    pub before: String,
    /// Absolute path of the latest "after" PNG, once one was captured.
    pub after: Option<String>,
    pub before_at: String,
    pub after_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct VisualDiff {
    pub entries: Vec<VisualEntry>,
}

/// Only http(s) pages with a host. A visual diff is about the app being
/// built — normally a local dev server — and the Review button re-opens these
/// URLs on the user's click, so `file:`, `javascript:` and friends stay out.
pub fn validate_visual_url(url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url.trim()).map_err(|_| {
        format!("`{url}` is not a valid URL: give an http(s) URL such as http://localhost:5173/")
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("A visual diff only captures http(s) URLs (for example your dev server at http://localhost:5173/).".into());
    }
    Ok(parsed.to_string())
}

fn manifest(task_id: &str) -> PathBuf {
    task_dir(task_id).join("visual.json")
}

pub fn load_visual(task_id: &str) -> VisualDiff {
    std::fs::read_to_string(manifest(task_id))
        .ok()
        .and_then(|s| serde_json::from_str::<VisualDiff>(&s).ok())
        .map(|mut v| {
            // Paths come from our own file, but it sits on disk where anything
            // could edit it: only ever hand back files inside this task's dir.
            let dir = task_dir(task_id);
            let inside = |p: &str| Path::new(p).parent() == Some(dir.as_path());
            v.entries.retain(|e| inside(&e.before));
            for e in &mut v.entries {
                if e.after.as_deref().is_some_and(|a| !inside(a)) {
                    e.after = None;
                    e.after_at = None;
                }
            }
            v.entries.truncate(MAX_VISUAL_URLS);
            v
        })
        .unwrap_or_default()
}

fn save_visual(task_id: &str, v: &VisualDiff) -> Result<(), String> {
    let dir = task_dir(task_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;
    let json = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    std::fs::write(manifest(task_id), json).map_err(|e| format!("Can't save the visual diff: {e}"))
}

/// Write a screenshot as a new file. A fresh name each time on purpose: the
/// UI caches pictures by path, so overwriting in place would keep showing the
/// old capture after a recapture.
fn write_shot(task_id: &str, kind: &str, png: &[u8]) -> Result<String, String> {
    let img = image::load_from_memory(png)
        .map_err(|e| format!("bad screenshot: {e}"))?
        .to_rgba8();
    let img = fit(img, VISUAL_MAX_EDGE);
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| format!("can't encode the screenshot: {e}"))?;
    let bytes = buf.into_inner();
    if bytes.len() as u64 > super::tools::MAX_IMAGE_BYTES {
        return Err("The screenshot is over the image size limit.".into());
    }
    let dir = task_dir(task_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;
    let path = dir.join(format!(
        "{kind}-{}-{}.png",
        chrono::Utc::now().timestamp_millis(),
        short_id()
    ));
    std::fs::write(&path, bytes).map_err(|e| format!("Can't save the screenshot: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

fn remove_file(p: Option<&str>) {
    if let Some(p) = p {
        let _ = std::fs::remove_file(p);
    }
}

/// Record (or replace) the baseline for `url`. Replacing a baseline drops its
/// old "after", which was a comparison against a different "before".
pub fn save_baseline(task_id: &str, url: &str, png: &[u8]) -> Result<VisualEntry, String> {
    let url = validate_visual_url(url)?;
    let mut v = load_visual(task_id);
    if !v.entries.iter().any(|e| e.url == url) && v.entries.len() >= MAX_VISUAL_URLS {
        return Err(format!(
            "A chat's visual diff holds at most {MAX_VISUAL_URLS} URLs. Re-baseline one of: {}",
            v.entries
                .iter()
                .map(|e| e.url.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let before = write_shot(task_id, "before", png)?;
    let entry = VisualEntry {
        url: url.clone(),
        before,
        after: None,
        before_at: chrono::Utc::now().to_rfc3339(),
        after_at: None,
    };
    if let Some(old) = v.entries.iter_mut().find(|e| e.url == url) {
        remove_file(Some(&old.before));
        remove_file(old.after.as_deref());
        *old = entry.clone();
    } else {
        v.entries.push(entry.clone());
    }
    save_visual(task_id, &v)?;
    Ok(entry)
}

/// Record the "after" for a URL that has a baseline.
pub fn save_after(task_id: &str, url: &str, png: &[u8]) -> Result<VisualEntry, String> {
    let url = validate_visual_url(url)?;
    let mut v = load_visual(task_id);
    let Some(i) = v.entries.iter().position(|e| e.url == url) else {
        return Err(format!(
            "There is no baseline for {url}. Capture one with `baseline` before changing the UI."
        ));
    };
    let after = write_shot(task_id, "after", png)?;
    let e = &mut v.entries[i];
    remove_file(e.after.as_deref());
    e.after = Some(after);
    e.after_at = Some(chrono::Utc::now().to_rfc3339());
    let entry = e.clone();
    save_visual(task_id, &v)?;
    Ok(entry)
}

/// Discard a chat's visual diff and its screenshots (clips are left alone).
pub fn clear_visual(task_id: &str) {
    let v = load_visual(task_id);
    for e in &v.entries {
        remove_file(Some(&e.before));
        remove_file(e.after.as_deref());
    }
    let _ = std::fs::remove_file(manifest(task_id));
}

/// A ready-to-paste `openleash-image` fence comparing one entry, for the agent.
pub fn fence(e: &VisualEntry) -> String {
    let spec = serde_json::json!({
        "title": e.url,
        "src": e.before.replace('\\', "/"),
        "after": e.after.as_deref().map(|a| a.replace('\\', "/")),
    });
    format!("```openleash-image\n{spec}\n```")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_of(w: u32, h: u32, shade: u8) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([shade, (x % 256) as u8, (y % 256) as u8, 255])
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    /// Counts `fcTL` chunks: one per animation frame.
    fn apng_frames(bytes: &[u8]) -> (u32, usize) {
        let mut i = 8;
        let (mut declared, mut fctl) = (0, 0);
        while i + 8 <= bytes.len() {
            let len = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
            let kind = &bytes[i + 4..i + 8];
            if kind == b"acTL" {
                declared = u32::from_be_bytes(bytes[i + 8..i + 12].try_into().unwrap());
            }
            if kind == b"fcTL" {
                fctl += 1;
            }
            i += 12 + len;
        }
        (declared, fctl)
    }

    fn owner(tag: &str) -> String {
        format!("proof-test-{tag}-{}:main", short_id())
    }

    #[test]
    fn encodes_an_animated_png_the_image_pipeline_accepts() {
        let frames = vec![
            (png_of(320, 200, 10), 500),
            (png_of(320, 200, 200), 700),
            // A different size is letterboxed onto the first frame's canvas.
            (png_of(200, 300, 90), 500),
        ];
        let (bytes, w, h) = encode_bounded(&frames).unwrap();
        assert_eq!((w, h), (320, 200));
        assert_eq!(
            super::super::tools::sniff_media_type(&bytes),
            Some("image/png")
        );
        assert_eq!(apng_frames(&bytes), (3, 3));
        // Still a valid PNG for a decoder that ignores the animation.
        let first = image::load_from_memory(&bytes).unwrap();
        assert_eq!((first.width(), first.height()), (320, 200));
    }

    #[test]
    fn frames_are_bounded_in_size_count_and_source() {
        let o = owner("bounds");
        assert_eq!(
            start(&o, Source::Browser, Some(1), "x").unwrap(),
            MIN_FRAME_MS
        );
        // A second start would throw away the first one's frames.
        assert!(start(&o, Source::Browser, None, "x").is_err());
        // A desktop capture can never land in a browser recording.
        assert!(!push(&o, Source::Screen, &png_of(64, 64, 1)));
        assert_eq!(frame_count(&o), 0);
        // An oversized frame is shrunk to the edge cap.
        assert!(push(&o, Source::Browser, &png_of(2400, 1200, 0)));
        // An identical frame extends the last one instead of adding another.
        assert!(push(&o, Source::Browser, &png_of(2400, 1200, 0)));
        assert_eq!(frame_count(&o), 1);
        for i in 1..(MAX_FRAMES as u32 + 5) {
            push(&o, Source::Browser, &png_of(48, 48, i as u8));
        }
        assert_eq!(frame_count(&o), MAX_FRAMES);
        // Garbage is refused, not a panic.
        assert!(!push(&o, Source::Browser, b"not a png"));
        // Another chat's recording is untouched by this one.
        let other = owner("other");
        assert!(!push(&other, Source::Browser, &png_of(48, 48, 3)));

        let dir = std::env::temp_dir().join(format!("ol-proof-{}", short_id()));
        let clip = stop(&o, &dir).unwrap();
        assert_eq!(clip.frames, MAX_FRAMES);
        assert!(clip.dropped >= 4, "dropped {}", clip.dropped);
        assert!(clip.width <= MAX_EDGE && clip.height <= MAX_EDGE);
        assert_eq!((clip.width, clip.height), (MAX_EDGE, MAX_EDGE / 2));
        assert!(clip.bytes <= MAX_CLIP_BYTES);
        assert!(clip.path.starts_with(&dir));
        let bytes = std::fs::read(&clip.path).unwrap();
        assert_eq!(apng_frames(&bytes).0 as usize, MAX_FRAMES);
        assert!(clip.describe().contains("![proof clip]("));
        // Stopped means stopped.
        assert!(active(&o).is_none());
        assert!(stop(&o, &dir).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_empty_recording_is_an_error_and_a_task_end_discards_recordings() {
        let o = owner("empty");
        start(&o, Source::Screen, None, "").unwrap();
        let dir = std::env::temp_dir().join(format!("ol-proof-{}", short_id()));
        assert!(stop(&o, &dir).unwrap_err().contains("no frames"));
        let o2 = owner("discard");
        start(&o2, Source::Screen, None, "").unwrap();
        let task = o2.split(':').next().unwrap().to_string();
        discard_task(&task);
        assert!(active(&o2).is_none());
    }

    #[test]
    fn an_oversized_clip_is_shrunk_until_it_fits() {
        // Noise compresses badly: 40 frames of it at full size is far over the
        // inline limit, so the encoder must shrink rather than fail or overrun.
        let noise = |seed: u32| {
            let mut s = seed.wrapping_mul(2654435761) | 1;
            let img = image::RgbaImage::from_fn(MAX_EDGE, MAX_EDGE * 3 / 4, |_, _| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                let b = s.to_le_bytes();
                image::Rgba([b[0], b[1], b[2], 255])
            });
            let mut buf = std::io::Cursor::new(Vec::new());
            img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
            (buf.into_inner(), 500u64)
        };
        let frames: Vec<_> = (0..12).map(noise).collect();
        let (bytes, w, _) = encode_bounded(&frames).unwrap();
        assert!(bytes.len() as u64 <= MAX_CLIP_BYTES);
        assert!(w < MAX_EDGE, "should have been shrunk, got width {w}");
    }

    #[test]
    fn visual_diff_round_trip_and_url_rules() {
        let home = std::env::temp_dir().join(format!("ol-proof-home-{}", short_id()));
        let _g = super::super::store::test_home(&home);
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "about:blank",
            "nope",
        ] {
            assert!(validate_visual_url(bad).is_err(), "{bad}");
        }
        let t = "visualtask";
        assert!(save_after(t, "http://localhost:5173/", &png_of(40, 30, 1)).is_err());
        let b = save_baseline(t, "http://localhost:5173/", &png_of(40, 30, 1)).unwrap();
        assert!(Path::new(&b.before).is_file());
        let a = save_after(t, "http://localhost:5173/", &png_of(40, 30, 2)).unwrap();
        let first_after = a.after.clone().unwrap();
        // A recapture writes a new file (the UI caches by path) and drops the old.
        let a2 = save_after(t, "http://localhost:5173/", &png_of(40, 30, 3)).unwrap();
        assert_ne!(a2.after, Some(first_after.clone()));
        assert!(!Path::new(&first_after).exists());
        let v = load_visual(t);
        assert_eq!(v.entries.len(), 1);
        assert_eq!(v.entries[0].after, a2.after);
        assert!(fence(&v.entries[0]).starts_with("```openleash-image\n"));

        // A tampered manifest can't point the UI at files outside the task dir.
        let mut evil = v.clone();
        evil.entries[0].after = Some("C:/Windows/win.ini".into());
        evil.entries.push(VisualEntry {
            url: "http://x/".into(),
            before: "/etc/passwd".into(),
            after: None,
            before_at: String::new(),
            after_at: None,
        });
        save_visual(t, &evil).unwrap();
        let v = load_visual(t);
        assert_eq!(v.entries.len(), 1);
        assert!(v.entries[0].after.is_none());

        // Bounded number of URLs.
        for i in 1..MAX_VISUAL_URLS {
            save_baseline(
                t,
                &format!("http://localhost:{}/", 3000 + i),
                &png_of(8, 8, 1),
            )
            .unwrap();
        }
        assert!(save_baseline(t, "http://localhost:9999/", &png_of(8, 8, 1)).is_err());

        clear_visual(t);
        assert!(load_visual(t).entries.is_empty());
        forget_task(t);
        assert!(!task_dir(t).exists());
        let _ = std::fs::remove_dir_all(home);
    }
}
