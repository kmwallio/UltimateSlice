//! Reference-still persistence: PNG write, thumbnail load, cache-directory
//! helpers for the Program Monitor A/B-compare feature.
//!
//! Stills are stored as PNGs under
//! `$XDG_CACHE_HOME/ultimateslice/reference_stills/<id>.png`. The project file
//! only carries metadata (id, label, dimensions, filename); the pixel data
//! lives entirely in the cache, keyed by a UUID filename so there are no
//! cross-project collisions.

use crate::media::cache_support::cache_root_dir;
use crate::media::program_player::ScopeFrame;
use anyhow::{anyhow, Result};
use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

const CACHE_NAME: &str = "reference_stills";

pub fn reference_stills_dir() -> PathBuf {
    cache_root_dir(CACHE_NAME)
}

pub fn ensure_reference_stills_dir() -> Result<PathBuf> {
    let dir = reference_stills_dir();
    fs::create_dir_all(&dir)
        .map_err(|e| anyhow!("create reference_stills dir {:?}: {e}", dir))?;
    Ok(dir)
}

pub fn still_path(filename: &str) -> PathBuf {
    reference_stills_dir().join(filename)
}

/// Write an RGBA `ScopeFrame` to `path` as a PNG (RGBA, 8-bit).
pub fn write_png(path: &Path, frame: &ScopeFrame) -> Result<()> {
    if frame.width == 0 || frame.height == 0 {
        return Err(anyhow!("frame has zero dimensions"));
    }
    let expected = frame.width.saturating_mul(frame.height).saturating_mul(4);
    if frame.data.len() < expected {
        return Err(anyhow!(
            "frame buffer incomplete: have {}, need {}",
            frame.data.len(),
            expected
        ));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| anyhow!("create {:?}: {e}", parent))?;
    }
    let file = fs::File::create(path).map_err(|e| anyhow!("open {:?} for write: {e}", path))?;
    let w = BufWriter::new(file);

    let mut encoder = png::Encoder::new(w, frame.width as u32, frame.height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| anyhow!("png header: {e}"))?;
    writer
        .write_image_data(&frame.data[..expected])
        .map_err(|e| anyhow!("png write: {e}"))?;
    Ok(())
}

/// Delete a cached still file (best-effort — missing files are not an error).
pub fn delete_still(filename: &str) {
    let path = still_path(filename);
    let _ = fs::remove_file(path);
}

/// Build the default filename for a new still given its uuid.
pub fn filename_for_id(id: &str) -> String {
    format!("{id}.png")
}

/// Decoded PNG buffer for rendering as a Cairo ImageSurface.
pub struct DecodedStill {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Load an RGBA or RGB PNG from disk and return its pixel data.
///
/// Alpha-less PNGs are promoted to opaque RGBA so callers can treat the
/// result uniformly. Non-8-bit PNGs are rejected (the writer only emits
/// 8-bit RGBA, so this should not happen for our own files).
pub fn load_decoded(path: &Path) -> Result<DecodedStill> {
    let file = fs::File::open(path).map_err(|e| anyhow!("open {:?}: {e}", path))?;
    let decoder = png::Decoder::new(file);
    let mut reader = decoder.read_info().map_err(|e| anyhow!("png read_info: {e}"))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| anyhow!("png next_frame: {e}"))?;
    buf.truncate(info.buffer_size());
    if info.bit_depth != png::BitDepth::Eight {
        return Err(anyhow!(
            "unsupported PNG bit depth: {:?}",
            info.bit_depth
        ));
    }
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let pixel_count = (info.width as usize) * (info.height as usize);
            let mut out = Vec::with_capacity(pixel_count * 4);
            for px in buf.chunks_exact(3) {
                out.extend_from_slice(px);
                out.push(255);
            }
            out
        }
        other => return Err(anyhow!("unsupported PNG color type: {:?}", other)),
    };
    Ok(DecodedStill {
        rgba,
        width: info.width,
        height: info.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_frame(w: usize, h: usize, seed: u8) -> ScopeFrame {
        let mut data = vec![0u8; w * h * 4];
        for (i, px) in data.chunks_exact_mut(4).enumerate() {
            px[0] = ((i as u8).wrapping_add(seed)).wrapping_mul(3);
            px[1] = ((i as u8).wrapping_add(seed)).wrapping_mul(5);
            px[2] = ((i as u8).wrapping_add(seed)).wrapping_mul(7);
            px[3] = 255;
        }
        ScopeFrame {
            data,
            width: w,
            height: h,
        }
    }

    #[test]
    fn write_png_produces_readable_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("test.png");
        let frame = make_frame(16, 9, 42);
        write_png(&path, &frame).expect("write");

        let bytes = fs::read(&path).expect("read");
        // PNG magic
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn write_png_rejects_empty_frame() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("empty.png");
        let frame = ScopeFrame {
            data: vec![],
            width: 0,
            height: 0,
        };
        assert!(write_png(&path, &frame).is_err());
    }

    #[test]
    fn write_png_rejects_short_buffer() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("short.png");
        let frame = ScopeFrame {
            data: vec![0u8; 10], // too small for 4×4 RGBA
            width: 4,
            height: 4,
        };
        assert!(write_png(&path, &frame).is_err());
    }
}
