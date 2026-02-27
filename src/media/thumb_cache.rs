use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc;
use anyhow::Result;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use gtk4::cairo;

const THUMB_W: i32 = 160;
const THUMB_H: i32 = 90;

/// Maximum number of simultaneous thumbnail extraction threads.
const MAX_CONCURRENT: usize = 4;

/// A loaded thumbnail ready to draw.
struct RawFrame {
    key: String,
    data: Vec<u8>, // RGBA bytes, THUMB_W * THUMB_H * 4
}

/// Asynchronous thumbnail cache.
///
/// Usage pattern (all on GTK main thread):
/// 1. Call `request(path, time_ns)` in the draw func — queues extraction
///    (up to MAX_CONCURRENT run in parallel).
/// 2. Call `poll()` periodically (e.g., in the 100 ms redraw timer) to drain
///    completed frames and convert them to Cairo surfaces.
/// 3. Call `get(path, time_ns)` in the draw func to obtain a surface.
pub struct ThumbnailCache {
    pub surfaces: HashMap<String, cairo::ImageSurface>,
    loading: HashSet<String>,
    pending: VecDeque<(String, String, u64)>, // (key, source_path, time_ns)
    in_flight: usize,
    tx: mpsc::SyncSender<RawFrame>,
    rx: mpsc::Receiver<RawFrame>,
}

impl ThumbnailCache {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(32);
        Self {
            surfaces: HashMap::new(),
            loading: HashSet::new(),
            pending: VecDeque::new(),
            in_flight: 0,
            tx,
            rx,
        }
    }

    /// Request a thumbnail for `source_path` at `time_ns`.
    /// Queues extraction; spawns up to MAX_CONCURRENT background threads.
    /// Returns `true` if the thumbnail is already cached.
    pub fn request(&mut self, source_path: &str, time_ns: u64) -> bool {
        let key = cache_key(source_path, time_ns);
        if self.surfaces.contains_key(&key) {
            return true;
        }
        if !self.loading.contains(&key) {
            self.loading.insert(key.clone());
            self.pending.push_back((key, source_path.to_string(), time_ns));
            self.flush_pending();
        }
        false
    }

    /// Drain completed background frames → convert to Cairo surfaces.
    /// Returns `true` if at least one new surface was added (caller should redraw).
    pub fn poll(&mut self) -> bool {
        let mut dirty = false;
        while let Ok(frame) = self.rx.try_recv() {
            self.loading.remove(&frame.key);
            self.in_flight = self.in_flight.saturating_sub(1);
            if let Ok(surf) = rgba_to_surface(&frame.data) {
                self.surfaces.insert(frame.key, surf);
                dirty = true;
            }
        }
        self.flush_pending();
        dirty
    }

    /// Returns the cached surface for `source_path` at `time_ns`, if available.
    pub fn get(&self, source_path: &str, time_ns: u64) -> Option<&cairo::ImageSurface> {
        self.surfaces.get(&cache_key(source_path, time_ns))
    }

    /// Spawn pending extraction threads up to the concurrency limit.
    fn flush_pending(&mut self) {
        while self.in_flight < MAX_CONCURRENT {
            if let Some((key, path, time_ns)) = self.pending.pop_front() {
                self.in_flight += 1;
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    match extract_rgba(path, time_ns) {
                        Ok(data) => { let _ = tx.send(RawFrame { key, data }); }
                        Err(e)   => { eprintln!("[thumb] error: {e}"); }
                    }
                });
            } else {
                break;
            }
        }
    }
}

/// Round time to the nearest second to avoid redundant extractions for nearby seeks.
fn cache_key(source_path: &str, time_ns: u64) -> String {
    let sec = time_ns / 1_000_000_000;
    format!("{source_path}@{sec}s")
}

/// Extract a single RGBA frame from `source_path` at `time_ns` in a background thread.
fn extract_rgba(source_path: String, time_ns: u64) -> Result<Vec<u8>> {
    gst::init()?;

    let uri = crate::media::thumbnail::path_to_uri(&source_path);

    let pipeline_desc = format!(
        "uridecodebin uri=\"{uri}\" ! videoconvert ! videoscale ! \
         video/x-raw,format=RGBA,width={THUMB_W},height={THUMB_H} ! \
         appsink name=sink sync=false max-buffers=1 drop=false"
    );

    let pipeline = gst::parse::launch(&pipeline_desc)?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow::anyhow!("not a pipeline"))?;

    let appsink = pipeline
        .by_name("sink")
        .ok_or_else(|| anyhow::anyhow!("no appsink"))?
        .downcast::<AppSink>()
        .map_err(|_| anyhow::anyhow!("not appsink"))?;

    pipeline.set_state(gst::State::Paused)?;
    // Wait for pre-roll (up to 5 s)
    let _ = pipeline.state(Some(gst::ClockTime::from_seconds(5)));

    if time_ns > 0 {
        let _ = pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::from_nseconds(time_ns),
        );
        let _ = pipeline.state(Some(gst::ClockTime::from_seconds(3)));
    }

    let _ = pipeline.set_state(gst::State::Playing);

    let sample = appsink.pull_sample()
        .map_err(|_| anyhow::anyhow!("pull_sample failed"))?;

    let buffer = sample.buffer().ok_or_else(|| anyhow::anyhow!("no buffer"))?;
    let map = buffer.map_readable()?;
    let data = map.as_slice().to_vec();
    drop(map);

    let _ = pipeline.set_state(gst::State::Null);

    Ok(data)
}

/// Convert raw RGBA bytes to a Cairo ARGB32 `ImageSurface`.
///
/// Cairo ARGB32 on little-endian stores pixels as [B, G, R, A].
fn rgba_to_surface(rgba: &[u8]) -> Result<cairo::ImageSurface> {
    let stride = cairo::Format::ARgb32
        .stride_for_width(THUMB_W as u32)
        .map_err(|_| anyhow::anyhow!("stride error"))? as usize;

    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, THUMB_W, THUMB_H)
        .map_err(|_| anyhow::anyhow!("surface create failed"))?;

    {
        let mut buf = surface.data().map_err(|_| anyhow::anyhow!("surface data error"))?;
        for row in 0..THUMB_H as usize {
            let src_row = row * THUMB_W as usize * 4;
            let dst_row = row * stride;
            for col in 0..THUMB_W as usize {
                let s = src_row + col * 4;
                let d = dst_row + col * 4;
                if s + 3 < rgba.len() && d + 3 < buf.len() {
                    buf[d]     = rgba[s + 2]; // B
                    buf[d + 1] = rgba[s + 1]; // G
                    buf[d + 2] = rgba[s];     // R
                    buf[d + 3] = rgba[s + 3]; // A
                }
            }
        }
    }

    Ok(surface)
}
