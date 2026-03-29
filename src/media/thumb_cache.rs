use anyhow::Result;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use gtk4::cairo;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc;

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
    /// When true, no new extraction threads are started (during active playback).
    paused: bool,
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
            paused: false,
        }
    }

    /// Pause / resume background extraction. When paused (during playback),
    /// pending requests are queued but no new threads are spawned until resumed.
    pub fn set_extraction_paused(&mut self, paused: bool) {
        self.paused = paused;
        if !paused {
            self.flush_pending();
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
            self.pending
                .push_back((key, source_path.to_string(), time_ns));
            self.flush_pending();
        }
        false
    }

    /// Drain completed background frames → convert to Cairo surfaces.
    /// Returns `true` if at least one new surface was added (caller should redraw).
    pub fn poll(&mut self) -> bool {
        !self.poll_ready_keys().is_empty()
    }

    /// Drain completed frames and return cache keys that became available.
    pub fn poll_ready_keys(&mut self) -> Vec<String> {
        let mut ready = Vec::new();
        while let Ok(frame) = self.rx.try_recv() {
            self.loading.remove(&frame.key);
            self.in_flight = self.in_flight.saturating_sub(1);
            if frame.data.is_empty() {
                continue;
            }
            if let Ok(surf) = rgba_to_surface(&frame.data) {
                self.surfaces.insert(frame.key.clone(), surf);
                ready.push(frame.key);
            }
        }
        self.flush_pending();
        ready
    }

    /// Returns the cached surface for `source_path` at `time_ns`, if available.
    pub fn get(&self, source_path: &str, time_ns: u64) -> Option<&cairo::ImageSurface> {
        self.surfaces.get(&cache_key(source_path, time_ns))
    }

    /// Spawn pending extraction threads up to the concurrency limit.
    fn flush_pending(&mut self) {
        if self.paused {
            return;
        }
        while self.in_flight < MAX_CONCURRENT {
            if let Some((key, path, time_ns)) = self.pending.pop_front() {
                self.in_flight += 1;
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let data = extract_rgba(path, time_ns).unwrap_or_else(|e| {
                        eprintln!("[thumb] error: {e}");
                        Vec::new()
                    });
                    let _ = tx.send(RawFrame { key, data });
                });
            } else {
                break;
            }
        }
    }
}

/// Quantize sample times into 2-second buckets to avoid excessive unique
/// extraction jobs while preserving useful visual coverage.
fn cache_key(source_path: &str, time_ns: u64) -> String {
    let sec = time_ns / 1_000_000_000;
    let sec = (sec / 2) * 2;
    format!("{source_path}@{sec}s")
}

/// Extract a single RGBA frame from `source_path` at `time_ns` in a background thread.
fn extract_rgba(source_path: String, time_ns: u64) -> Result<Vec<u8>> {
    gst::init()?;

    let uri = crate::media::thumbnail::path_to_uri(&source_path);
    let is_image = crate::model::clip::is_image_file(&source_path);

    // For still images, insert imagefreeze so the single decoded frame becomes
    // a continuous stream that pull_sample() can always grab from.
    let freeze = if is_image { "imagefreeze ! " } else { "" };
    // Connect the secondary (audio / metadata) pads from uridecodebin to a
    // fakesink so the multiqueue never stalls waiting for a consumer.  Without
    // this, a video+audio file blocks during PAUSED preroll because the audio
    // pad is unlinked and the multiqueue fills up, starving the video path.
    let pipeline_desc = format!(
        "uridecodebin name=dec uri=\"{uri}\" \
         dec. ! {freeze}videoconvert ! videoscale ! \
         video/x-raw,format=RGBA,width={THUMB_W},height={THUMB_H} ! \
         appsink name=sink sync=false max-buffers=1 drop=false \
         dec. ! fakesink sync=false"
    );

    let guard = super::PipelineGuard(
        gst::parse::launch(&pipeline_desc)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow::anyhow!("not a pipeline"))?,
    );
    let pipeline = &guard.0;

    let appsink = pipeline
        .by_name("sink")
        .ok_or_else(|| anyhow::anyhow!("no appsink"))?
        .downcast::<AppSink>()
        .map_err(|_| anyhow::anyhow!("not appsink"))?;

    if let Some(dec) = pipeline
        .by_name("dec")
        .and_then(|e| e.dynamic_cast::<gst::Bin>().ok())
    {
        dec.connect_element_added(|_, element| {
            tune_decoder_threads(element);
        });
    }

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

    let sample = appsink
        .try_pull_sample(gst::ClockTime::from_seconds(8))
        .ok_or_else(|| anyhow::anyhow!("pull_sample timed out"))?;

    let buffer = sample
        .buffer()
        .ok_or_else(|| anyhow::anyhow!("no buffer"))?;
    let map = buffer.map_readable()?;
    let data = map.as_slice().to_vec();
    drop(map);

    // PipelineGuard ensures pipeline is set to Null when this function returns.
    Ok(data)
}

fn tune_decoder_threads(element: &gst::Element) {
    if element.find_property("max-threads").is_some() {
        element.set_property_from_str("max-threads", "1");
    }
    if element.find_property("threads").is_some() {
        element.set_property_from_str("threads", "1");
    }
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
        let mut buf = surface
            .data()
            .map_err(|_| anyhow::anyhow!("surface data error"))?;
        for row in 0..THUMB_H as usize {
            let src_row = row * THUMB_W as usize * 4;
            let dst_row = row * stride;
            for col in 0..THUMB_W as usize {
                let s = src_row + col * 4;
                let d = dst_row + col * 4;
                if s + 3 < rgba.len() && d + 3 < buf.len() {
                    buf[d] = rgba[s + 2]; // B
                    buf[d + 1] = rgba[s + 1]; // G
                    buf[d + 2] = rgba[s]; // R
                    buf[d + 3] = rgba[s + 3]; // A
                }
            }
        }
    }

    Ok(surface)
}
