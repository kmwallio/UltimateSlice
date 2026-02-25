use anyhow::{anyhow, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_pbutils::prelude::*;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use crate::model::project::Project;
use crate::media::thumbnail::path_to_uri;

/// Progress updates sent back to the UI thread
#[derive(Debug)]
pub enum ExportProgress {
    Progress(f64),   // 0.0 – 1.0
    Done,
    Error(String),
}

/// Export the project to an MP4 file at `output_path`.
/// Sends progress to `tx`. Call this from a background thread.
pub fn export_project(
    project: &Project,
    output_path: &str,
    tx: mpsc::Sender<ExportProgress>,
) -> Result<()> {
    gst::init()?;

    // For the MVP we concatenate clips from the primary video+audio tracks
    // using a GNonLin / nlecomposition approach via a manual pipeline.
    // We use a simpler strategy here: concatenate clip URIs via a playlist pipeline.

    let video_clips: Vec<_> = project.video_tracks()
        .flat_map(|t| t.clips.iter())
        .collect();

    if video_clips.is_empty() {
        return Err(anyhow!("No video clips to export"));
    }

    // Build a concatenation pipeline using uridecodebin + concat + encode
    let pipeline = gst::Pipeline::new();

    let video_concat = gst::ElementFactory::make("concat").build()?;
    let audio_concat = gst::ElementFactory::make("concat").build()?;
    let videoconvert = gst::ElementFactory::make("videoconvert").build()?;
    let videoscale = gst::ElementFactory::make("videoscale").build()?;
    let x264enc = gst::ElementFactory::make("x264enc")
        .property("bitrate", 4000u32)
        .property_from_str("tune", "zerolatency")
        .build()?;
    let audioconvert = gst::ElementFactory::make("audioconvert").build()?;
    let audioresample = gst::ElementFactory::make("audioresample").build()?;
    let aacenc = gst::ElementFactory::make("faac")
        .build()
        .or_else(|_| gst::ElementFactory::make("avenc_aac").build())
        .or_else(|_| gst::ElementFactory::make("lamemp3enc").build())?;
    let mp4mux = gst::ElementFactory::make("mp4mux").build()?;
    let filesink = gst::ElementFactory::make("filesink")
        .property("location", output_path)
        .build()?;

    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("width", project.width as i32)
                .field("height", project.height as i32)
                .build(),
        )
        .build()?;

    pipeline.add_many([
        &video_concat, &audio_concat,
        &videoconvert, &videoscale, &capsfilter, &x264enc,
        &audioconvert, &audioresample, &aacenc,
        &mp4mux, &filesink,
    ])?;

    // Link video chain
    gst::Element::link_many([&video_concat, &videoconvert, &videoscale, &capsfilter, &x264enc])?;
    x264enc.link_pads(Some("src"), &mp4mux, None)?;

    // Link audio chain
    gst::Element::link_many([&audio_concat, &audioconvert, &audioresample, &aacenc])?;
    aacenc.link_pads(Some("src"), &mp4mux, None)?;

    mp4mux.link(&filesink)?;

    // Add a uridecodebin for each clip
    for clip in &video_clips {
        let uri = path_to_uri(&clip.source_path);
        let _source_in = clip.source_in;
        let _source_out = clip.source_out;

        let decode = gst::ElementFactory::make("uridecodebin")
            .property("uri", &uri)
            .build()?;

        pipeline.add(&decode)?;

        let vc = video_concat.clone();
        let ac = audio_concat.clone();

        decode.connect_pad_added(move |_src, src_pad| {
            let caps = src_pad.current_caps().unwrap_or_else(|| src_pad.query_caps(None));
            let structure = caps.structure(0).unwrap();
            let name = structure.name();

            if name.starts_with("video/") {
                if let Some(sink_pad) = vc.request_pad_simple("sink_%u") {
                    let _ = src_pad.link(&sink_pad);
                }
            } else if name.starts_with("audio/") {
                if let Some(sink_pad) = ac.request_pad_simple("sink_%u") {
                    let _ = src_pad.link(&sink_pad);
                }
            }
        });
    }

    // Watch bus for progress and EOS
    let bus = pipeline.bus().unwrap();
    pipeline.set_state(gst::State::Playing)?;

    let total_duration = project.duration().max(1) as f64;
    let started_at = Instant::now();
    let hard_timeout = Duration::from_secs(180);
    let stall_timeout = Duration::from_secs(20);
    let mut last_pos_ns = 0u64;
    let mut last_pos_change_at = Instant::now();

    loop {
        if started_at.elapsed() > hard_timeout {
            let _ = tx.send(ExportProgress::Error("Export timed out".to_string()));
            pipeline.set_state(gst::State::Null)?;
            return Err(anyhow!("Export timed out"));
        }

        let msg = bus.timed_pop(gst::ClockTime::from_mseconds(100));
        if let Some(msg) = msg {
            use gst::MessageView;
            match msg.view() {
                MessageView::Eos(_) => {
                    let _ = tx.send(ExportProgress::Done);
                    break;
                }
                MessageView::Error(e) => {
                    let err_str = e.error().to_string();
                    let _ = tx.send(ExportProgress::Error(err_str.clone()));
                    pipeline.set_state(gst::State::Null)?;
                    return Err(anyhow!("Export error: {err_str}"));
                }
                _ => {}
            }
        }

        // Report progress
        if let Some(pos) = pipeline.query_position::<gst::ClockTime>() {
            let pos_ns = pos.nseconds();
            if pos_ns != last_pos_ns {
                last_pos_ns = pos_ns;
                last_pos_change_at = Instant::now();
            } else if last_pos_change_at.elapsed() > stall_timeout {
                let _ = tx.send(ExportProgress::Error("Export stalled without progress".to_string()));
                pipeline.set_state(gst::State::Null)?;
                return Err(anyhow!("Export stalled without progress"));
            }
            let progress = pos_ns as f64 / total_duration;
            let _ = tx.send(ExportProgress::Progress(progress.min(1.0)));
        }
    }

    pipeline.set_state(gst::State::Null)?;
    Ok(())
}
