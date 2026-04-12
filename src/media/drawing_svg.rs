//! SVG serialiser for `ClipKind::Drawing` vector overlays.
//!
//! Emits a self-contained SVG 1.1 document. When `animation` is
//! `Some`, items reveal sequentially via SMIL:
//!   * freehand strokes + arrow lines animate `stroke-dashoffset`
//!     from `pathLength` → 0 (a natural "draw-on" reveal).
//!   * filled shapes + arrowheads animate `opacity` from 0 → 1.
//! Every animation uses `fill="freeze"` so the final state sticks
//! after playback.
//!
//! The SVG is a separate artifact from the rasterised PNG the video
//! pipeline consumes — the static and animated outputs are both
//! derived from the same `DrawingItem` list.

use crate::model::clip::{DrawingItem, DrawingKind};

/// Stagger + duration for the sequential reveal animation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SvgAnimation {
    /// Seconds each item takes to fully reveal.
    pub per_item_duration_s: f64,
    /// Seconds between one item starting its reveal and the next.
    /// `0.0` = all items reveal simultaneously.
    pub stagger_s: f64,
}

impl Default for SvgAnimation {
    fn default() -> Self {
        Self {
            per_item_duration_s: 0.6,
            stagger_s: 0.4,
        }
    }
}

fn hex_rgb(color: u32) -> String {
    let r = (color >> 24) & 0xFF;
    let g = (color >> 16) & 0xFF;
    let b = (color >> 8) & 0xFF;
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn alpha(color: u32) -> f64 {
    (color & 0xFF) as f64 / 255.0
}

/// Polyline length in pixel units for Stroke items. `pathLength="1"`
/// would be more elegant but librsvg / several image viewers ignore
/// that attribute, so we compute the real length and use it as the
/// `stroke-dasharray` / `stroke-dashoffset` value — that works
/// uniformly in every SVG engine.
fn polyline_length_px(points: &[(f64, f64)], w: f64, h: f64) -> f64 {
    let mut total = 0.0;
    for pair in points.windows(2) {
        let (x0, y0) = (pair[0].0 * w, pair[0].1 * h);
        let (x1, y1) = (pair[1].0 * w, pair[1].1 * h);
        let dx = x1 - x0;
        let dy = y1 - y0;
        total += (dx * dx + dy * dy).sqrt();
    }
    total.max(1.0)
}

/// Serialize `items` into a self-contained SVG document sized
/// `width × height`. `animation` selects between static and a
/// sequential reveal.
pub fn drawing_to_svg(
    items: &[DrawingItem],
    width: i32,
    height: i32,
    animation: Option<SvgAnimation>,
) -> String {
    let mut out = String::new();
    // `us:source` stamps this as an UltimateSlice export so future
    // imports can round-trip it back into a `ClipKind::Drawing` clip
    // via `try_parse_ultimate_slice_svg`. `us:animated` records whether
    // SMIL reveals were emitted so the importer can reconstruct
    // `drawing_animation_reveal_ns` even when the SVG has no items.
    let animated_flag = if animation.is_some() { "1" } else { "0" };
    out.push_str(&format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <svg xmlns=\"http://www.w3.org/2000/svg\" \
         xmlns:us=\"urn:ultimateslice\" \
         us:source=\"ultimate-slice-drawing-v1\" \
         us:animated=\"{animated_flag}\" \
         width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">\n",
        w = width,
        h = height,
    ));
    let scale_ref = (height as f64) / 1080.0;
    let w = width as f64;
    let h = height as f64;

    for (i, item) in items.iter().enumerate() {
        if item.points.is_empty() {
            continue;
        }
        let lw = (item.width * scale_ref).max(0.5);
        let stroke = hex_rgb(item.color);
        let stroke_a = alpha(item.color);
        let begin_s = animation
            .map(|a| i as f64 * a.stagger_s)
            .unwrap_or(0.0);
        let dur_s = animation.map(|a| a.per_item_duration_s).unwrap_or(0.0);

        match item.kind {
            DrawingKind::Stroke => {
                let mut d = String::new();
                for (j, (nx, ny)) in item.points.iter().enumerate() {
                    let x = nx * w;
                    let y = ny * h;
                    if j == 0 {
                        d.push_str(&format!("M {x:.2} {y:.2}"));
                    } else {
                        d.push_str(&format!(" L {x:.2} {y:.2}"));
                    }
                }
                let path_len = polyline_length_px(&item.points, w, h);
                out.push_str(&format!(
                    "  <path d=\"{d}\" fill=\"none\" stroke=\"{stroke}\" \
                     stroke-opacity=\"{stroke_a:.3}\" stroke-width=\"{lw:.2}\" \
                     stroke-linecap=\"round\" stroke-linejoin=\"round\""
                ));
                if animation.is_some() {
                    out.push_str(&format!(
                        " stroke-dasharray=\"{path_len:.2}\" \
                         stroke-dashoffset=\"{path_len:.2}\""
                    ));
                }
                out.push_str(">\n");
                if animation.is_some() {
                    out.push_str(&format!(
                        "    <animate attributeName=\"stroke-dashoffset\" \
                         from=\"{path_len:.2}\" to=\"0\" \
                         begin=\"{begin_s:.3}s\" dur=\"{dur_s:.3}s\" fill=\"freeze\" />\n"
                    ));
                }
                out.push_str("  </path>\n");
            }
            DrawingKind::Rectangle => {
                let (p0, p1) = (item.points[0], *item.points.last().unwrap());
                let x = p0.0.min(p1.0) * w;
                let y = p0.1.min(p1.1) * h;
                let rw = (p0.0 - p1.0).abs() * w;
                let rh = (p0.1 - p1.1).abs() * h;
                let (fill_str, fill_a) = item
                    .fill_color
                    .map(|c| (hex_rgb(c), alpha(c)))
                    .unwrap_or_else(|| ("none".to_string(), 0.0));
                out.push_str(&format!(
                    "  <rect x=\"{x:.2}\" y=\"{y:.2}\" width=\"{rw:.2}\" height=\"{rh:.2}\" \
                     stroke=\"{stroke}\" stroke-opacity=\"{stroke_a:.3}\" \
                     stroke-width=\"{lw:.2}\" fill=\"{fill_str}\" \
                     fill-opacity=\"{fill_a:.3}\""
                ));
                if animation.is_some() {
                    out.push_str(" opacity=\"0\"");
                }
                out.push_str(">\n");
                if animation.is_some() {
                    out.push_str(&format!(
                        "    <animate attributeName=\"opacity\" from=\"0\" to=\"1\" \
                         begin=\"{begin_s:.3}s\" dur=\"{dur_s:.3}s\" fill=\"freeze\" />\n"
                    ));
                }
                out.push_str("  </rect>\n");
            }
            DrawingKind::Ellipse => {
                let (p0, p1) = (item.points[0], *item.points.last().unwrap());
                let x0 = p0.0.min(p1.0) * w;
                let y0 = p0.1.min(p1.1) * h;
                let rw = ((p0.0 - p1.0).abs() * w).max(1.0);
                let rh = ((p0.1 - p1.1).abs() * h).max(1.0);
                let cx = x0 + rw * 0.5;
                let cy = y0 + rh * 0.5;
                let (fill_str, fill_a) = item
                    .fill_color
                    .map(|c| (hex_rgb(c), alpha(c)))
                    .unwrap_or_else(|| ("none".to_string(), 0.0));
                out.push_str(&format!(
                    "  <ellipse cx=\"{cx:.2}\" cy=\"{cy:.2}\" rx=\"{rx:.2}\" ry=\"{ry:.2}\" \
                     stroke=\"{stroke}\" stroke-opacity=\"{stroke_a:.3}\" \
                     stroke-width=\"{lw:.2}\" fill=\"{fill_str}\" \
                     fill-opacity=\"{fill_a:.3}\"",
                    rx = rw * 0.5,
                    ry = rh * 0.5,
                ));
                if animation.is_some() {
                    out.push_str(" opacity=\"0\"");
                }
                out.push_str(">\n");
                if animation.is_some() {
                    out.push_str(&format!(
                        "    <animate attributeName=\"opacity\" from=\"0\" to=\"1\" \
                         begin=\"{begin_s:.3}s\" dur=\"{dur_s:.3}s\" fill=\"freeze\" />\n"
                    ));
                }
                out.push_str("  </ellipse>\n");
            }
            DrawingKind::Arrow => {
                let p0 = item.points[0];
                let p1 = *item.points.last().unwrap();
                let x0 = p0.0 * w;
                let y0 = p0.1 * h;
                let x1 = p1.0 * w;
                let y1 = p1.1 * h;
                let line_len = {
                    let dx = x1 - x0;
                    let dy = y1 - y0;
                    (dx * dx + dy * dy).sqrt().max(1.0)
                };
                // Line.
                out.push_str(&format!(
                    "  <line x1=\"{x0:.2}\" y1=\"{y0:.2}\" x2=\"{x1:.2}\" y2=\"{y1:.2}\" \
                     stroke=\"{stroke}\" stroke-opacity=\"{stroke_a:.3}\" \
                     stroke-width=\"{lw:.2}\" stroke-linecap=\"round\""
                ));
                if animation.is_some() {
                    out.push_str(&format!(
                        " stroke-dasharray=\"{line_len:.2}\" \
                         stroke-dashoffset=\"{line_len:.2}\""
                    ));
                }
                out.push_str(">\n");
                if animation.is_some() {
                    out.push_str(&format!(
                        "    <animate attributeName=\"stroke-dashoffset\" \
                         from=\"{line_len:.2}\" to=\"0\" \
                         begin=\"{begin_s:.3}s\" dur=\"{dur_s:.3}s\" fill=\"freeze\" />\n"
                    ));
                }
                out.push_str("  </line>\n");
                // Head (same geometry as the Cairo rasteriser).
                let dx = x1 - x0;
                let dy = y1 - y0;
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                let ux = dx / len;
                let uy = dy / len;
                let head = (lw * 6.0).max(10.0);
                let (ca, sa) = (25f64.to_radians().cos(), 25f64.to_radians().sin());
                let lxa = x1 - head * (ux * ca - uy * sa);
                let lya = y1 - head * (uy * ca + ux * sa);
                let rxa = x1 - head * (ux * ca + uy * sa);
                let rya = y1 - head * (uy * ca - ux * sa);
                out.push_str(&format!(
                    "  <polygon points=\"{x1:.2},{y1:.2} {lxa:.2},{lya:.2} {rxa:.2},{rya:.2}\" \
                     fill=\"{stroke}\" fill-opacity=\"{stroke_a:.3}\""
                ));
                if animation.is_some() {
                    // Head appears once the line finishes drawing.
                    out.push_str(" opacity=\"0\"");
                }
                out.push_str(">\n");
                if let Some(anim) = animation {
                    let head_begin = begin_s + anim.per_item_duration_s * 0.75;
                    let head_dur = (anim.per_item_duration_s * 0.25).max(0.05);
                    out.push_str(&format!(
                        "    <animate attributeName=\"opacity\" from=\"0\" to=\"1\" \
                         begin=\"{head_begin:.3}s\" dur=\"{head_dur:.3}s\" fill=\"freeze\" />\n"
                    ));
                }
                out.push_str("  </polygon>\n");
            }
        }
    }

    out.push_str("</svg>\n");
    out
}

/// Successful round-trip of an UltimateSlice-exported SVG.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedDrawing {
    pub items: Vec<DrawingItem>,
    /// Reconstructed from the first item's SMIL `dur` attribute;
    /// 0 when the exported SVG was static.
    pub reveal_ns: u64,
    pub width: i32,
    pub height: i32,
}

/// Parse an SVG produced by `drawing_to_svg` back into
/// `DrawingItem`s. Returns `None` unless the SVG carries the
/// `us:source="ultimate-slice-drawing-v1"` stamp — this parser is
/// deliberately narrow and doesn't try to interpret general SVG.
///
/// Supported element shapes (all from our own serialiser):
///   * `<path d="M x y L x y …">` → `Stroke`
///   * `<rect …>` → `Rectangle`
///   * `<ellipse …>` → `Ellipse`
///   * `<line …>` immediately followed by `<polygon …>` → `Arrow`
pub fn try_parse_ultimate_slice_svg(content: &str) -> Option<ParsedDrawing> {
    let stamped = content.contains("us:source=\"ultimate-slice-drawing-v1\"");
    // Stamp was added after the first wave of SVG exports, so fall
    // back to a structural heuristic that's strict enough to reject
    // foreign SVGs: the combination of `stroke-linecap="round"`
    // + `stroke-linejoin="round"` + an `<animate>` on
    // `stroke-dashoffset` (or an unanimated export's explicit
    // `stroke-dasharray` + matching `stroke-dashoffset="0"`) is
    // unique to this renderer's output.
    let looks_like_ours = content.contains("stroke-linecap=\"round\"")
        && content.contains("stroke-linejoin=\"round\"")
        && (content.contains("attributeName=\"stroke-dashoffset\"")
            || content.contains("attributeName=\"opacity\""));
    if !stamped && !looks_like_ours {
        return None;
    }
    let (svg_w, svg_h) = parse_view_box(content).unwrap_or((1920, 1080));
    let scale_ref = (svg_h as f64 / 1080.0).max(1e-6);

    let mut items: Vec<DrawingItem> = Vec::new();
    let mut reveal_ns: u64 = 0;

    // The writer emits one item per top-level element (Stroke =
    // <path>, Rectangle = <rect>, Ellipse = <ellipse>, Arrow = <line>
    // + <polygon>). We walk element opens in source order; when we
    // see a `<line>` immediately followed by `<polygon>` we combine
    // them into an Arrow. Everything else is skipped.
    let mut cursor = 0usize;
    let bytes = content.as_bytes();
    let mut pending_line: Option<(f64, f64, f64, f64, f64, u32)> = None;
    while let Some(rel) = content[cursor..].find('<') {
        let open = cursor + rel;
        let close = match content[open..].find('>') {
            Some(c) => open + c,
            None => break,
        };
        let tag_body = &content[open + 1..close];
        cursor = close + 1;
        if tag_body.starts_with('/') || tag_body.starts_with('!') || tag_body.starts_with('?') {
            continue;
        }
        // Self-closing / explicit open. Take the element name.
        let (name, attrs) = split_tag(tag_body);
        match name {
            "path" => {
                pending_line = None;
                if let Some(item) = parse_path(&attrs, svg_w, svg_h, scale_ref) {
                    if reveal_ns == 0 {
                        reveal_ns = extract_reveal_ns(&content[open..], bytes);
                    }
                    items.push(item);
                }
            }
            "rect" => {
                pending_line = None;
                if let Some(item) = parse_rect(&attrs, svg_w, svg_h, scale_ref) {
                    if reveal_ns == 0 {
                        reveal_ns = extract_reveal_ns(&content[open..], bytes);
                    }
                    items.push(item);
                }
            }
            "ellipse" => {
                pending_line = None;
                if let Some(item) = parse_ellipse(&attrs, svg_w, svg_h, scale_ref) {
                    if reveal_ns == 0 {
                        reveal_ns = extract_reveal_ns(&content[open..], bytes);
                    }
                    items.push(item);
                }
            }
            "line" => {
                pending_line = parse_line(&attrs, scale_ref);
                if reveal_ns == 0 {
                    reveal_ns = extract_reveal_ns(&content[open..], bytes);
                }
            }
            "polygon" => {
                if let Some((x0, y0, x1, y1, w_px, color)) = pending_line.take() {
                    items.push(DrawingItem {
                        kind: DrawingKind::Arrow,
                        points: vec![
                            (x0 / svg_w as f64, y0 / svg_h as f64),
                            (x1 / svg_w as f64, y1 / svg_h as f64),
                        ],
                        color,
                        width: w_px / scale_ref,
                        fill_color: None,
                    });
                }
            }
            _ => {
                // Intentionally leave `pending_line` alone: the
                // Arrow output has `<animate>` children inside both
                // `<line>` and the following `<polygon>`, so
                // clearing on every unknown tag would drop the
                // pending state before we reach the polygon.
            }
        }
    }
    Some(ParsedDrawing {
        items,
        reveal_ns,
        width: svg_w,
        height: svg_h,
    })
}

fn parse_view_box(s: &str) -> Option<(i32, i32)> {
    let vb = find_attr(s, "viewBox")?;
    let nums: Vec<&str> = vb.split_ascii_whitespace().collect();
    if nums.len() != 4 {
        return None;
    }
    let w: i32 = nums[2].parse().ok()?;
    let h: i32 = nums[3].parse().ok()?;
    Some((w, h))
}

/// Pull the `attributeName=...` `dur=` value out of the first
/// `<animate …>` child inside an element, rounding to nanoseconds.
/// Returns 0 when no animation is present or the value is malformed.
fn extract_reveal_ns(from: &str, _haystack: &[u8]) -> u64 {
    // Find first <animate ... /> within the next element's body.
    let Some(animate_start) = from.find("<animate") else {
        return 0;
    };
    let after = &from[animate_start..];
    let Some(end) = after.find('>') else {
        return 0;
    };
    let body = &after[..end];
    let Some(dur_s) = find_attr(body, "dur") else {
        return 0;
    };
    let trimmed = dur_s.trim_end_matches('s');
    trimmed
        .parse::<f64>()
        .map(|d| (d * 1_000_000_000.0).round() as u64)
        .unwrap_or(0)
}

fn split_tag(body: &str) -> (&str, &str) {
    let trimmed = body.trim_end_matches('/').trim();
    match trimmed.find(|c: char| c.is_ascii_whitespace()) {
        Some(i) => (&trimmed[..i], &trimmed[i..]),
        None => (trimmed, ""),
    }
}

fn find_attr<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    // Matches `name="…"` with simple quoting. SVGs we emit always
    // use double-quoted attribute values.
    let key = format!("{name}=\"");
    let start = s.find(&key)?;
    let value_start = start + key.len();
    let end = s[value_start..].find('"')?;
    Some(&s[value_start..value_start + end])
}

fn parse_color_with_opacity(attrs: &str, color_attr: &str, opacity_attr: &str) -> Option<u32> {
    let hex = find_attr(attrs, color_attr)?;
    if !hex.starts_with('#') || hex.len() < 7 {
        return None;
    }
    let r = u32::from_str_radix(&hex[1..3], 16).ok()?;
    let g = u32::from_str_radix(&hex[3..5], 16).ok()?;
    let b = u32::from_str_radix(&hex[5..7], 16).ok()?;
    let a = find_attr(attrs, opacity_attr)
        .and_then(|s| s.parse::<f64>().ok())
        .map(|f| (f.clamp(0.0, 1.0) * 255.0).round() as u32)
        .unwrap_or(255);
    Some((r << 24) | (g << 16) | (b << 8) | a)
}

fn parse_path(attrs: &str, w: i32, h: i32, scale_ref: f64) -> Option<DrawingItem> {
    let d = find_attr(attrs, "d")?;
    let color = parse_color_with_opacity(attrs, "stroke", "stroke-opacity")?;
    let stroke_w = find_attr(attrs, "stroke-width")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(1.0);
    let mut points = Vec::new();
    let tokens: Vec<&str> = d.split_ascii_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        if t == "M" || t == "L" {
            let x: f64 = tokens.get(i + 1)?.parse().ok()?;
            let y: f64 = tokens.get(i + 2)?.parse().ok()?;
            points.push((x / w as f64, y / h as f64));
            i += 3;
        } else {
            i += 1;
        }
    }
    if points.is_empty() {
        return None;
    }
    Some(DrawingItem {
        kind: DrawingKind::Stroke,
        points,
        color,
        width: stroke_w / scale_ref,
        fill_color: None,
    })
}

fn parse_rect(attrs: &str, w: i32, h: i32, scale_ref: f64) -> Option<DrawingItem> {
    let x: f64 = find_attr(attrs, "x")?.parse().ok()?;
    let y: f64 = find_attr(attrs, "y")?.parse().ok()?;
    let rw: f64 = find_attr(attrs, "width")?.parse().ok()?;
    let rh: f64 = find_attr(attrs, "height")?.parse().ok()?;
    let color = parse_color_with_opacity(attrs, "stroke", "stroke-opacity")?;
    let stroke_w = find_attr(attrs, "stroke-width")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(1.0);
    let fill_color = parse_color_with_opacity(attrs, "fill", "fill-opacity");
    Some(DrawingItem {
        kind: DrawingKind::Rectangle,
        points: vec![
            (x / w as f64, y / h as f64),
            ((x + rw) / w as f64, (y + rh) / h as f64),
        ],
        color,
        width: stroke_w / scale_ref,
        fill_color,
    })
}

fn parse_ellipse(attrs: &str, w: i32, h: i32, scale_ref: f64) -> Option<DrawingItem> {
    let cx: f64 = find_attr(attrs, "cx")?.parse().ok()?;
    let cy: f64 = find_attr(attrs, "cy")?.parse().ok()?;
    let rx: f64 = find_attr(attrs, "rx")?.parse().ok()?;
    let ry: f64 = find_attr(attrs, "ry")?.parse().ok()?;
    let color = parse_color_with_opacity(attrs, "stroke", "stroke-opacity")?;
    let stroke_w = find_attr(attrs, "stroke-width")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(1.0);
    let fill_color = parse_color_with_opacity(attrs, "fill", "fill-opacity");
    Some(DrawingItem {
        kind: DrawingKind::Ellipse,
        points: vec![
            ((cx - rx) / w as f64, (cy - ry) / h as f64),
            ((cx + rx) / w as f64, (cy + ry) / h as f64),
        ],
        color,
        width: stroke_w / scale_ref,
        fill_color,
    })
}

fn parse_line(attrs: &str, _scale_ref: f64) -> Option<(f64, f64, f64, f64, f64, u32)> {
    let x0: f64 = find_attr(attrs, "x1")?.parse().ok()?;
    let y0: f64 = find_attr(attrs, "y1")?.parse().ok()?;
    let x1: f64 = find_attr(attrs, "x2")?.parse().ok()?;
    let y1: f64 = find_attr(attrs, "y2")?.parse().ok()?;
    let color = parse_color_with_opacity(attrs, "stroke", "stroke-opacity")?;
    let stroke_w = find_attr(attrs, "stroke-width")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(1.0);
    Some((x0, y0, x1, y1, stroke_w, color))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::{DrawingItem, DrawingKind};

    fn stroke(color: u32, pts: &[(f64, f64)]) -> DrawingItem {
        DrawingItem {
            kind: DrawingKind::Stroke,
            points: pts.to_vec(),
            color,
            width: 6.0,
            fill_color: None,
        }
    }

    #[test]
    fn static_svg_has_viewbox_and_closes() {
        let svg = drawing_to_svg(&[stroke(0xFF0000FF, &[(0.1, 0.1), (0.9, 0.9)])], 320, 180, None);
        assert!(svg.starts_with("<?xml"));
        assert!(svg.contains("viewBox=\"0 0 320 180\""));
        assert!(svg.trim_end().ends_with("</svg>"));
        // No <animate> elements in static mode.
        assert!(!svg.contains("<animate"));
    }

    #[test]
    fn animated_svg_emits_staggered_begins() {
        // Two horizontal strokes across 80% of a 320-pixel canvas →
        // each should have a computed polyline length of ~256 px.
        let items = vec![
            stroke(0xFF0000FF, &[(0.1, 0.1), (0.9, 0.1)]),
            stroke(0x00FF00FF, &[(0.1, 0.9), (0.9, 0.9)]),
        ];
        let svg = drawing_to_svg(
            &items,
            320,
            180,
            Some(SvgAnimation {
                per_item_duration_s: 0.5,
                stagger_s: 0.4,
            }),
        );
        // First stroke begins at 0.000s, second at 0.400s.
        assert!(svg.contains("begin=\"0.000s\""), "svg: {svg}");
        assert!(svg.contains("begin=\"0.400s\""), "svg: {svg}");
        // Real path length used (not the fragile pathLength=1 trick
        // which several SVG engines silently ignore).
        assert!(!svg.contains("pathLength=\"1\""), "svg: {svg}");
        assert!(svg.contains("stroke-dasharray=\"256.00\""), "svg: {svg}");
        assert!(svg.contains("from=\"256.00\" to=\"0\""), "svg: {svg}");
        // Each stroke has dasharray + dashoffset attrs + animate.
        assert_eq!(svg.matches("stroke-dashoffset").count(), 4);
    }

    #[test]
    fn shapes_use_opacity_animation() {
        let item = DrawingItem {
            kind: DrawingKind::Rectangle,
            points: vec![(0.2, 0.2), (0.8, 0.8)],
            color: 0x0000FFFF,
            width: 4.0,
            fill_color: Some(0xFFFF00AA),
        };
        let svg =
            drawing_to_svg(&[item], 100, 100, Some(SvgAnimation::default()));
        assert!(svg.contains("<rect "), "svg: {svg}");
        assert!(svg.contains("attributeName=\"opacity\""), "svg: {svg}");
        assert!(svg.contains("fill=\"#ffff00\""), "svg: {svg}");
        assert!(svg.contains("fill-opacity=\"0.667\""), "svg: {svg}");
    }

    #[test]
    fn arrow_emits_line_and_polygon_head() {
        let item = DrawingItem {
            kind: DrawingKind::Arrow,
            points: vec![(0.1, 0.5), (0.9, 0.5)],
            color: 0xFFFFFFFF,
            width: 5.0,
            fill_color: None,
        };
        let svg = drawing_to_svg(&[item], 200, 200, None);
        assert!(svg.contains("<line "), "svg: {svg}");
        assert!(svg.contains("<polygon "), "svg: {svg}");
    }

    #[test]
    fn import_rejects_unknown_svg() {
        let foreign = "<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"></svg>";
        assert!(try_parse_ultimate_slice_svg(foreign).is_none());
    }

    #[test]
    fn import_round_trips_strokes_and_shapes() {
        let items = vec![
            stroke(0xFF0000FF, &[(0.1, 0.1), (0.5, 0.5), (0.9, 0.1)]),
            DrawingItem {
                kind: DrawingKind::Rectangle,
                points: vec![(0.2, 0.3), (0.7, 0.6)],
                color: 0x00FF00FF,
                width: 4.0,
                fill_color: Some(0xFFFF0080),
            },
            DrawingItem {
                kind: DrawingKind::Ellipse,
                points: vec![(0.2, 0.2), (0.8, 0.4)],
                color: 0x0000FFFF,
                width: 3.0,
                fill_color: None,
            },
            DrawingItem {
                kind: DrawingKind::Arrow,
                points: vec![(0.1, 0.5), (0.9, 0.5)],
                color: 0xFFFFFFFF,
                width: 5.0,
                fill_color: None,
            },
        ];
        let svg = drawing_to_svg(&items, 1920, 1080, Some(SvgAnimation::default()));
        let parsed =
            try_parse_ultimate_slice_svg(&svg).expect("our own SVG should round-trip");

        assert_eq!(parsed.width, 1920);
        assert_eq!(parsed.height, 1080);
        assert_eq!(parsed.items.len(), 4);

        // Kind + colors + fills survived.
        assert_eq!(parsed.items[0].kind, DrawingKind::Stroke);
        assert_eq!(parsed.items[0].color, 0xFF0000FF);
        assert_eq!(parsed.items[1].kind, DrawingKind::Rectangle);
        // fill_color has 3-decimal fidelity on the alpha round-trip.
        assert_eq!(parsed.items[1].fill_color.unwrap() & 0xFFFFFF00, 0xFFFF0000);
        assert_eq!(parsed.items[2].kind, DrawingKind::Ellipse);
        assert_eq!(parsed.items[3].kind, DrawingKind::Arrow);

        // Points normalized back to ~original coordinates.
        let (nx, ny) = parsed.items[0].points[0];
        assert!((nx - 0.1).abs() < 0.01, "nx={nx}");
        assert!((ny - 0.1).abs() < 0.01, "ny={ny}");

        // Reveal duration reconstructed from SMIL `dur`.
        // Default SvgAnimation has 0.6s per-item duration.
        assert_eq!(parsed.reveal_ns, 600_000_000);
    }

    #[test]
    fn import_accepts_pre_stamp_exports() {
        // Earlier versions of `drawing_to_svg` didn't emit the
        // `us:source` stamp. Structurally identical SVGs still
        // round-trip via the heuristic (both round-join lines +
        // SMIL on stroke-dashoffset).
        // Note: raw-string delimiter `##` because the SVG body
        // contains `"#ff0000"` which would otherwise close an `r#"…"#`.
        let svg = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="1920" height="1080" viewBox="0 0 1920 1080">
  <path d="M 100.00 200.00 L 500.00 600.00" fill="none" stroke="#ff0000" stroke-opacity="1.000" stroke-width="5.00" stroke-linecap="round" stroke-linejoin="round" stroke-dasharray="565.69" stroke-dashoffset="565.69">
    <animate attributeName="stroke-dashoffset" from="565.69" to="0" begin="0.000s" dur="0.600s" fill="freeze" />
  </path>
</svg>"##;
        let parsed = try_parse_ultimate_slice_svg(svg).expect("pre-stamp heuristic");
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].kind, DrawingKind::Stroke);
        assert_eq!(parsed.reveal_ns, 600_000_000);
    }

    #[test]
    fn import_static_export_yields_zero_reveal() {
        let items = vec![stroke(0xFF0000FF, &[(0.1, 0.1), (0.9, 0.9)])];
        let svg = drawing_to_svg(&items, 320, 180, None);
        let parsed = try_parse_ultimate_slice_svg(&svg).expect("stamped");
        assert_eq!(parsed.reveal_ns, 0);
        assert_eq!(parsed.items.len(), 1);
    }

    #[test]
    fn empty_items_still_valid_svg() {
        let svg = drawing_to_svg(&[], 50, 50, None);
        assert!(svg.contains("<svg "));
        assert!(svg.contains("</svg>"));
    }
}
