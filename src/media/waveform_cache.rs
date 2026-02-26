/// Asynchronous audio waveform peak cache.
///
/// Background threads decode audio via GStreamer and compute normalized
/// peak amplitude at PEAKS_PER_SEC resolution. Main thread draws from these peaks.
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;

/// Number of peak samples stored per second of audio (time-axis resolution).
pub const PEAKS_PER_SEC: f64 = 100.0; // one peak per 10 ms

struct RawPeaks {
    key: String,
    peaks: Vec<f32>, // normalized 0.0..=1.0
}

pub struct WaveformCache {
    pub data: HashMap<String, Vec<f32>>,
    loading: HashSet<String>,
    tx: mpsc::SyncSender<RawPeaks>,
    rx: mpsc::Receiver<RawPeaks>,
}

impl WaveformCache {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(8);
        Self {
            data: HashMap::new(),
            loading: HashSet::new(),
            tx,
            rx,
        }
    }

    /// Request waveform data for `source_path`. No-op if already cached or pending.
    pub fn request(&mut self, source_path: &str) {
        if self.data.contains_key(source_path) || self.loading.contains(source_path) {
            return;
        }
        // Limit concurrent extraction threads to avoid starving playback.
        if self.loading.len() >= 2 {
            return; // will be re-requested on next timeline draw cycle
        }
        self.loading.insert(source_path.to_string());
        let key = source_path.to_string();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            if let Some(peaks) = extract_peaks(&key) {
                let _ = tx.send(RawPeaks { key, peaks });
            }
        });
    }

    /// Drain completed background results. Call this periodically on the main thread.
    pub fn poll(&mut self) {
        while let Ok(raw) = self.rx.try_recv() {
            self.loading.remove(&raw.key);
            self.data.insert(raw.key, raw.peaks);
        }
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
        let all = self.data.get(source_path)?;
        if all.is_empty() || pixel_width == 0 {
            return None;
        }

        let in_sec  = source_in_ns  as f64 / 1_000_000_000.0;
        let out_sec = source_out_ns as f64 / 1_000_000_000.0;
        let dur_sec = (out_sec - in_sec).max(0.001);

        let peaks_per_px = (dur_sec * PEAKS_PER_SEC) / pixel_width as f64;

        let mut result = Vec::with_capacity(pixel_width);
        for px in 0..pixel_width {
            let start_peak = (in_sec * PEAKS_PER_SEC + px as f64 * peaks_per_px) as usize;
            let end_peak   = (in_sec * PEAKS_PER_SEC + (px + 1) as f64 * peaks_per_px) as usize;
            let end_peak   = end_peak.max(start_peak + 1);

            let mut max = 0.0f32;
            for i in start_peak..end_peak {
                if let Some(&v) = all.get(i) {
                    if v > max { max = v; }
                }
            }
            result.push(max);
        }
        Some(result)
    }
}

// ── Background extraction ──────────────────────────────────────────────────

fn extract_peaks(source_path: &str) -> Option<Vec<f32>> {
    let uri = if source_path.starts_with("file://") {
        source_path.to_string()
    } else {
        format!("file://{source_path}")
    };

    // Decode audio → F32LE mono via appsink
    let pipeline = gst::Pipeline::new();

    let src  = gst::ElementFactory::make("uridecodebin").property("uri", &uri).build().ok()?;
    let conv = gst::ElementFactory::make("audioconvert").build().ok()?;
    let resample = gst::ElementFactory::make("audioresample").build().ok()?;
    let capsf = gst::ElementFactory::make("capsfilter").build().ok()?;
    let sink  = gst::ElementFactory::make("appsink").build().ok()?;

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

    pipeline.add_many([&src, &conv, &resample, &capsf, &sink]).ok()?;
    gst::Element::link_many([&conv, &resample, &capsf, &sink]).ok()?;

    // uridecodebin pads are dynamic — connect when an audio pad appears
    {
        let conv = conv.clone();
        src.connect_pad_added(move |_, pad| {
            let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
            let name = caps.structure(0).map(|s| s.name()).unwrap_or_default();
            if name.starts_with("audio/") {
                let sink_pad = conv.static_pad("sink").unwrap();
                if sink_pad.is_linked() { return; }
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
                let peak = floats[i..end].iter().map(|v| v.abs()).fold(0.0f32, f32::max);
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
        if appsink.is_eos() { break; }
    }

    let _ = pipeline.set_state(gst::State::Null);

    if samples.is_empty() { return None; }

    // Normalize to 0..=1
    let max = samples.iter().cloned().fold(0.0f32, f32::max);
    if max > 0.0 {
        for v in &mut samples { *v /= max; }
    }

    Some(samples)
}
