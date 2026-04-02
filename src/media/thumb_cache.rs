use anyhow::Result;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use gtk4::cairo;
use std::collections::{HashMap, HashSet, VecDeque};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

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
    pending: VecDeque<(String, String, u64, Option<u64>)>, // (key, source_path, time_ns, animated_svg_duration_ns)
    in_flight: usize,
    tx: mpsc::SyncSender<RawFrame>,
    rx: mpsc::Receiver<RawFrame>,
    /// When true, no new extraction threads are started (during active playback).
    paused: bool,
    animated_svg_duration_cache: HashMap<String, Option<u64>>,
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
            animated_svg_duration_cache: HashMap::new(),
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
            let animated_svg_duration_ns = if crate::model::clip::is_svg_file(source_path) {
                if let Some(cached) = self.animated_svg_duration_cache.get(source_path) {
                    *cached
                } else {
                    let analyzed = crate::media::animated_svg::analyze_svg_path(source_path)
                        .ok()
                        .and_then(|analysis| analysis.is_animated.then_some(analysis.duration_ns))
                        .flatten();
                    self.animated_svg_duration_cache
                        .insert(source_path.to_string(), analyzed);
                    analyzed
                }
            } else {
                None
            };
            self.loading.insert(key.clone());
            self.pending
                .push_back((key, source_path.to_string(), time_ns, animated_svg_duration_ns));
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
            if let Some((key, path, time_ns, animated_svg_duration_ns)) = self.pending.pop_front() {
                self.in_flight += 1;
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    // For image files, GStreamer is required (imagefreeze).
                    // For video files, go straight to ffmpeg: it is faster
                    // and handles multi-stream files (HEVC + audio + GPS)
                    // without GStreamer multiqueue stall risks.
                    let is_image = crate::model::clip::is_image_file(&path);
                    let data = if let Some(duration_ns) = animated_svg_duration_ns {
                        let clamped_time_ns = if duration_ns > 0 {
                            time_ns.min(duration_ns.saturating_sub(1))
                        } else {
                            0
                        };
                        extract_animated_svg_rgba(&path, clamped_time_ns).unwrap_or_else(|e| {
                            eprintln!("[thumb] animated svg render failed: {e}");
                            Vec::new()
                        })
                    } else if is_image {
                        extract_rgba(path.clone(), time_ns)
                            .or_else(|gst_err| {
                                eprintln!("[thumb] gstreamer extraction failed: {gst_err}");
                                extract_rgba_ffmpeg(&path, time_ns)
                            })
                            .unwrap_or_else(|e| {
                                eprintln!("[thumb] ffmpeg fallback failed: {e}");
                                Vec::new()
                            })
                    } else {
                        extract_rgba_ffmpeg(&path, time_ns)
                            .or_else(|ffmpeg_err| {
                                eprintln!("[thumb] ffmpeg extraction failed: {ffmpeg_err}");
                                extract_rgba(path.clone(), time_ns)
                            })
                            .unwrap_or_else(|e| {
                                eprintln!("[thumb] gstreamer fallback failed: {e}");
                                Vec::new()
                            })
                    };
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

    // Build the pipeline programmatically to reliably handle multi-stream files
    // (e.g. GoPro HEVC which has video + audio + GPS metadata pads).
    //
    // parse::launch with `dec. ! videoconvert dec. ! fakesink` is ambiguous
    // for uridecodebin's dynamic pads: depending on the GStreamer version the
    // audio pad may steal the videoconvert slot and leave the video path
    // unlinked, causing the multiqueue to fill and stall preroll forever.
    //
    // Instead: connect pad-added on uridecodebin and route video pads to the
    // thumbnail chain, all other pads to a fakesink.  This guarantees every
    // pad is consumed and preroll can always complete.
    let pipeline = gst::Pipeline::new();

    let uridec_video_caps = gst::Caps::builder("video/x-raw").build();
    let uridec = gst::ElementFactory::make("uridecodebin")
        .property("uri", &uri)
        .property("caps", &uridec_video_caps)
        .build()
        .map_err(|e| anyhow::anyhow!("uridecodebin: {e}"))?;

    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|e| anyhow::anyhow!("videoconvert: {e}"))?;

    let maybe_freeze: Option<gst::Element> = if is_image {
        Some(
            gst::ElementFactory::make("imagefreeze")
                .build()
                .map_err(|e| anyhow::anyhow!("imagefreeze: {e}"))?,
        )
    } else {
        None
    };

    let scale = gst::ElementFactory::make("videoscale")
        .build()
        .map_err(|e| anyhow::anyhow!("videoscale: {e}"))?;

    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGBA")
        .field("width", THUMB_W)
        .field("height", THUMB_H)
        .build();
    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()
        .map_err(|e| anyhow::anyhow!("capsfilter: {e}"))?;

    let appsink = gst::ElementFactory::make("appsink")
        .property("sync", false)
        .property("max-buffers", 1u32)
        .property("drop", false)
        .build()
        .map_err(|e| anyhow::anyhow!("appsink: {e}"))?
        .downcast::<AppSink>()
        .map_err(|_| anyhow::anyhow!("appsink downcast"))?;

    // Add all static elements to the pipeline.
    let mut static_elements: Vec<&gst::Element> = vec![&uridec, &convert, &scale, &capsfilter];
    if let Some(ref f) = maybe_freeze {
        static_elements.push(f);
    }
    static_elements.push(appsink.upcast_ref());
    pipeline.add_many(static_elements.as_slice())?;

    // Link the video processing chain:  [imagefreeze →] videoconvert → videoscale → capsfilter → appsink
    if let Some(ref freeze) = maybe_freeze {
        gst::Element::link_many(&[freeze, &convert, &scale, &capsfilter, appsink.upcast_ref()])?;
    } else {
        gst::Element::link_many(&[&convert, &scale, &capsfilter, appsink.upcast_ref()])?;
    }

    // When uridecodebin adds a decoded pad: route video to the thumbnail
    // chain, everything else (audio, metadata) to a new fakesink.
    let video_linked = Arc::new(AtomicBool::new(false));
    {
        let pipeline_weak = pipeline.downgrade();
        let convert_weak = convert.downgrade();
        let freeze_weak = maybe_freeze.as_ref().map(|f| f.downgrade());
        let video_linked_for_cb = video_linked.clone();
        uridec.connect_pad_added(move |_src, pad| {
            let Some(pipeline) = pipeline_weak.upgrade() else {
                return;
            };
            // Prefer linking directly to the video chain sink. We cannot rely on
            // current_caps() in pad-added: some demux/decode combinations emit
            // pads before caps are fixed. A direct link attempt is robust here:
            // video/raw succeeds, non-video fails and is drained to fakesink.
            let sink_element = if let Some(ref fw) = freeze_weak {
                fw.upgrade()
            } else {
                convert_weak.upgrade()
            };
            if let Some(sink) = sink_element {
                if let Some(sink_pad) = sink.static_pad("sink") {
                    if !sink_pad.is_linked() && pad.link(&sink_pad).is_ok() {
                        video_linked_for_cb.store(true, Ordering::Relaxed);
                        return;
                    }
                }
            }
            // Drain non-video pads (or extra video pads once sink is linked) to
            // prevent any decodebin/multiqueue branch from stalling preroll.
            if let Ok(fakesink) = gst::ElementFactory::make("fakesink")
                .property("sync", false)
                .build()
            {
                let _ = pipeline.add(&fakesink);
                let _ = fakesink.sync_state_with_parent();
                if let Some(sink_pad) = fakesink.static_pad("sink") {
                    let _ = pad.link(&sink_pad);
                }
            }
        });
    }

    // Also tune any decoder elements for single-threaded, low-latency extraction.
    {
        let uridec_bin = uridec.dynamic_cast_ref::<gst::Bin>();
        if let Some(bin) = uridec_bin {
            bin.connect_element_added(|_, element| {
                tune_decoder_threads(element);
            });
        }
    }

    let guard = super::PipelineGuard(pipeline.clone());
    let pipeline = &guard.0;

    pipeline.set_state(gst::State::Paused)?;
    // Wait for pre-roll (up to 10 s — 4K HEVC files can be slow to decode).
    let _ = pipeline.state(Some(gst::ClockTime::from_seconds(10)));
    if !video_linked.load(Ordering::Relaxed) {
        return Err(anyhow::anyhow!("no video pad linked"));
    }

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

fn extract_rgba_ffmpeg(source_path: &str, time_ns: u64) -> Result<Vec<u8>> {
    let ffmpeg = crate::media::export::find_ffmpeg()?;
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-hide_banner").arg("-loglevel").arg("error");
    if time_ns > 0 {
        let seek_sec = format!("{:.3}", time_ns as f64 / 1_000_000_000.0);
        cmd.arg("-ss").arg(seek_sec);
    }
    let filter = format!(
        "scale={THUMB_W}:{THUMB_H}:force_original_aspect_ratio=decrease,\
         pad={THUMB_W}:{THUMB_H}:(ow-iw)/2:(oh-ih)/2:color=black,format=rgba"
    );
    cmd.arg("-i")
        .arg(source_path)
        .arg("-frames:v")
        .arg("1")
        .arg("-an")
        .arg("-sn")
        .arg("-dn")
        .arg("-vf")
        .arg(filter)
        .arg("-f")
        .arg("rawvideo")
        .arg("-pix_fmt")
        .arg("rgba")
        .arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = cmd.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("ffmpeg failed: {stderr}"));
    }
    let expected = (THUMB_W * THUMB_H * 4) as usize;
    if output.stdout.len() < expected {
        return Err(anyhow::anyhow!(
            "ffmpeg output too short: {} < {}",
            output.stdout.len(),
            expected
        ));
    }
    let mut data = output.stdout;
    if data.len() > expected {
        data.truncate(expected);
    }
    Ok(data)
}

fn extract_animated_svg_rgba(source_path: &str, time_ns: u64) -> Result<Vec<u8>> {
    let frame = crate::media::animated_svg::render_svg_frame_at_time(source_path, time_ns)?;
    Ok(scale_and_pad_rgba(
        &frame.rgba,
        frame.width as usize,
        frame.height as usize,
        THUMB_W as usize,
        THUMB_H as usize,
    ))
}

fn scale_and_pad_rgba(
    src: &[u8],
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
) -> Vec<u8> {
    let mut out = vec![0u8; dst_w * dst_h * 4];
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return out;
    }
    let scale = (dst_w as f64 / src_w as f64).min(dst_h as f64 / src_h as f64);
    let scaled_w = ((src_w as f64 * scale).round() as usize).clamp(1, dst_w);
    let scaled_h = ((src_h as f64 * scale).round() as usize).clamp(1, dst_h);
    let offset_x = (dst_w.saturating_sub(scaled_w)) / 2;
    let offset_y = (dst_h.saturating_sub(scaled_h)) / 2;

    for y in 0..scaled_h {
        let src_y = ((y as f64 / scaled_h as f64) * src_h as f64).floor() as usize;
        let src_y = src_y.min(src_h - 1);
        for x in 0..scaled_w {
            let src_x = ((x as f64 / scaled_w as f64) * src_w as f64).floor() as usize;
            let src_x = src_x.min(src_w - 1);
            let src_idx = (src_y * src_w + src_x) * 4;
            let dst_idx = ((offset_y + y) * dst_w + (offset_x + x)) * 4;
            out[dst_idx..dst_idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
        }
    }
    out
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
