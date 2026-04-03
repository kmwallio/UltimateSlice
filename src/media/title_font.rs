use std::collections::HashMap;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

pub(crate) const DEFAULT_TITLE_FONT_DESC: &str = "Sans Bold 36";
const DEFAULT_TITLE_FONT_SIZE_POINTS: f64 = 36.0;
pub(crate) const DEFAULT_SUBTITLE_FONT_DESC: &str = "Sans Bold 24";
const DEFAULT_SUBTITLE_FONT_SIZE_POINTS: f64 = 24.0;
const FC_MATCH_SEPARATOR: &str = "\u{1f}";

#[derive(Clone, Debug)]
pub(crate) struct FontSpec {
    desc: pango::FontDescription,
    normalized_desc: String,
    family: String,
    size_points: f64,
    fontconfig_pattern: String,
    requested_style: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DrawtextFontMatchKind {
    Exact,
    Fallback,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DrawtextFontMatch {
    pub(crate) kind: DrawtextFontMatchKind,
    pub(crate) option_name: &'static str,
    pub(crate) option_value: String,
    pub(crate) matched_family: Option<String>,
    pub(crate) matched_style: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FcMatchRecord {
    file: String,
    family: Option<String>,
    style: Option<String>,
}

static DRAWTEXT_FONT_CACHE: OnceLock<Mutex<HashMap<String, DrawtextFontMatch>>> = OnceLock::new();

pub(crate) fn parse_title_font(font_desc: &str) -> FontSpec {
    FontSpec::new(
        font_desc,
        DEFAULT_TITLE_FONT_DESC,
        DEFAULT_TITLE_FONT_SIZE_POINTS,
    )
}

pub(crate) fn parse_subtitle_font(font_desc: &str) -> FontSpec {
    FontSpec::new(
        font_desc,
        DEFAULT_SUBTITLE_FONT_DESC,
        DEFAULT_SUBTITLE_FONT_SIZE_POINTS,
    )
}

pub(crate) fn normalize_title_font_label(font_desc: &str) -> String {
    parse_title_font(font_desc)
        .normalized_description()
        .to_string()
}

pub(crate) fn normalize_subtitle_font_label(font_desc: &str) -> String {
    parse_subtitle_font(font_desc)
        .normalized_description()
        .to_string()
}

pub(crate) fn build_preview_title_font_desc(font_desc: &str, size_points: f64) -> String {
    let spec = parse_title_font(font_desc);
    let resolution = resolve_drawtext_font_for_spec(&spec);
    spec.preview_description_for_match(&resolution, size_points)
}

pub(crate) fn build_preview_subtitle_font_desc(font_desc: &str, size_points: f64) -> String {
    let spec = parse_subtitle_font(font_desc);
    let resolution = resolve_drawtext_font_for_spec(&spec);
    spec.preview_description_for_match(&resolution, size_points)
}

pub(crate) fn build_drawtext_font_selector(font_desc: &str) -> String {
    parse_title_font(font_desc).fontconfig_pattern().to_string()
}

pub(crate) fn build_drawtext_font_option(font_desc: &str) -> String {
    resolve_drawtext_font(font_desc).filter_fragment()
}

pub(crate) fn build_title_font_tooltip(font_desc: &str, base: &str) -> String {
    let spec = parse_title_font(font_desc);
    let resolution = resolve_drawtext_font_for_spec(&spec);
    build_font_tooltip(&spec, &resolution, base, "Export/prerender")
}

pub(crate) fn build_subtitle_font_tooltip(font_desc: &str, base: &str) -> String {
    let spec = parse_subtitle_font(font_desc);
    let resolution = resolve_drawtext_font_for_spec(&spec);
    build_font_tooltip(&spec, &resolution, base, "Preview/export")
}

fn build_font_tooltip(
    spec: &FontSpec,
    resolution: &DrawtextFontMatch,
    base: &str,
    target_label: &str,
) -> String {
    let Some(matched_label) = resolution.matched_label() else {
        return match resolution.kind {
            DrawtextFontMatchKind::Unavailable => format!(
                "{base}\n{target_label} font resolution unavailable; using best-effort font matching."
            ),
            _ => base.to_string(),
        };
    };
    match resolution.kind {
        DrawtextFontMatchKind::Exact => format!("{base}\n{target_label}: {matched_label}"),
        DrawtextFontMatchKind::Fallback => format!(
            "{base}\nRequested \"{}\" falls back in {target_label} to {matched_label}.",
            spec.normalized_description()
        ),
        DrawtextFontMatchKind::Unavailable => format!(
            "{base}\n{target_label} font resolution unavailable; using best-effort font matching."
        ),
    }
}

pub(crate) fn escape_drawtext_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(':', "\\:")
        .replace('\'', "\\'")
        .replace('%', "\\%")
}

pub(crate) fn resolve_drawtext_font(font_desc: &str) -> DrawtextFontMatch {
    let spec = parse_title_font(font_desc);
    resolve_drawtext_font_for_spec(&spec)
}

fn resolve_drawtext_font_for_spec(spec: &FontSpec) -> DrawtextFontMatch {
    let cache_key = spec.fontconfig_pattern().to_string();
    {
        let cache = drawtext_font_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = cache.get(&cache_key) {
            return cached.clone();
        }
    }

    let resolved = resolve_drawtext_font_uncached(spec);
    let mut cache = drawtext_font_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.insert(cache_key, resolved.clone());
    resolved
}

impl FontSpec {
    fn new(font_desc: &str, default_desc: &str, default_size_points: f64) -> Self {
        let trimmed = font_desc.trim();
        let mut desc = pango::FontDescription::from_string(if trimmed.is_empty() {
            default_desc
        } else {
            trimmed
        });
        let family = desc
            .family()
            .map(|family| family.trim().to_string())
            .filter(|family| !family.is_empty());
        if family.is_none() {
            desc = pango::FontDescription::from_string(default_desc);
        }
        if desc.size() <= 0 {
            desc.set_size((default_size_points * pango::SCALE as f64).round() as i32);
        }
        let family = desc
            .family()
            .map(|family| family.trim().to_string())
            .filter(|family| !family.is_empty())
            .unwrap_or_else(|| "Sans".to_string());
        let size_points = (desc.size() as f64 / pango::SCALE as f64).max(1.0);
        let fontconfig_pattern = build_fontconfig_pattern(&desc, &family);
        let requested_style = requested_style_label(&desc);
        let normalized_desc = desc.to_string();
        Self {
            desc,
            normalized_desc,
            family,
            size_points,
            fontconfig_pattern,
            requested_style,
        }
    }

    pub(crate) fn normalized_description(&self) -> &str {
        &self.normalized_desc
    }

    pub(crate) fn size_points(&self) -> f64 {
        self.size_points
    }

    pub(crate) fn fontconfig_pattern(&self) -> &str {
        &self.fontconfig_pattern
    }

    fn preview_description_for_match(
        &self,
        resolution: &DrawtextFontMatch,
        size_points: f64,
    ) -> String {
        if resolution.kind == DrawtextFontMatchKind::Fallback {
            if let Some(desc) = build_matched_preview_desc(
                resolution.matched_family.as_deref(),
                resolution.matched_style.as_deref(),
                size_points,
            ) {
                return desc;
            }
        }
        let mut desc = self.desc.clone();
        desc.set_size((size_points.max(1.0) * pango::SCALE as f64).round() as i32);
        desc.to_string()
    }
}

impl DrawtextFontMatch {
    fn filter_fragment(&self) -> String {
        format!(
            "{}='{}'",
            self.option_name,
            escape_drawtext_value(&self.option_value)
        )
    }

    fn matched_label(&self) -> Option<String> {
        let family = self
            .matched_family
            .as_deref()
            .map(str::trim)
            .filter(|family| !family.is_empty())?;
        let style = self
            .matched_style
            .as_deref()
            .map(str::trim)
            .filter(|style| !style.is_empty() && !is_regular_style(style));
        Some(match style {
            Some(style) => format!("{family} {style}"),
            None => family.to_string(),
        })
    }
}

fn drawtext_font_cache() -> &'static Mutex<HashMap<String, DrawtextFontMatch>> {
    DRAWTEXT_FONT_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn resolve_drawtext_font_uncached(spec: &FontSpec) -> DrawtextFontMatch {
    let output = Command::new("fc-match")
        .arg("-f")
        .arg(format!(
            "%{{file}}{FC_MATCH_SEPARATOR}%{{family[0]}}{FC_MATCH_SEPARATOR}%{{style[0]}}"
        ))
        .arg(spec.fontconfig_pattern())
        .output();

    let Ok(output) = output else {
        return DrawtextFontMatch {
            kind: DrawtextFontMatchKind::Unavailable,
            option_name: "font",
            option_value: spec.fontconfig_pattern().to_string(),
            matched_family: None,
            matched_style: None,
        };
    };

    if !output.status.success() {
        return DrawtextFontMatch {
            kind: DrawtextFontMatchKind::Unavailable,
            option_name: "font",
            option_value: spec.fontconfig_pattern().to_string(),
            matched_family: None,
            matched_style: None,
        };
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(record) = parse_fc_match_record(&stdout) else {
        return DrawtextFontMatch {
            kind: DrawtextFontMatchKind::Unavailable,
            option_name: "font",
            option_value: spec.fontconfig_pattern().to_string(),
            matched_family: None,
            matched_style: None,
        };
    };

    DrawtextFontMatch {
        kind: evaluate_match_kind(spec, &record),
        option_name: "fontfile",
        option_value: record.file.clone(),
        matched_family: record.family.clone(),
        matched_style: record.style.clone(),
    }
}

fn parse_fc_match_record(stdout: &str) -> Option<FcMatchRecord> {
    let mut parts = stdout.trim().split(FC_MATCH_SEPARATOR);
    let file = parts.next()?.trim();
    if file.is_empty() {
        return None;
    }
    let family = parts
        .next()
        .map(str::trim)
        .filter(|family| !family.is_empty());
    let style = parts
        .next()
        .map(str::trim)
        .filter(|style| !style.is_empty());
    Some(FcMatchRecord {
        file: file.to_string(),
        family: family.map(ToOwned::to_owned),
        style: style.map(ToOwned::to_owned),
    })
}

fn evaluate_match_kind(spec: &FontSpec, record: &FcMatchRecord) -> DrawtextFontMatchKind {
    let family_matches = if is_generic_family(&spec.family) {
        record
            .family
            .as_deref()
            .map(|family| !family.trim().is_empty())
            .unwrap_or(false)
    } else {
        record
            .family
            .as_deref()
            .map(|family| normalized_font_name(family) == normalized_font_name(&spec.family))
            .unwrap_or(false)
    };
    let style_matches = spec
        .requested_style
        .as_ref()
        .map_or(true, |requested_style| {
            record.style.as_deref().map_or(false, |matched_style| {
                normalized_font_name(matched_style) == normalized_font_name(requested_style)
            })
        });
    if family_matches && style_matches {
        DrawtextFontMatchKind::Exact
    } else {
        DrawtextFontMatchKind::Fallback
    }
}

fn build_matched_preview_desc(
    family: Option<&str>,
    style: Option<&str>,
    size_points: f64,
) -> Option<String> {
    let family = family.map(str::trim).filter(|family| !family.is_empty())?;
    let mut desc = if let Some(style) = style
        .map(str::trim)
        .filter(|style| !style.is_empty() && !is_regular_style(style))
    {
        pango::FontDescription::from_string(&format!("{family} {style}"))
    } else {
        pango::FontDescription::from_string(family)
    };
    desc.set_size((size_points.max(1.0) * pango::SCALE as f64).round() as i32);
    Some(desc.to_string())
}

fn build_fontconfig_pattern(desc: &pango::FontDescription, family: &str) -> String {
    let mut selector = escape_fontconfig_pattern_value(family);
    if let Some(weight) = fontconfig_weight_selector(desc.weight()) {
        selector.push_str(":weight=");
        selector.push_str(weight);
    }
    if let Some(slant) = fontconfig_slant_selector(desc.style()) {
        selector.push_str(":slant=");
        selector.push_str(slant);
    }
    if let Some(width) = fontconfig_width_selector(desc.stretch()) {
        selector.push_str(":width=");
        selector.push_str(width);
    }
    selector
}

fn escape_fontconfig_pattern_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' | ':' | ',' | '=' | '-' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn fontconfig_weight_selector(weight: pango::Weight) -> Option<&'static str> {
    match weight {
        pango::Weight::Thin => Some("thin"),
        pango::Weight::Ultralight => Some("ultralight"),
        pango::Weight::Light => Some("light"),
        pango::Weight::Semilight => Some("semilight"),
        pango::Weight::Book => Some("book"),
        pango::Weight::Normal => None,
        pango::Weight::Medium => Some("medium"),
        pango::Weight::Semibold => Some("semibold"),
        pango::Weight::Bold => Some("bold"),
        pango::Weight::Ultrabold => Some("ultrabold"),
        pango::Weight::Heavy => Some("heavy"),
        pango::Weight::Ultraheavy => Some("ultraheavy"),
        _ => None,
    }
}

fn fontconfig_slant_selector(style: pango::Style) -> Option<&'static str> {
    match style {
        pango::Style::Normal => None,
        pango::Style::Oblique => Some("oblique"),
        pango::Style::Italic => Some("italic"),
        _ => None,
    }
}

fn fontconfig_width_selector(stretch: pango::Stretch) -> Option<&'static str> {
    match stretch {
        pango::Stretch::UltraCondensed => Some("ultracondensed"),
        pango::Stretch::ExtraCondensed => Some("extracondensed"),
        pango::Stretch::Condensed => Some("condensed"),
        pango::Stretch::SemiCondensed => Some("semicondensed"),
        pango::Stretch::Normal => None,
        pango::Stretch::SemiExpanded => Some("semiexpanded"),
        pango::Stretch::Expanded => Some("expanded"),
        pango::Stretch::ExtraExpanded => Some("extraexpanded"),
        pango::Stretch::UltraExpanded => Some("ultraexpanded"),
        _ => None,
    }
}

fn requested_style_label(desc: &pango::FontDescription) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(width) = stretch_style_label(desc.stretch()) {
        parts.push(width);
    }
    if let Some(weight) = weight_style_label(desc.weight()) {
        parts.push(weight);
    }
    if let Some(style) = slant_style_label(desc.style()) {
        parts.push(style);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn weight_style_label(weight: pango::Weight) -> Option<&'static str> {
    match weight {
        pango::Weight::Thin => Some("Thin"),
        pango::Weight::Ultralight => Some("UltraLight"),
        pango::Weight::Light => Some("Light"),
        pango::Weight::Semilight => Some("SemiLight"),
        pango::Weight::Book | pango::Weight::Normal => None,
        pango::Weight::Medium => Some("Medium"),
        pango::Weight::Semibold => Some("SemiBold"),
        pango::Weight::Bold => Some("Bold"),
        pango::Weight::Ultrabold => Some("UltraBold"),
        pango::Weight::Heavy => Some("Heavy"),
        pango::Weight::Ultraheavy => Some("UltraHeavy"),
        _ => None,
    }
}

fn slant_style_label(style: pango::Style) -> Option<&'static str> {
    match style {
        pango::Style::Normal => None,
        pango::Style::Italic => Some("Italic"),
        pango::Style::Oblique => Some("Oblique"),
        _ => None,
    }
}

fn stretch_style_label(stretch: pango::Stretch) -> Option<&'static str> {
    match stretch {
        pango::Stretch::UltraCondensed => Some("UltraCondensed"),
        pango::Stretch::ExtraCondensed => Some("ExtraCondensed"),
        pango::Stretch::Condensed => Some("Condensed"),
        pango::Stretch::SemiCondensed => Some("SemiCondensed"),
        pango::Stretch::Normal => None,
        pango::Stretch::SemiExpanded => Some("SemiExpanded"),
        pango::Stretch::Expanded => Some("Expanded"),
        pango::Stretch::ExtraExpanded => Some("ExtraExpanded"),
        pango::Stretch::UltraExpanded => Some("UltraExpanded"),
        _ => None,
    }
}

fn is_generic_family(family: &str) -> bool {
    matches!(
        family.trim().to_ascii_lowercase().as_str(),
        "sans" | "sans-serif" | "serif" | "monospace" | "system-ui"
    )
}

fn is_regular_style(style: &str) -> bool {
    matches!(
        normalized_font_name(style).as_str(),
        "regular" | "book" | "roman" | "normal"
    )
}

fn normalized_font_name(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|ch| ch.is_alphanumeric())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_title_font_preserves_style_and_size() {
        let font = parse_title_font("DejaVu Sans Mono Bold Oblique 54");
        assert_eq!(
            font.normalized_description(),
            "DejaVu Sans Mono Bold Oblique 54"
        );
        assert_eq!(font.size_points(), 54.0);
        assert_eq!(
            font.fontconfig_pattern(),
            "DejaVu Sans Mono:weight=bold:slant=oblique"
        );
    }

    #[test]
    fn build_preview_title_font_desc_uses_scaled_size() {
        let desc = build_matched_preview_desc(Some("Noto Sans"), Some("Bold"), 18.0)
            .expect("matched preview description");
        assert_eq!(desc, "Noto Sans Bold 18");
    }

    #[test]
    fn parse_fc_match_record_reads_file_family_and_style() {
        let parsed =
            parse_fc_match_record("/tmp/font.ttf\u{1f}Liberation Sans Narrow\u{1f}Bold Italic")
                .expect("parsed fc-match record");
        assert_eq!(parsed.file, "/tmp/font.ttf");
        assert_eq!(parsed.family.as_deref(), Some("Liberation Sans Narrow"));
        assert_eq!(parsed.style.as_deref(), Some("Bold Italic"));
    }

    #[test]
    fn evaluate_match_kind_treats_missing_family_as_fallback() {
        let spec = parse_title_font("Definitely Missing Font Bold 54");
        let record = FcMatchRecord {
            file: "/tmp/font.ttf".to_string(),
            family: Some("Noto Sans".to_string()),
            style: Some("Bold".to_string()),
        };
        assert_eq!(
            evaluate_match_kind(&spec, &record),
            DrawtextFontMatchKind::Fallback
        );
    }

    #[test]
    fn evaluate_match_kind_accepts_generic_sans_family() {
        let spec = parse_title_font("Sans Bold 36");
        let record = FcMatchRecord {
            file: "/tmp/font.ttf".to_string(),
            family: Some("Noto Sans".to_string()),
            style: Some("Bold".to_string()),
        };
        assert_eq!(
            evaluate_match_kind(&spec, &record),
            DrawtextFontMatchKind::Exact
        );
    }
}
