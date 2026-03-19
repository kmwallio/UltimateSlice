//! Built-in title templates — preset configurations for common title styles.

use crate::model::clip::{Clip, ClipKind};
use uuid::Uuid;

/// A built-in title template with preset styling.
pub struct TitleTemplate {
    pub id: &'static str,
    pub display_name: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub font: &'static str,
    pub color: u32,
    pub x: f64,
    pub y: f64,
    pub outline_color: u32,
    pub outline_width: f64,
    pub shadow: bool,
    pub shadow_color: u32,
    pub shadow_offset_x: f64,
    pub shadow_offset_y: f64,
    pub bg_box: bool,
    pub bg_box_color: u32,
    pub bg_box_padding: f64,
    pub clip_bg_color: u32,
    pub default_duration_ns: u64,
}

const FIVE_SECONDS_NS: u64 = 5_000_000_000;

pub static TEMPLATES: &[TitleTemplate] = &[
    // ── Standard ──────────────────────────────────────────────────────
    TitleTemplate {
        id: "lower_third_banner",
        display_name: "Lower Third (Banner)",
        category: "Standard",
        description: "Semi-transparent dark banner near the bottom",
        font: "Sans Bold 28",
        color: 0xFFFFFFFF,
        x: 0.5,
        y: 0.85,
        outline_color: 0x000000FF,
        outline_width: 0.0,
        shadow: false,
        shadow_color: 0x000000AA,
        shadow_offset_x: 2.0,
        shadow_offset_y: 2.0,
        bg_box: true,
        bg_box_color: 0x000000BB,
        bg_box_padding: 12.0,
        clip_bg_color: 0x00000000,
        default_duration_ns: FIVE_SECONDS_NS,
    },
    TitleTemplate {
        id: "lower_third_clean",
        display_name: "Lower Third Clean",
        category: "Standard",
        description: "Clean text with outline, no banner",
        font: "Sans Bold 28",
        color: 0xFFFFFFFF,
        x: 0.5,
        y: 0.85,
        outline_color: 0x000000FF,
        outline_width: 2.0,
        shadow: false,
        shadow_color: 0x000000AA,
        shadow_offset_x: 2.0,
        shadow_offset_y: 2.0,
        bg_box: false,
        bg_box_color: 0x00000088,
        bg_box_padding: 8.0,
        clip_bg_color: 0x00000000,
        default_duration_ns: FIVE_SECONDS_NS,
    },
    TitleTemplate {
        id: "centered_title",
        display_name: "Centered Title",
        category: "Standard",
        description: "Large bold text centered with drop shadow",
        font: "Sans Bold 48",
        color: 0xFFFFFFFF,
        x: 0.5,
        y: 0.5,
        outline_color: 0x000000FF,
        outline_width: 0.0,
        shadow: true,
        shadow_color: 0x000000CC,
        shadow_offset_x: 3.0,
        shadow_offset_y: 3.0,
        bg_box: false,
        bg_box_color: 0x00000088,
        bg_box_padding: 8.0,
        clip_bg_color: 0x00000000,
        default_duration_ns: FIVE_SECONDS_NS,
    },
    TitleTemplate {
        id: "subtitle",
        display_name: "Subtitle",
        category: "Standard",
        description: "Small text at the very bottom with background box",
        font: "Sans 22",
        color: 0xFFFFFFFF,
        x: 0.5,
        y: 0.92,
        outline_color: 0x000000FF,
        outline_width: 0.0,
        shadow: false,
        shadow_color: 0x000000AA,
        shadow_offset_x: 2.0,
        shadow_offset_y: 2.0,
        bg_box: true,
        bg_box_color: 0x000000AA,
        bg_box_padding: 6.0,
        clip_bg_color: 0x00000000,
        default_duration_ns: FIVE_SECONDS_NS,
    },
    // ── Cinematic ─────────────────────────────────────────────────────
    TitleTemplate {
        id: "full_screen",
        display_name: "Full Screen",
        category: "Cinematic",
        description: "Very large centered text on solid black background",
        font: "Sans Bold 64",
        color: 0xFFFFFFFF,
        x: 0.5,
        y: 0.5,
        outline_color: 0x000000FF,
        outline_width: 0.0,
        shadow: true,
        shadow_color: 0x000000CC,
        shadow_offset_x: 4.0,
        shadow_offset_y: 4.0,
        bg_box: false,
        bg_box_color: 0x00000088,
        bg_box_padding: 8.0,
        clip_bg_color: 0x000000FF,
        default_duration_ns: FIVE_SECONDS_NS,
    },
    TitleTemplate {
        id: "chapter_heading",
        display_name: "Chapter Heading",
        category: "Cinematic",
        description: "Outline + shadow serif heading",
        font: "Serif Bold 42",
        color: 0xFFFFFFFF,
        x: 0.5,
        y: 0.45,
        outline_color: 0x333333FF,
        outline_width: 2.0,
        shadow: true,
        shadow_color: 0x000000CC,
        shadow_offset_x: 3.0,
        shadow_offset_y: 3.0,
        bg_box: false,
        bg_box_color: 0x00000088,
        bg_box_padding: 8.0,
        clip_bg_color: 0x00000000,
        default_duration_ns: FIVE_SECONDS_NS,
    },
    TitleTemplate {
        id: "cinematic",
        display_name: "Cinematic",
        category: "Cinematic",
        description: "Large serif text with outline and shadow",
        font: "Serif Bold 56",
        color: 0xFFFFFFFF,
        x: 0.5,
        y: 0.5,
        outline_color: 0x222222FF,
        outline_width: 2.0,
        shadow: true,
        shadow_color: 0x000000CC,
        shadow_offset_x: 4.0,
        shadow_offset_y: 4.0,
        bg_box: false,
        bg_box_color: 0x00000088,
        bg_box_padding: 8.0,
        clip_bg_color: 0x00000000,
        default_duration_ns: FIVE_SECONDS_NS,
    },
    // ── Informational ─────────────────────────────────────────────────
    TitleTemplate {
        id: "end_credits",
        display_name: "End Credits",
        category: "Informational",
        description: "Centered text on solid black, supports secondary line",
        font: "Sans 32",
        color: 0xFFFFFFFF,
        x: 0.5,
        y: 0.4,
        outline_color: 0x000000FF,
        outline_width: 0.0,
        shadow: false,
        shadow_color: 0x000000AA,
        shadow_offset_x: 2.0,
        shadow_offset_y: 2.0,
        bg_box: false,
        bg_box_color: 0x00000088,
        bg_box_padding: 8.0,
        clip_bg_color: 0x000000FF,
        default_duration_ns: FIVE_SECONDS_NS,
    },
    TitleTemplate {
        id: "callout",
        display_name: "Callout",
        category: "Informational",
        description: "Yellow box with black text for callouts",
        font: "Sans Bold 24",
        color: 0x000000FF,
        x: 0.3,
        y: 0.3,
        outline_color: 0x000000FF,
        outline_width: 0.0,
        shadow: false,
        shadow_color: 0x000000AA,
        shadow_offset_x: 2.0,
        shadow_offset_y: 2.0,
        bg_box: true,
        bg_box_color: 0xFFDD00EE,
        bg_box_padding: 10.0,
        clip_bg_color: 0x00000000,
        default_duration_ns: FIVE_SECONDS_NS,
    },
];

/// Find a template by id.
pub fn find_template(id: &str) -> Option<&'static TitleTemplate> {
    TEMPLATES.iter().find(|t| t.id == id)
}

/// Apply a template's preset values onto an existing clip (title overlay style).
pub fn apply_template_to_clip(template: &TitleTemplate, clip: &mut Clip) {
    clip.title_template = template.id.to_string();
    clip.title_font = template.font.to_string();
    clip.title_color = template.color;
    clip.title_x = template.x;
    clip.title_y = template.y;
    clip.title_outline_color = template.outline_color;
    clip.title_outline_width = template.outline_width;
    clip.title_shadow = template.shadow;
    clip.title_shadow_color = template.shadow_color;
    clip.title_shadow_offset_x = template.shadow_offset_x;
    clip.title_shadow_offset_y = template.shadow_offset_y;
    clip.title_bg_box = template.bg_box;
    clip.title_bg_box_color = template.bg_box_color;
    clip.title_bg_box_padding = template.bg_box_padding;
}

/// Create a standalone `ClipKind::Title` clip from a template.
pub fn create_title_clip(template: &TitleTemplate, timeline_start: u64) -> Clip {
    let mut clip = Clip::new("", template.default_duration_ns, timeline_start, ClipKind::Title);
    clip.id = Uuid::new_v4().to_string();
    clip.label = template.display_name.to_string();
    clip.title_text = template.display_name.to_string();
    clip.title_clip_bg_color = template.clip_bg_color;
    apply_template_to_clip(template, &mut clip);
    clip
}
