use anyhow::{anyhow, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use std::path::PathBuf;

/// An audio input device discovered by GStreamer.
#[derive(Debug, Clone)]
pub struct AudioInputDevice {
    pub display_name: String,
    pub device: gst::Device,
}

/// Enumerate available audio input devices.
pub fn list_audio_input_devices() -> Vec<AudioInputDevice> {
    let monitor = gst::DeviceMonitor::new();
    let caps = gst::Caps::builder("audio/x-raw").build();
    monitor.add_filter(Some("Audio/Source"), Some(&caps));
    if monitor.start().is_err() {
        return Vec::new();
    }
    let devices: Vec<AudioInputDevice> = monitor
        .devices()
        .into_iter()
        .map(|d| AudioInputDevice {
            display_name: d.display_name().to_string(),
            device: d,
        })
        .collect();
    monitor.stop();
    devices
}

/// State of the voiceover recorder.
#[derive(Debug, Clone)]
pub enum RecorderState {
    Idle,
    Recording {
        start_position_ns: u64,
        file_path: String,
    },
    Finished {
        file_path: String,
        duration_ns: u64,
        start_position_ns: u64,
    },
    Error(String),
}

/// GStreamer-based audio capture for voiceover recording.
///
/// Pipeline: `[autoaudiosrc|device] → audioconvert → audioresample → capsfilter → wavenc → filesink`
pub struct VoiceoverRecorder {
    pipeline: Option<gst::Pipeline>,
    src_element: Option<gst::Element>,
    state: RecorderState,
}

impl VoiceoverRecorder {
    pub fn new() -> Self {
        Self {
            pipeline: None,
            src_element: None,
            state: RecorderState::Idle,
        }
    }

    pub fn state(&self) -> &RecorderState {
        &self.state
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.state, RecorderState::Recording { .. })
    }

    /// Start recording audio. If `device` is Some, use that specific device;
    /// otherwise use `autoaudiosrc` (system default).
    pub fn start_recording(
        &mut self,
        start_position_ns: u64,
        device: Option<&gst::Device>,
        mono: bool,
    ) -> Result<String> {
        if self.is_recording() {
            return Err(anyhow!("Already recording"));
        }

        let cache_dir = voiceover_cache_dir();
        std::fs::create_dir_all(&cache_dir)
            .map_err(|e| anyhow!("Failed to create voiceover cache dir: {e}"))?;

        let file_path = cache_dir
            .join(format!("voiceover_{}.wav", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .to_string();

        // Build the pipeline using gst::parse::launch — this handles autoaudiosrc
        // bin ghost pad linking correctly (same mechanism as gst-launch-1.0).
        let src_name = if let Some(dev) = device {
            // Device-specific: get the element factory name and device property.
            let props = dev.properties();
            let device_path = props.as_ref().and_then(|s| {
                s.get::<String>("device.path")
                    .ok()
                    .or_else(|| s.get::<String>("object.path").ok())
            });
            if let Some(path) = device_path {
                format!("pulsesrc device=\"{}\"", path.replace('"', "\\\""))
            } else {
                // Fallback: let the device create its own element.
                "autoaudiosrc".to_string()
            }
        } else {
            // Try pulsesrc first (direct element, more reliable), fall back to autoaudiosrc.
            if gst::ElementFactory::find("pulsesrc").is_some() {
                "pulsesrc".to_string()
            } else {
                "autoaudiosrc".to_string()
            }
        };

        let escaped_path = file_path.replace('\\', "\\\\").replace('"', "\\\"");
        let channels = if mono { 1 } else { 2 };
        let launch_str = format!(
            "{src_name} ! audioconvert ! audioresample ! audio/x-raw,format=S16LE,rate=48000,channels={channels} ! wavenc ! filesink location=\"{escaped_path}\""
        );
        log::info!("Voiceover: launching pipeline: {}", launch_str);

        let element = gst::parse::launch(&launch_str)
            .map_err(|e| anyhow!("Failed to create recording pipeline: {e}"))?;
        let pipeline = element
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("parse::launch did not return a Pipeline"))?;

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| anyhow!("Failed to start recording — check microphone permissions"))?;

        // Wait for the pipeline to reach Playing (with data flowing).
        match pipeline.state(Some(gst::ClockTime::from_seconds(5))) {
            (Err(_), _, _) => {
                let _ = pipeline.set_state(gst::State::Null);
                return Err(anyhow!("Recording pipeline failed to reach Playing state"));
            }
            _ => {}
        }

        log::info!("Voiceover: recording started → {}", file_path);

        self.src_element = None; // Not needed with parse::launch — EOS goes to pipeline.
        self.pipeline = Some(pipeline);
        self.state = RecorderState::Recording {
            start_position_ns,
            file_path: file_path.clone(),
        };

        Ok(file_path)
    }

    /// Stop recording. Finalizes the WAV file and returns (file_path, duration_ns, start_position_ns).
    pub fn stop_recording(&mut self) -> Result<(String, u64, u64)> {
        let (start_position_ns, file_path) = match &self.state {
            RecorderState::Recording {
                start_position_ns,
                file_path,
            } => (*start_position_ns, file_path.clone()),
            _ => return Err(anyhow!("Not recording")),
        };

        if let Some(ref pipeline) = self.pipeline {
            // For live sources, sending EOS and waiting for propagation can take
            // seconds while buffered audio drains. Instead, send EOS to the source
            // pads and give wavenc a short window to finalize the WAV header, then
            // force the pipeline to Null.
            let iter = pipeline.iterate_sources();
            for src in iter.into_iter().flatten() {
                if let Some(pad) = src.static_pad("src") {
                    pad.send_event(gst::event::Eos::new());
                }
            }

            // Short wait for wavenc to write the finalized WAV header.
            if let Some(bus) = pipeline.bus() {
                let _ = bus.timed_pop_filtered(
                    Some(gst::ClockTime::from_mseconds(500)),
                    &[gst::MessageType::Eos, gst::MessageType::Error],
                );
            }

            // Force stop immediately — don't wait for the live source to drain.
            let _ = pipeline.set_state(gst::State::Null);
            let _ = pipeline.state(Some(gst::ClockTime::from_seconds(1)));
        }
        self.pipeline = None;
        self.src_element = None;

        // Check the file was actually written.
        let file_size = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
        log::info!(
            "Voiceover: WAV file size = {} bytes at {}",
            file_size,
            file_path
        );

        if file_size <= 44 {
            // 44 bytes is just the WAV header — no audio data was captured.
            self.state = RecorderState::Error("No audio data captured".to_string());
            return Err(anyhow!(
                "No audio data captured (file size {} bytes). Check microphone permissions.",
                file_size
            ));
        }

        // Probe the WAV file duration.
        let duration_ns = probe_wav_duration(&file_path).unwrap_or_else(|| {
            // Fallback: compute duration from file size (PCM S16LE, 48kHz, stereo).
            // bytes_per_second = 48000 * 2 channels * 2 bytes = 192000
            let audio_bytes = file_size.saturating_sub(44); // subtract WAV header
            let duration_s = audio_bytes as f64 / 192000.0;
            (duration_s * 1e9) as u64
        });

        log::info!(
            "Voiceover: recording stopped, duration={:.2}s file_size={} path={}",
            duration_ns as f64 / 1e9,
            file_size,
            file_path
        );

        if duration_ns == 0 {
            self.state = RecorderState::Error("Recording produced zero-length audio".to_string());
            return Err(anyhow!("Recording produced zero-length audio"));
        }

        self.state = RecorderState::Finished {
            file_path: file_path.clone(),
            duration_ns,
            start_position_ns,
        };

        Ok((file_path, duration_ns, start_position_ns))
    }

    /// Cancel a recording in progress without keeping the file.
    pub fn cancel(&mut self) {
        if let Some(ref pipeline) = self.pipeline {
            let _ = pipeline.set_state(gst::State::Null);
        }
        if let RecorderState::Recording { ref file_path, .. } = self.state {
            let _ = std::fs::remove_file(file_path);
        }
        self.pipeline = None;
        self.src_element = None;
        self.state = RecorderState::Idle;
    }

    /// Reset to idle state (after clip has been placed or error acknowledged).
    pub fn reset(&mut self) {
        self.state = RecorderState::Idle;
    }
}

impl Drop for VoiceoverRecorder {
    fn drop(&mut self) {
        if self.is_recording() {
            self.cancel();
        }
    }
}

/// Probe WAV file duration using GStreamer Discoverer.
fn probe_wav_duration(path: &str) -> Option<u64> {
    use gstreamer_pbutils::prelude::*;
    let discoverer = gstreamer_pbutils::Discoverer::new(gst::ClockTime::from_seconds(5)).ok()?;
    let uri = format!("file://{path}");
    let info = discoverer.discover_uri(&uri).ok()?;
    info.duration().map(|d| d.nseconds())
}

/// Persistent directory for voiceover recordings.
/// Uses `$XDG_DATA_HOME/ultimateslice/voiceovers/` (typically `~/.local/share/...`)
/// so recordings survive reboots. These files are source media referenced by
/// the project — they must not be in `/tmp`.
pub fn voiceover_cache_dir() -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(|v| PathBuf::from(v).join("ultimateslice").join("voiceovers"))
        .unwrap_or_else(|| {
            // Fallback: ~/.local/share/ultimateslice/voiceovers
            if let Some(home) = std::env::var("HOME").ok().filter(|v| !v.is_empty()) {
                PathBuf::from(home).join(".local/share/ultimateslice/voiceovers")
            } else {
                PathBuf::from("/tmp/ultimateslice/voiceovers")
            }
        })
}

/// Check if a new clip at `start_ns` with `duration_ns` would overlap existing clips on a track.
/// Returns the first free position at or after `start_ns`.
pub fn find_non_overlapping_start(
    clips: &[crate::model::clip::Clip],
    start_ns: u64,
    duration_ns: u64,
) -> u64 {
    let end_ns = start_ns + duration_ns;
    for clip in clips {
        let clip_end = clip.timeline_start + clip.duration();
        // Check overlap
        if start_ns < clip_end && end_ns > clip.timeline_start {
            // Overlap found — try placing after this clip.
            return find_non_overlapping_start(clips, clip_end, duration_ns);
        }
    }
    start_ns
}
