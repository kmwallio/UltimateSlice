//! Enumeration of Inspector scalar properties that can be copied and pasted.
//!
//! Phase 1 of the right-click Copy / Paste property work covers only
//! f64/f32 sliders. Each variant knows how to:
//!   * read the property's current value from a [`Clip`] as `f64`
//!   * write a new value back to a [`Clip`]
//!   * report whether the property is meaningful for a given clip kind
//!
//! Keeping the enum in one file means the right-click menu helper in
//! `inspector.rs` and the clipboard on `TimelineState` can both refer to
//! a single canonical list.

use crate::model::clip::{Clip, ClipKind};

/// Phase-1 copy/paste-able scalar properties.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClipProperty {
    // Color grade — f32 fields
    Brightness,
    Contrast,
    Saturation,
    Temperature,
    Tint,
    Exposure,
    BlackPoint,
    Shadows,
    Midtones,
    Highlights,
    HighlightsWarmth,
    HighlightsTint,
    MidtonesWarmth,
    MidtonesTint,
    ShadowsWarmth,
    ShadowsTint,
    // Detail — f32 fields
    Denoise,
    Sharpness,
    Blur,
    // Transform — f64 / i32 fields
    Scale,
    Opacity,
    PositionX,
    PositionY,
    Rotate,
    CropLeft,
    CropRight,
    CropTop,
    CropBottom,
    // Audio — f32 / f64 fields
    Volume,
    Pan,
    PitchShiftSemitones,
    // Speed — f64 field
    Speed,
    // Keying / BG removal — f32 / f64 fields
    ChromaKeyTolerance,
    ChromaKeySoftness,
    BgRemovalThreshold,
    // Motion blur — f64 field
    MotionBlurShutterAngle,
}

/// All Phase-1 properties in a fixed order, primarily for tests that want
/// full coverage of the read/write round-trip.
pub const PHASE_1_PROPERTIES: &[ClipProperty] = &[
    ClipProperty::Brightness,
    ClipProperty::Contrast,
    ClipProperty::Saturation,
    ClipProperty::Temperature,
    ClipProperty::Tint,
    ClipProperty::Exposure,
    ClipProperty::BlackPoint,
    ClipProperty::Shadows,
    ClipProperty::Midtones,
    ClipProperty::Highlights,
    ClipProperty::HighlightsWarmth,
    ClipProperty::HighlightsTint,
    ClipProperty::MidtonesWarmth,
    ClipProperty::MidtonesTint,
    ClipProperty::ShadowsWarmth,
    ClipProperty::ShadowsTint,
    ClipProperty::Denoise,
    ClipProperty::Sharpness,
    ClipProperty::Blur,
    ClipProperty::Scale,
    ClipProperty::Opacity,
    ClipProperty::PositionX,
    ClipProperty::PositionY,
    ClipProperty::Rotate,
    ClipProperty::CropLeft,
    ClipProperty::CropRight,
    ClipProperty::CropTop,
    ClipProperty::CropBottom,
    ClipProperty::Volume,
    ClipProperty::Pan,
    ClipProperty::PitchShiftSemitones,
    ClipProperty::Speed,
    ClipProperty::ChromaKeyTolerance,
    ClipProperty::ChromaKeySoftness,
    ClipProperty::BgRemovalThreshold,
    ClipProperty::MotionBlurShutterAngle,
];

/// Broad clip-kind categories used to filter which properties make sense
/// for a given clip. `applies_to()` consults these per-property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    /// Visual properties (video, image, title, drawing, adjustment,
    /// compound, multicam, audition, empty-placeholder).
    Visual,
    /// Audio-affecting properties (video with audio stream, audio clip,
    /// title clip also gets volume because embedded tests pass a default).
    Audio,
    /// Properties that only make sense for clips with a video stream
    /// (anything except pure audio clips).
    VideoOnly,
}

fn clip_has_visual(kind: &ClipKind) -> bool {
    !matches!(kind, ClipKind::Audio)
}

fn clip_has_audio(kind: &ClipKind) -> bool {
    // Only exclude clip kinds that never carry audio under any
    // circumstance. Title/Drawing/Image/Adjustment have no intrinsic
    // audio, but a Video clip always can.
    matches!(
        kind,
        ClipKind::Video | ClipKind::Audio | ClipKind::Compound | ClipKind::Multicam
    )
}

impl ClipProperty {
    /// Short human-readable label shown in the right-click menu (e.g.
    /// "Copy **Brightness**"). Kept terse — the menu prefix supplies
    /// the verb.
    pub fn label(self) -> &'static str {
        use ClipProperty::*;
        match self {
            Brightness => "Brightness",
            Contrast => "Contrast",
            Saturation => "Saturation",
            Temperature => "Temperature",
            Tint => "Tint",
            Exposure => "Exposure",
            BlackPoint => "Black Point",
            Shadows => "Shadows",
            Midtones => "Midtones",
            Highlights => "Highlights",
            HighlightsWarmth => "Highlights Warmth",
            HighlightsTint => "Highlights Tint",
            MidtonesWarmth => "Midtones Warmth",
            MidtonesTint => "Midtones Tint",
            ShadowsWarmth => "Shadows Warmth",
            ShadowsTint => "Shadows Tint",
            Denoise => "Denoise",
            Sharpness => "Sharpness",
            Blur => "Blur",
            Scale => "Scale",
            Opacity => "Opacity",
            PositionX => "Position X",
            PositionY => "Position Y",
            Rotate => "Rotate",
            CropLeft => "Crop Left",
            CropRight => "Crop Right",
            CropTop => "Crop Top",
            CropBottom => "Crop Bottom",
            Volume => "Volume",
            Pan => "Pan",
            PitchShiftSemitones => "Pitch Shift",
            Speed => "Speed",
            ChromaKeyTolerance => "Chroma Key Tolerance",
            ChromaKeySoftness => "Chroma Key Softness",
            BgRemovalThreshold => "BG Removal Threshold",
            MotionBlurShutterAngle => "Motion Blur Shutter Angle",
        }
    }

    fn family(self) -> Family {
        use ClipProperty::*;
        match self {
            Volume | Pan | PitchShiftSemitones => Family::Audio,
            ChromaKeyTolerance | ChromaKeySoftness | BgRemovalThreshold
            | MotionBlurShutterAngle => Family::VideoOnly,
            _ => Family::Visual,
        }
    }

    /// Whether the property is meaningful on this clip. The right-click
    /// menu disables Paste and the Paste-to-all loop skips clips for
    /// which this returns `false`.
    pub fn applies_to(self, clip: &Clip) -> bool {
        match self.family() {
            Family::Visual => clip_has_visual(&clip.kind),
            Family::Audio => clip_has_audio(&clip.kind),
            Family::VideoOnly => matches!(
                clip.kind,
                ClipKind::Video | ClipKind::Compound | ClipKind::Multicam | ClipKind::Audition
            ),
        }
    }

    /// Read the current static value of this property. Keyframe lanes are
    /// ignored — Phase 1 copies only the baseline value (matching the
    /// existing Color Grade copy/paste behavior).
    pub fn read(self, clip: &Clip) -> f64 {
        use ClipProperty::*;
        match self {
            Brightness => clip.brightness as f64,
            Contrast => clip.contrast as f64,
            Saturation => clip.saturation as f64,
            Temperature => clip.temperature as f64,
            Tint => clip.tint as f64,
            Exposure => clip.exposure as f64,
            BlackPoint => clip.black_point as f64,
            Shadows => clip.shadows as f64,
            Midtones => clip.midtones as f64,
            Highlights => clip.highlights as f64,
            HighlightsWarmth => clip.highlights_warmth as f64,
            HighlightsTint => clip.highlights_tint as f64,
            MidtonesWarmth => clip.midtones_warmth as f64,
            MidtonesTint => clip.midtones_tint as f64,
            ShadowsWarmth => clip.shadows_warmth as f64,
            ShadowsTint => clip.shadows_tint as f64,
            Denoise => clip.denoise as f64,
            Sharpness => clip.sharpness as f64,
            Blur => clip.blur as f64,
            Scale => clip.scale,
            Opacity => clip.opacity,
            PositionX => clip.position_x,
            PositionY => clip.position_y,
            Rotate => clip.rotate as f64,
            CropLeft => clip.crop_left as f64,
            CropRight => clip.crop_right as f64,
            CropTop => clip.crop_top as f64,
            CropBottom => clip.crop_bottom as f64,
            Volume => clip.volume as f64,
            Pan => clip.pan as f64,
            PitchShiftSemitones => clip.pitch_shift_semitones,
            Speed => clip.speed,
            ChromaKeyTolerance => clip.chroma_key_tolerance as f64,
            ChromaKeySoftness => clip.chroma_key_softness as f64,
            BgRemovalThreshold => clip.bg_removal_threshold,
            MotionBlurShutterAngle => clip.motion_blur_shutter_angle,
        }
    }

    /// Write a previously-copied value to this property. The value is
    /// truncated to the backing field's precision where necessary (e.g.,
    /// `Rotate` rounds to the nearest integer degree).
    pub fn write(self, clip: &mut Clip, value: f64) {
        use ClipProperty::*;
        match self {
            Brightness => clip.brightness = value as f32,
            Contrast => clip.contrast = value as f32,
            Saturation => clip.saturation = value as f32,
            Temperature => clip.temperature = value as f32,
            Tint => clip.tint = value as f32,
            Exposure => clip.exposure = value as f32,
            BlackPoint => clip.black_point = value as f32,
            Shadows => clip.shadows = value as f32,
            Midtones => clip.midtones = value as f32,
            Highlights => clip.highlights = value as f32,
            HighlightsWarmth => clip.highlights_warmth = value as f32,
            HighlightsTint => clip.highlights_tint = value as f32,
            MidtonesWarmth => clip.midtones_warmth = value as f32,
            MidtonesTint => clip.midtones_tint = value as f32,
            ShadowsWarmth => clip.shadows_warmth = value as f32,
            ShadowsTint => clip.shadows_tint = value as f32,
            Denoise => clip.denoise = value as f32,
            Sharpness => clip.sharpness = value as f32,
            Blur => clip.blur = value as f32,
            Scale => clip.scale = value,
            Opacity => clip.opacity = value,
            PositionX => clip.position_x = value,
            PositionY => clip.position_y = value,
            Rotate => clip.rotate = value.round() as i32,
            CropLeft => clip.crop_left = value.round() as i32,
            CropRight => clip.crop_right = value.round() as i32,
            CropTop => clip.crop_top = value.round() as i32,
            CropBottom => clip.crop_bottom = value.round() as i32,
            Volume => clip.volume = value as f32,
            Pan => clip.pan = value as f32,
            PitchShiftSemitones => clip.pitch_shift_semitones = value,
            Speed => clip.speed = value,
            ChromaKeyTolerance => clip.chroma_key_tolerance = value as f32,
            ChromaKeySoftness => clip.chroma_key_softness = value as f32,
            BgRemovalThreshold => clip.bg_removal_threshold = value,
            MotionBlurShutterAngle => clip.motion_blur_shutter_angle = value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::{Clip, ClipKind};

    fn sample_video_clip() -> Clip {
        Clip::new("/tmp/s.mp4", 10_000_000_000, 0, ClipKind::Video)
    }

    fn sample_audio_clip() -> Clip {
        Clip::new("/tmp/a.wav", 10_000_000_000, 0, ClipKind::Audio)
    }

    fn sample_title_clip() -> Clip {
        Clip::new("", 5_000_000_000, 0, ClipKind::Title)
    }

    #[test]
    fn every_property_read_write_round_trips() {
        let mut clip = sample_video_clip();
        for prop in PHASE_1_PROPERTIES.iter().copied() {
            // Pick a representative non-default value per property. The exact
            // value doesn't matter; what matters is that write(read(x)) is a
            // no-op and read(write(y)) == y (modulo f32 truncation).
            let sentinel: f64 = match prop {
                ClipProperty::Rotate
                | ClipProperty::CropLeft
                | ClipProperty::CropRight
                | ClipProperty::CropTop
                | ClipProperty::CropBottom => 12.0,
                ClipProperty::Temperature => 5200.0,
                ClipProperty::MotionBlurShutterAngle => 270.0,
                ClipProperty::Speed => 0.5,
                ClipProperty::Volume => 0.75,
                ClipProperty::PitchShiftSemitones => -3.0,
                ClipProperty::BgRemovalThreshold => 0.4,
                _ => 0.25,
            };

            prop.write(&mut clip, sentinel);
            let read_back = prop.read(&clip);
            assert!(
                (read_back - sentinel).abs() < 1e-3,
                "{:?}: expected {sentinel}, got {read_back}",
                prop,
            );

            // round-trip: write(read(...)) is a no-op
            prop.write(&mut clip, read_back);
            let read_again = prop.read(&clip);
            assert!(
                (read_again - read_back).abs() < 1e-6,
                "{:?}: second write changed the value",
                prop,
            );
        }
    }

    #[test]
    fn applies_to_filters_audio_only_on_title_clip() {
        let title = sample_title_clip();
        assert!(
            !ClipProperty::Volume.applies_to(&title),
            "Volume should not apply to a Title clip"
        );
        assert!(
            !ClipProperty::Pan.applies_to(&title),
            "Pan should not apply to a Title clip"
        );
        assert!(
            ClipProperty::Opacity.applies_to(&title),
            "Opacity applies to title (visual)"
        );
    }

    #[test]
    fn applies_to_filters_visual_only_on_audio_clip() {
        let audio = sample_audio_clip();
        assert!(
            !ClipProperty::Brightness.applies_to(&audio),
            "Brightness should not apply to an Audio clip"
        );
        assert!(
            !ClipProperty::Scale.applies_to(&audio),
            "Scale should not apply to an Audio clip"
        );
        assert!(
            ClipProperty::Volume.applies_to(&audio),
            "Volume applies to audio clips"
        );
    }

    #[test]
    fn applies_to_filters_video_only_on_audio_and_title() {
        let audio = sample_audio_clip();
        let title = sample_title_clip();
        for prop in [
            ClipProperty::ChromaKeyTolerance,
            ClipProperty::ChromaKeySoftness,
            ClipProperty::BgRemovalThreshold,
            ClipProperty::MotionBlurShutterAngle,
        ] {
            assert!(
                !prop.applies_to(&audio),
                "{:?} should not apply to audio",
                prop
            );
            assert!(
                !prop.applies_to(&title),
                "{:?} should not apply to title",
                prop
            );
        }
    }

    #[test]
    fn applies_to_all_true_for_video_clip() {
        let video = sample_video_clip();
        for prop in PHASE_1_PROPERTIES.iter().copied() {
            assert!(
                prop.applies_to(&video),
                "{:?} should apply to video",
                prop
            );
        }
    }
}
