use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
/// Asynchronous audio waveform peak cache.
///
/// Background threads decode audio via GStreamer and compute normalized
/// peak amplitude at PEAKS_PER_SEC resolution. Main thread draws from these peaks.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc;

/// Number of peak samples stored per second of audio (time-axis resolution).
pub const PEAKS_PER_SEC: f64 = 100.0; // one peak per 10 ms

/// Maximum number of simultaneous waveform extraction threads.
const MAX_CONCURRENT: usize = 4;

struct RawPeaks {
    key: String,
    peaks: Vec<f32>, // normalized 0.0..=1.0
}

type WaveformLevels = Vec<Vec<f32>>;

pub struct WaveformCache {
    pub data: HashMap<String, Vec<f32>>,
    /// Coarser max-pooled summaries built from `data`; level N aggregates
    /// 2^(N+1) raw peaks so zoomed-out draws can avoid rescanning raw samples.
    lod_levels: HashMap<String, WaveformLevels>,
    loading: HashSet<String>,
    pending: VecDeque<String>,
    in_flight: usize,
    tx: mpsc::SyncSender<RawPeaks>,
    rx: mpsc::Receiver<RawPeaks>,
    /// When true, no new extraction threads are started (during active playback).
    paused: bool,
}

impl WaveformCache {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(8);
        Self {
            data: HashMap::new(),
            lod_levels: HashMap::new(),
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

    /// Request waveform data for `source_path`. No-op if already cached or pending.
    pub fn request(&mut self, source_path: &str) {
        if self.data.contains_key(source_path) || self.loading.contains(source_path) {
            return;
        }
        self.loading.insert(source_path.to_string());
        self.pending.push_back(source_path.to_string());
        self.flush_pending();
    }

    /// Drain completed background results. Call this periodically on the main thread.
    /// Returns true when new waveform data became available.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(raw) = self.rx.try_recv() {
            self.loading.remove(&raw.key);
            self.in_flight = self.in_flight.saturating_sub(1);
            if !raw.peaks.is_empty() {
                self.lod_levels
                    .insert(raw.key.clone(), build_lod_levels(&raw.peaks));
                self.data.insert(raw.key, raw.peaks);
                changed = true;
            }
        }
        self.flush_pending();
        changed
    }

    /// Get peak slice for the clip's marked region, downsampled to `pixel_width` columns.
    /// Returns None if the waveform isn't ready yet.
    pub fn get_peaks(
        &self,
        source_path: &str,
        source_in_ns: u64,
        source_out_ns: u64,
        pixel_width: usize,
    ) -> Option<Vec<f32>> {
        let raw = self.data.get(source_path)?;
        let lod_levels = self
            .lod_levels
            .get(source_path)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        sample_cached_peaks(raw, lod_levels, source_in_ns, source_out_ns, pixel_width)
    }

    /// Spawn pending extraction threads up to the concurrency limit.
    fn flush_pending(&mut self) {
        if self.paused {
            return;
        }
        while self.in_flight < MAX_CONCURRENT {
            if let Some(key) = self.pending.pop_front() {
                self.in_flight += 1;
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let peaks = extract_peaks(&key).unwrap_or_default();
                    let _ = tx.send(RawPeaks { key, peaks });
                });
            } else {
                break;
            }
        }
    }
}

fn build_lod_levels(raw: &[f32]) -> WaveformLevels {
    let mut levels = Vec::new();
    let mut current = raw.to_vec();
    while current.len() > 1 {
        let mut next = Vec::with_capacity((current.len() + 1) / 2);
        for chunk in current.chunks(2) {
            next.push(chunk.iter().copied().fold(0.0f32, f32::max));
        }
        levels.push(next.clone());
        current = next;
    }
    levels
}

fn choose_lod_level(raw_peaks_per_px: f64, extra_levels: usize) -> usize {
    let mut level = 0usize;
    let mut peaks_per_px = raw_peaks_per_px.max(1.0);
    while level < extra_levels && peaks_per_px >= 2.0 {
        peaks_per_px *= 0.5;
        level += 1;
    }
    level
}

fn sample_cached_peaks(
    raw: &[f32],
    lod_levels: &[Vec<f32>],
    source_in_ns: u64,
    source_out_ns: u64,
    pixel_width: usize,
) -> Option<Vec<f32>> {
    if raw.is_empty() || pixel_width == 0 {
        return None;
    }
    let in_sec = source_in_ns as f64 / 1_000_000_000.0;
    let out_sec = source_out_ns as f64 / 1_000_000_000.0;
    let dur_sec = (out_sec - in_sec).max(0.001);
    let raw_peaks_per_px = (dur_sec * PEAKS_PER_SEC) / pixel_width as f64;
    let level = choose_lod_level(raw_peaks_per_px, lod_levels.len());
    if level == 0 {
        return Some(sample_peaks_from_level(
            raw,
            1.0,
            source_in_ns,
            source_out_ns,
            pixel_width,
        ));
    }
    let scale = (1usize << level) as f64;
    let level_data = lod_levels.get(level - 1)?;
    Some(sample_peaks_from_level(
        level_data,
        scale,
        source_in_ns,
        source_out_ns,
        pixel_width,
    ))
}

fn sample_peaks_from_level(
    level_data: &[f32],
    scale: f64,
    source_in_ns: u64,
    source_out_ns: u64,
    pixel_width: usize,
) -> Vec<f32> {
    let in_sec = source_in_ns as f64 / 1_000_000_000.0;
    let out_sec = source_out_ns as f64 / 1_000_000_000.0;
    let dur_sec = (out_sec - in_sec).max(0.001);
    let start_peak = (in_sec * PEAKS_PER_SEC) / scale;
    let peaks_per_px = (dur_sec * PEAKS_PER_SEC) / (pixel_width as f64 * scale);
    let mut result = Vec::with_capacity(pixel_width);
    for px in 0..pixel_width {
        let start = (start_peak + px as f64 * peaks_per_px) as usize;
        let end = (start_peak + (px + 1) as f64 * peaks_per_px).ceil() as usize;
        let start = start.min(level_data.len());
        let end = end.max(start + 1).min(level_data.len());
        let max = level_data[start..end]
            .iter()
            .copied()
            .fold(0.0f32, f32::max);
        result.push(max);
    }
    result
}

// ── Background extraction ──────────────────────────────────────────────────

fn extract_peaks(source_path: &str) -> Option<Vec<f32>> {
    let uri = if source_path.starts_with("file://") {
        source_path.to_string()
    } else {
        format!("file://{source_path}")
    };

    // Decode audio → F32LE mono via appsink
    let guard = super::PipelineGuard(gst::Pipeline::new());
    let pipeline = &guard.0;

    let src = gst::ElementFactory::make("uridecodebin")
        .property("uri", &uri)
        .build()
        .ok()?;
    let conv = gst::ElementFactory::make("audioconvert").build().ok()?;
    let resample = gst::ElementFactory::make("audioresample").build().ok()?;
    let capsf = gst::ElementFactory::make("capsfilter").build().ok()?;
    let sink = gst::ElementFactory::make("appsink").build().ok()?;

    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("channels", 1i32)
        .field("rate", 22050i32)
        .build();
    capsf.set_property("caps", &caps);

    let appsink = sink.clone().dynamic_cast::<AppSink>().ok()?;
    appsink.set_property("sync", false);
    appsink.set_property("max-buffers", 200u32);
    appsink.set_property("drop", false);
    appsink.set_property("emit-signals", false);

    if let Ok(src_bin) = src.clone().dynamic_cast::<gst::Bin>() {
        src_bin.connect_element_added(|_, element| {
            tune_decoder_threads(element);
        });
    }

    pipeline
        .add_many([&src, &conv, &resample, &capsf, &sink])
        .ok()?;
    gst::Element::link_many([&conv, &resample, &capsf, &sink]).ok()?;

    // uridecodebin pads are dynamic — connect when an audio pad appears
    {
        let conv = conv.clone();
        src.connect_pad_added(move |_, pad| {
            let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
            let name = caps.structure(0).map(|s| s.name()).unwrap_or_default();
            if name.starts_with("audio/") {
                let sink_pad = conv.static_pad("sink").unwrap();
                if sink_pad.is_linked() {
                    return;
                }
                let _ = pad.link(&sink_pad);
            }
        });
    }

    pipeline.set_state(gst::State::Playing).ok()?;

    let mut samples: Vec<f32> = Vec::new();
    let bus = pipeline.bus()?;

    loop {
        // Pull one buffer's worth of samples
        if let Some(s) = appsink.try_pull_sample(gst::ClockTime::from_mseconds(100)) {
            let buffer = s.buffer()?;
            let map = buffer.map_readable().ok()?;
            let raw_bytes = map.as_slice();
            // F32LE: 4 bytes per sample
            let floats: Vec<f32> = raw_bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();

            // Compute peaks at PEAKS_PER_SEC: group ~220 samples per peak at 22050 Hz / 100
            let chunk = (22050.0 / PEAKS_PER_SEC) as usize;
            let mut i = 0;
            while i < floats.len() {
                let end = (i + chunk).min(floats.len());
                let peak = floats[i..end]
                    .iter()
                    .map(|v| v.abs())
                    .fold(0.0f32, f32::max);
                samples.push(peak);
                i = end;
            }
        }

        // Check bus for EOS or error (non-blocking)
        if let Some(msg) = bus.pop() {
            use gst::MessageView;
            match msg.view() {
                MessageView::Eos(_) | MessageView::Error(_) => break,
                _ => {}
            }
        }

        // If no sample came in and pipeline reached EOS state, stop
        if appsink.is_eos() {
            break;
        }
    }

    // PipelineGuard ensures pipeline is set to Null when this function returns.

    if samples.is_empty() {
        return None;
    }

    // Normalize to 0..=1
    let max = samples.iter().cloned().fold(0.0f32, f32::max);
    if max > 0.0 {
        for v in &mut samples {
            *v /= max;
        }
    }

    Some(samples)
}

fn tune_decoder_threads(element: &gst::Element) {
    if element.find_property("max-threads").is_some() {
        element.set_property_from_str("max-threads", "1");
    }
    if element.find_property("threads").is_some() {
        element.set_property_from_str("threads", "1");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_lod_levels_max_pools_successive_levels() {
        let levels = build_lod_levels(&[0.1, 0.4, 0.3, 0.2, 0.8]);
        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0], vec![0.4, 0.3, 0.8]);
        assert_eq!(levels[1], vec![0.4, 0.8]);
        assert_eq!(levels[2], vec![0.8]);
    }

    #[test]
    fn choose_lod_level_prefers_near_one_bucket_per_pixel() {
        assert_eq!(choose_lod_level(0.8, 6), 0);
        assert_eq!(choose_lod_level(1.9, 6), 0);
        assert_eq!(choose_lod_level(2.0, 6), 1);
        assert_eq!(choose_lod_level(3.5, 6), 1);
        assert_eq!(choose_lod_level(16.0, 6), 4);
    }

    #[test]
    fn get_peaks_uses_lod_for_zoomed_out_ranges() {
        let raw = [
            0.2, 0.1, 0.05, 0.1, 0.2, 0.15, 0.1, 0.05, 0.2, 0.1, 0.05, 0.1, 0.2, 0.15, 0.1, 0.05,
            0.4, 0.3, 0.2, 0.1, 0.4, 0.35, 0.2, 0.1, 0.4, 0.3, 0.2, 0.1, 0.4, 0.35, 0.2, 0.1, 0.6,
            0.5, 0.3, 0.2, 0.6, 0.55, 0.3, 0.2, 0.6, 0.5, 0.3, 0.2, 0.6, 0.55, 0.3, 0.2, 0.9, 0.7,
            0.4, 0.2, 0.9, 0.8, 0.4, 0.2, 0.9, 0.7, 0.4, 0.2, 0.9, 0.8, 0.4, 0.2,
        ];
        let mut cache = WaveformCache::new();
        cache.data.insert("clip".into(), raw.to_vec());
        cache
            .lod_levels
            .insert("clip".into(), build_lod_levels(&raw));

        let peaks = cache.get_peaks("clip", 0, 640_000_000, 4).unwrap();

        assert_eq!(peaks, vec![0.2, 0.4, 0.6, 0.9]);
    }

    #[test]
    fn poll_reports_when_new_waveform_data_arrives() {
        let mut cache = WaveformCache::new();
        cache.loading.insert("clip".into());
        cache.in_flight = 1;
        cache
            .tx
            .send(RawPeaks {
                key: "clip".into(),
                peaks: vec![0.2, 0.5, 0.1],
            })
            .unwrap();

        assert!(cache.poll());
        assert_eq!(cache.data.get("clip"), Some(&vec![0.2, 0.5, 0.1]));
        assert_eq!(cache.in_flight, 0);
        assert!(!cache.loading.contains("clip"));
        assert!(!cache.poll());
    }
}
