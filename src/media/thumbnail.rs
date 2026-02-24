use anyhow::{anyhow, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use std::path::Path;

/// Extract a single video frame from a file at `time_ns` nanoseconds.
/// Returns the frame as a `gdk4::MemoryTexture`.
pub fn extract_frame(source_path: &str, time_ns: u64) -> Result<gdk4::MemoryTexture> {
    gst::init()?;

    let uri = path_to_uri(source_path);

    let pipeline_desc = format!(
        "uridecodebin uri=\"{uri}\" ! videoconvert ! videoscale ! \
         video/x-raw,format=RGBA,width=160,height=90 ! \
         appsink name=sink sync=false"
    );

    let pipeline = gst::parse::launch(&pipeline_desc)?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("not a pipeline"))?;

    let appsink = pipeline
        .by_name("sink")
        .ok_or_else(|| anyhow!("no appsink"))?
        .downcast::<AppSink>()
        .map_err(|_| anyhow!("not an appsink"))?;

    appsink.set_property("max-buffers", 1u32);
    appsink.set_property("drop", false);

    pipeline.set_state(gst::State::Paused)?;
    pipeline.state(Some(gst::ClockTime::from_seconds(5)));

    // Seek to desired time
    pipeline.seek_simple(
        gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
        gst::ClockTime::from_nseconds(time_ns),
    )?;
    pipeline.state(Some(gst::ClockTime::from_seconds(2)));

    pipeline.set_state(gst::State::Playing)?;

    let sample = appsink.pull_sample()
        .map_err(|_| anyhow!("failed to pull frame"))?;

    let buffer = sample.buffer().ok_or_else(|| anyhow!("no buffer"))?;
    let _caps = sample.caps().ok_or_else(|| anyhow!("no caps"))?;

    let map = buffer.map_readable()?;
    let bytes = glib::Bytes::from(map.as_slice());

    let texture = gdk4::MemoryTexture::new(
        160,
        90,
        gdk4::MemoryFormat::R8g8b8a8,
        &bytes,
        160 * 4,
    );

    pipeline.set_state(gst::State::Null)?;

    Ok(texture)
}

pub fn path_to_uri(path: &str) -> String {
    if path.starts_with("file://") || path.starts_with("http") {
        path.to_string()
    } else {
        format!("file://{}", Path::new(path).canonicalize()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string()))
    }
}
