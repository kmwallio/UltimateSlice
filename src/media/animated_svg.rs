use anyhow::{anyhow, Context, Result};
use quick_xml::escape::escape;
use resvg::{tiny_skia, usvg};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;

pub const DEFAULT_ANIMATED_SVG_DURATION_NS: u64 = 4_000_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvgAnimationAnalysis {
    pub is_animated: bool,
    pub duration_ns: Option<u64>,
}

impl Default for SvgAnimationAnalysis {
    fn default() -> Self {
        Self {
            is_animated: false,
            duration_ns: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RenderedSvgFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug)]
struct AnimatedSvgRenderResult {
    render_key: String,
    render_path: String,
    success: bool,
}

type AnimatedSvgWorkItem = (String, u64, u64, Option<u64>, u32, u32);

pub struct AnimatedSvgCache {
    pub paths: HashMap<String, String>,
    pending: HashSet<String>,
    failed: HashSet<String>,
    result_rx: mpsc::Receiver<AnimatedSvgRenderResult>,
    work_tx: Option<mpsc::Sender<AnimatedSvgWorkItem>>,
    local_cache_root: PathBuf,
    ffprobe_path: Option<String>,
}

impl AnimatedSvgCache {
    pub fn new() -> Self {
        let local_cache_root = animated_svg_cache_root();
        let _ = std::fs::create_dir_all(&local_cache_root);
        let (result_tx, result_rx) = mpsc::sync_channel::<AnimatedSvgRenderResult>(32);
        let (work_tx, work_rx) = mpsc::channel::<AnimatedSvgWorkItem>();
        let work_rx = std::sync::Arc::new(std::sync::Mutex::new(work_rx));
        for _ in 0..2 {
            let rx = work_rx.clone();
            let tx = result_tx.clone();
            let root = local_cache_root.clone();
            std::thread::spawn(move || loop {
                let item = {
                    let lock = rx.lock().unwrap();
                    lock.recv()
                };
                let Ok((
                    source_path,
                    source_in_ns,
                    source_out_ns,
                    media_duration_ns,
                    fps_num,
                    fps_den,
                )) = item
                else {
                    break;
                };
                let key = animated_svg_render_key(
                    &source_path,
                    source_in_ns,
                    source_out_ns,
                    media_duration_ns,
                    fps_num,
                    fps_den,
                );
                let render_path = render_output_path_for(&key, &root);
                let success = render_animated_svg_clip(
                    &source_path,
                    &render_path,
                    source_in_ns,
                    source_out_ns,
                    media_duration_ns,
                    fps_num,
                    fps_den,
                )
                .is_ok();
                if tx
                    .send(AnimatedSvgRenderResult {
                        render_key: key,
                        render_path,
                        success,
                    })
                    .is_err()
                {
                    break;
                }
            });
        }
        Self {
            paths: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            result_rx,
            work_tx: Some(work_tx),
            local_cache_root,
            ffprobe_path: find_ffprobe_path(),
        }
    }

    pub fn request(
        &mut self,
        source_path: &str,
        source_in_ns: u64,
        source_out_ns: u64,
        media_duration_ns: Option<u64>,
        fps_num: u32,
        fps_den: u32,
    ) {
        let key = animated_svg_render_key(
            source_path,
            source_in_ns,
            source_out_ns,
            media_duration_ns,
            fps_num,
            fps_den,
        );
        if self.paths.contains_key(&key)
            || self.pending.contains(&key)
            || self.failed.contains(&key)
        {
            return;
        }
        if let Some(existing) = existing_render_path_for(
            source_path,
            source_in_ns,
            source_out_ns,
            media_duration_ns,
            fps_num,
            fps_den,
            &self.local_cache_root,
            self.ffprobe_path.as_deref(),
        ) {
            self.paths.insert(key, existing);
            return;
        }
        self.pending.insert(key);
        if let Some(ref tx) = self.work_tx {
            let _ = tx.send((
                source_path.to_string(),
                source_in_ns,
                source_out_ns,
                media_duration_ns,
                fps_num,
                fps_den,
            ));
        }
    }

    pub fn poll(&mut self) -> Vec<String> {
        let mut resolved = Vec::new();
        while let Ok(result) = self.result_rx.try_recv() {
            self.pending.remove(&result.render_key);
            if result.success {
                self.paths
                    .insert(result.render_key.clone(), result.render_path.clone());
                resolved.push(result.render_key);
            } else {
                self.failed.insert(result.render_key);
            }
        }
        resolved
    }
}

pub fn analyze_svg_path(path: &str) -> Result<SvgAnimationAnalysis> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read SVG source {}", path))?;
    analyze_svg_str(&text)
}

pub fn analyze_svg_str(svg: &str) -> Result<SvgAnimationAnalysis> {
    let doc = usvg::roxmltree::Document::parse(svg).context("invalid SVG XML")?;
    let mut max_duration_ns = None;
    let mut animated = false;
    for node in doc.descendants().filter(|node| node.is_element()) {
        if let Some(animation) = parse_animation_node(node) {
            animated = true;
            if let Some(end_ns) = animation.total_end_ns() {
                max_duration_ns = Some(max_duration_ns.unwrap_or(0).max(end_ns));
            }
        }
    }
    Ok(SvgAnimationAnalysis {
        is_animated: animated,
        duration_ns: if animated {
            Some(max_duration_ns.unwrap_or(DEFAULT_ANIMATED_SVG_DURATION_NS))
        } else {
            None
        },
    })
}

pub fn animated_svg_render_key(
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
    media_duration_ns: Option<u64>,
    fps_num: u32,
    fps_den: u32,
) -> String {
    let mut hasher = DefaultHasher::new();
    source_path.hash(&mut hasher);
    source_in_ns.hash(&mut hasher);
    source_out_ns.hash(&mut hasher);
    media_duration_ns.hash(&mut hasher);
    fps_num.hash(&mut hasher);
    fps_den.hash(&mut hasher);
    if let Ok(meta) = std::fs::metadata(source_path) {
        meta.len().hash(&mut hasher);
        if let Ok(modified) = meta.modified() {
            modified
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .hash(&mut hasher);
        }
    }
    format!("animated-svg-{:016x}", hasher.finish())
}

pub fn ensure_rendered_clip(
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
    media_duration_ns: Option<u64>,
    fps_num: u32,
    fps_den: u32,
) -> Result<String> {
    let root = animated_svg_cache_root();
    std::fs::create_dir_all(&root).ok();
    if let Some(path) = existing_render_path_for(
        source_path,
        source_in_ns,
        source_out_ns,
        media_duration_ns,
        fps_num,
        fps_den,
        &root,
        find_ffprobe_path().as_deref(),
    ) {
        return Ok(path);
    }
    let key = animated_svg_render_key(
        source_path,
        source_in_ns,
        source_out_ns,
        media_duration_ns,
        fps_num,
        fps_den,
    );
    let path = render_output_path_for(&key, &root);
    render_animated_svg_clip(
        source_path,
        &path,
        source_in_ns,
        source_out_ns,
        media_duration_ns,
        fps_num,
        fps_den,
    )?;
    Ok(path)
}

pub fn render_svg_frame_at_time(source_path: &str, time_ns: u64) -> Result<RenderedSvgFrame> {
    let svg = std::fs::read_to_string(source_path)
        .with_context(|| format!("failed to read SVG source {}", source_path))?;
    render_svg_frame_from_str(&svg, time_ns, Path::new(source_path).parent())
}

fn render_animated_svg_clip(
    source_path: &str,
    output_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
    media_duration_ns: Option<u64>,
    fps_num: u32,
    fps_den: u32,
) -> Result<()> {
    let ffmpeg = crate::media::export::find_ffmpeg()?;
    let clip_duration_ns = source_out_ns.saturating_sub(source_in_ns).max(1);
    let authored_end_ns = media_duration_ns.unwrap_or(source_out_ns);
    let animated_end_ns = source_out_ns.min(authored_end_ns);
    let motion_duration_ns = animated_end_ns.saturating_sub(source_in_ns);
    let last_motion_time_ns = if motion_duration_ns > 0 {
        animated_end_ns.saturating_sub(1)
    } else if authored_end_ns > source_in_ns {
        authored_end_ns.saturating_sub(1)
    } else {
        source_in_ns
    };
    let first_frame_time_ns = if motion_duration_ns > 0 {
        source_in_ns
    } else {
        last_motion_time_ns
    };
    let first_frame = render_svg_frame_at_time(source_path, first_frame_time_ns)?;
    let width = first_frame.width.max(1);
    let height = first_frame.height.max(1);
    let frame_times = frame_times_ns(clip_duration_ns, fps_num, fps_den);
    let temp_path = format!("{output_path}.partial");
    let parent = Path::new(output_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create animated SVG cache dir {}",
            parent.display()
        )
    })?;
    let _ = std::fs::remove_file(&temp_path);
    let mut encoder = Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            &format!("{width}x{height}"),
            "-r",
            &format!("{}/{}", fps_num.max(1), fps_den.max(1)),
            "-i",
            "pipe:0",
            "-c:v",
            "libvpx-vp9",
            "-pix_fmt",
            "yuva420p",
            "-crf",
            "30",
            "-b:v",
            "0",
            "-auto-alt-ref",
            "0",
            "-f",
            "webm",
            &temp_path,
        ])
        .stdin(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn animated SVG encoder")?;
    let stdin = encoder
        .stdin
        .take()
        .ok_or_else(|| anyhow!("animated SVG encoder stdin unavailable"))?;
    let mut writer = std::io::BufWriter::new(stdin);
    writer.write_all(&first_frame.rgba)?;
    for time_ns in frame_times.into_iter().skip(1) {
        let svg_time_ns = if motion_duration_ns == 0 || time_ns >= motion_duration_ns {
            last_motion_time_ns
        } else {
            source_in_ns + time_ns
        };
        let frame = render_svg_frame_at_time(source_path, svg_time_ns)?;
        if frame.width != width || frame.height != height {
            return Err(anyhow!(
                "animated SVG frame size changed from {}x{} to {}x{}",
                width,
                height,
                frame.width,
                frame.height
            ));
        }
        writer.write_all(&frame.rgba)?;
    }
    writer.flush()?;
    drop(writer);
    let status = encoder
        .wait()
        .context("waiting for animated SVG encoder failed")?;
    if !status.success() {
        let _ = std::fs::remove_file(&temp_path);
        return Err(anyhow!(
            "animated SVG encoder exited with status {}",
            status
        ));
    }
    std::fs::rename(&temp_path, output_path)
        .with_context(|| format!("failed to finalize animated SVG render {}", output_path))?;
    Ok(())
}

fn render_svg_frame_from_str(
    svg: &str,
    time_ns: u64,
    resources_dir: Option<&Path>,
) -> Result<RenderedSvgFrame> {
    let snapshot = snapshot_svg(svg, time_ns)?;
    let mut options = usvg::Options::default();
    if let Some(dir) = resources_dir {
        options.resources_dir = Some(dir.to_path_buf());
    }
    options.fontdb_mut().load_system_fonts();
    let tree = usvg::Tree::from_str(&snapshot, &options).context("failed to parse SVG snapshot")?;
    let size = tree.size();
    let width = size.width().ceil().max(1.0) as u32;
    let height = size.height().ceil().max(1.0) as u32;
    let mut pixmap =
        tiny_skia::Pixmap::new(width, height).ok_or_else(|| anyhow!("invalid SVG frame size"))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    Ok(RenderedSvgFrame {
        width,
        height,
        rgba: pixmap.data().to_vec(),
    })
}

fn snapshot_svg(svg: &str, time_ns: u64) -> Result<String> {
    let doc = usvg::roxmltree::Document::parse(svg).context("invalid SVG XML")?;
    let root = doc.root_element();
    let mut output = String::with_capacity(svg.len());
    render_svg_node(root, time_ns, &mut output)?;
    Ok(output)
}

fn render_svg_node(
    node: usvg::roxmltree::Node<'_, '_>,
    time_ns: u64,
    out: &mut String,
) -> Result<()> {
    if !node.is_element() {
        if let Some(text) = node.text() {
            out.push_str(&escape(text));
        }
        return Ok(());
    }
    let tag = node.tag_name().name();
    if is_animation_tag(tag) {
        return Ok(());
    }
    let mut attrs: Vec<(String, String)> = node
        .attributes()
        .map(|attr| (attr.name().to_string(), attr.value().to_string()))
        .collect();
    let mut style_map = parse_style_map(get_attr_value(&attrs, "style"));
    let base_transform = get_attr_value(&attrs, "transform")
        .unwrap_or("")
        .trim()
        .to_string();
    let mut animated_transform = None;
    for child in node.children().filter(|child| child.is_element()) {
        if let Some(animation) = parse_animation_node(child) {
            if let Some(value) = animation.value_at_ns(time_ns) {
                match animation {
                    ParsedAnimation::Attribute(ref attr_anim) => {
                        set_attr_value(&mut attrs, &attr_anim.attribute_name, &value);
                        if style_map.contains_key(attr_anim.attribute_name.as_str()) {
                            style_map.insert(attr_anim.attribute_name.clone(), value);
                        }
                    }
                    ParsedAnimation::Transform(_) => {
                        animated_transform = Some(value);
                    }
                }
            }
        }
    }
    if !style_map.is_empty() {
        set_attr_value(&mut attrs, "style", &serialize_style_map(&style_map));
    }
    if let Some(animated_transform) = animated_transform {
        let merged = if base_transform.is_empty() {
            animated_transform
        } else {
            format!("{base_transform} {animated_transform}")
        };
        set_attr_value(&mut attrs, "transform", &merged);
    }
    out.push('<');
    out.push_str(tag);
    for (name, value) in &attrs {
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        out.push_str(&escape(value));
        out.push('"');
    }
    let has_non_animation_children = node.children().any(|child| {
        !child.is_element()
            || child.tag_name().name().is_empty()
            || !is_animation_tag(child.tag_name().name())
    });
    if !has_non_animation_children {
        out.push_str("/>");
        return Ok(());
    }
    out.push('>');
    for child in node.children() {
        if child.is_element() && is_animation_tag(child.tag_name().name()) {
            continue;
        }
        render_svg_node(child, time_ns, out)?;
    }
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
    Ok(())
}

fn frame_times_ns(duration_ns: u64, fps_num: u32, fps_den: u32) -> Vec<u64> {
    let fps_num = fps_num.max(1) as u128;
    let fps_den = fps_den.max(1) as u128;
    let duration_ns = duration_ns as u128;
    let mut times = Vec::new();
    let mut frame_idx = 0u128;
    loop {
        let time_ns = frame_idx
            .saturating_mul(fps_den)
            .saturating_mul(1_000_000_000u128)
            / fps_num;
        if !times.is_empty() && time_ns >= duration_ns {
            break;
        }
        times.push(time_ns.min(duration_ns.saturating_sub(1)) as u64);
        frame_idx += 1;
        if duration_ns <= 1 {
            break;
        }
    }
    if times.is_empty() {
        times.push(0);
    }
    times
}

fn animated_svg_cache_root() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("ultimateslice")
        .join("animated_svg")
}

fn render_output_path_for(key: &str, root: &Path) -> String {
    root.join(format!("{key}.webm"))
        .to_string_lossy()
        .into_owned()
}

fn existing_render_path_for(
    source_path: &str,
    source_in_ns: u64,
    source_out_ns: u64,
    media_duration_ns: Option<u64>,
    fps_num: u32,
    fps_den: u32,
    root: &Path,
    ffprobe_path: Option<&str>,
) -> Option<String> {
    let key = animated_svg_render_key(
        source_path,
        source_in_ns,
        source_out_ns,
        media_duration_ns,
        fps_num,
        fps_den,
    );
    let path = render_output_path_for(&key, root);
    if rendered_file_is_ready(&path, ffprobe_path) {
        Some(path)
    } else {
        None
    }
}

fn rendered_file_is_ready(path: &str, ffprobe_path: Option<&str>) -> bool {
    if std::fs::metadata(path)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
    {
        if let Some(ffprobe) = ffprobe_path {
            let output = Command::new(ffprobe)
                .args([
                    "-v",
                    "error",
                    "-select_streams",
                    "v:0",
                    "-show_entries",
                    "stream=codec_name",
                    "-of",
                    "csv=p=0",
                    path,
                ])
                .output();
            return output.ok().filter(|o| o.status.success()).is_some();
        }
        return true;
    }
    false
}

fn find_ffprobe_path() -> Option<String> {
    let ffmpeg = crate::media::export::find_ffmpeg().ok()?;
    Some(ffmpeg.replace("ffmpeg", "ffprobe"))
}

fn is_animation_tag(name: &str) -> bool {
    matches!(name, "animate" | "animateTransform")
}

fn get_attr_value<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn set_attr_value(attrs: &mut Vec<(String, String)>, name: &str, value: &str) {
    if let Some((_, existing)) = attrs.iter_mut().find(|(key, _)| key == name) {
        *existing = value.to_string();
    } else {
        attrs.push((name.to_string(), value.to_string()));
    }
}

fn parse_style_map(style: Option<&str>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(style) = style else {
        return map;
    };
    for decl in style.split(';') {
        let mut parts = decl.splitn(2, ':');
        let Some(key) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(value) = parts.next().map(str::trim) else {
            continue;
        };
        map.insert(key.to_string(), value.to_string());
    }
    map
}

fn serialize_style_map(style: &HashMap<String, String>) -> String {
    let mut entries: Vec<_> = style.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
        .into_iter()
        .map(|(key, value)| format!("{key}:{value}"))
        .collect::<Vec<_>>()
        .join(";")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CalcMode {
    Linear,
    Discrete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransformKind {
    Translate,
    Scale,
    Rotate,
}

#[derive(Clone, Debug)]
struct TimingSpec {
    begin_ns: u64,
    duration_ns: u64,
    total_duration_ns: Option<u64>,
    fill_freeze: bool,
}

impl TimingSpec {
    fn total_end_ns(&self) -> Option<u64> {
        self.total_duration_ns
            .map(|duration| self.begin_ns.saturating_add(duration))
    }

    fn progress_at_ns(&self, time_ns: u64) -> Option<f64> {
        if self.duration_ns == 0 || time_ns < self.begin_ns {
            return None;
        }
        let elapsed = time_ns - self.begin_ns;
        if let Some(total) = self.total_duration_ns {
            if elapsed >= total {
                return self.fill_freeze.then_some(1.0);
            }
        }
        let cycle = elapsed % self.duration_ns;
        Some(cycle as f64 / self.duration_ns as f64)
    }
}

#[derive(Clone, Debug)]
struct AttributeAnimation {
    attribute_name: String,
    timing: TimingSpec,
    calc_mode: CalcMode,
    values: Option<Vec<String>>,
    from: Option<String>,
    to: Option<String>,
}

#[derive(Clone, Debug)]
struct TransformAnimation {
    transform_kind: TransformKind,
    timing: TimingSpec,
    calc_mode: CalcMode,
    values: Option<Vec<String>>,
    from: Option<String>,
    to: Option<String>,
}

#[derive(Clone, Debug)]
enum ParsedAnimation {
    Attribute(AttributeAnimation),
    Transform(TransformAnimation),
}

impl ParsedAnimation {
    fn total_end_ns(&self) -> Option<u64> {
        match self {
            ParsedAnimation::Attribute(anim) => anim.timing.total_end_ns(),
            ParsedAnimation::Transform(anim) => anim.timing.total_end_ns(),
        }
    }

    fn value_at_ns(&self, time_ns: u64) -> Option<String> {
        match self {
            ParsedAnimation::Attribute(anim) => {
                let progress = anim.timing.progress_at_ns(time_ns)?;
                interpolate_animation_value(
                    &anim.attribute_name,
                    anim.values.as_deref(),
                    anim.from.as_deref(),
                    anim.to.as_deref(),
                    progress,
                    anim.calc_mode,
                )
            }
            ParsedAnimation::Transform(anim) => {
                let progress = anim.timing.progress_at_ns(time_ns)?;
                interpolate_transform_value(
                    anim.transform_kind,
                    anim.values.as_deref(),
                    anim.from.as_deref(),
                    anim.to.as_deref(),
                    progress,
                    anim.calc_mode,
                )
            }
        }
    }
}

fn parse_animation_node(node: usvg::roxmltree::Node<'_, '_>) -> Option<ParsedAnimation> {
    if !node.is_element() {
        return None;
    }
    let name = node.tag_name().name();
    let timing = parse_timing_spec(node)?;
    let calc_mode = match node.attribute("calcMode") {
        Some("discrete") => CalcMode::Discrete,
        _ => CalcMode::Linear,
    };
    let values = node
        .attribute("values")
        .map(|values| {
            values
                .split(';')
                .map(|v| v.trim().to_string())
                .collect::<Vec<_>>()
        })
        .filter(|values| values.len() >= 2);
    let from = node.attribute("from").map(|s| s.trim().to_string());
    let to = node.attribute("to").map(|s| s.trim().to_string());
    match name {
        "animate" => {
            let attribute_name = node.attribute("attributeName")?.trim();
            if !is_supported_attribute_animation(attribute_name) {
                return None;
            }
            Some(ParsedAnimation::Attribute(AttributeAnimation {
                attribute_name: attribute_name.to_string(),
                timing,
                calc_mode,
                values,
                from,
                to,
            }))
        }
        "animateTransform" => {
            let kind = match node.attribute("type")?.trim() {
                "translate" => TransformKind::Translate,
                "scale" => TransformKind::Scale,
                "rotate" => TransformKind::Rotate,
                _ => return None,
            };
            Some(ParsedAnimation::Transform(TransformAnimation {
                transform_kind: kind,
                timing,
                calc_mode,
                values,
                from,
                to,
            }))
        }
        _ => None,
    }
}

fn parse_timing_spec(node: usvg::roxmltree::Node<'_, '_>) -> Option<TimingSpec> {
    let begin_ns = node
        .attribute("begin")
        .and_then(parse_svg_time_to_ns)
        .unwrap_or(0);
    let duration_ns = node
        .attribute("dur")
        .and_then(parse_svg_time_to_ns)
        .filter(|duration| *duration > 0)?;
    let repeat_count = match node.attribute("repeatCount").map(str::trim) {
        Some("indefinite") => None,
        Some(value) => value.parse::<f64>().ok(),
        None => Some(1.0),
    };
    let repeat_dur_ns = node.attribute("repeatDur").and_then(parse_svg_time_to_ns);
    let fill_freeze = matches!(node.attribute("fill"), Some("freeze"));
    let total_duration_ns = match repeat_count {
        Some(count) if count.is_finite() && count > 0.0 => {
            let repeated = (duration_ns as f64 * count) as u64;
            Some(repeat_dur_ns.map_or(repeated, |repeat_dur| repeated.min(repeat_dur)))
        }
        _ => Some(repeat_dur_ns.unwrap_or(DEFAULT_ANIMATED_SVG_DURATION_NS)),
    };
    Some(TimingSpec {
        begin_ns,
        duration_ns,
        total_duration_ns,
        fill_freeze,
    })
}

fn parse_svg_time_to_ns(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(ms) = value.strip_suffix("ms") {
        return ms
            .trim()
            .parse::<f64>()
            .ok()
            .map(|v| (v * 1_000_000.0) as u64);
    }
    if let Some(sec) = value.strip_suffix('s') {
        return sec
            .trim()
            .parse::<f64>()
            .ok()
            .map(|v| (v * 1_000_000_000.0) as u64);
    }
    if value.contains(':') {
        let parts: Vec<_> = value.split(':').collect();
        let mut total = 0.0f64;
        for part in parts {
            total = total * 60.0 + part.trim().parse::<f64>().ok()?;
        }
        return Some((total * 1_000_000_000.0) as u64);
    }
    value
        .parse::<f64>()
        .ok()
        .map(|v| (v * 1_000_000_000.0) as u64)
}

fn is_supported_attribute_animation(attribute_name: &str) -> bool {
    matches!(
        attribute_name,
        "opacity"
            | "x"
            | "y"
            | "width"
            | "height"
            | "cx"
            | "cy"
            | "r"
            | "rx"
            | "ry"
            | "x1"
            | "y1"
            | "x2"
            | "y2"
            | "fill"
            | "stroke"
    )
}

fn interpolate_animation_value(
    attribute_name: &str,
    values: Option<&[String]>,
    from: Option<&str>,
    to: Option<&str>,
    progress: f64,
    calc_mode: CalcMode,
) -> Option<String> {
    let is_color = matches!(attribute_name, "fill" | "stroke");
    if let Some(values) = values {
        if is_color {
            return interpolate_value_list(values, progress, calc_mode, interpolate_color_value);
        }
        return interpolate_value_list(values, progress, calc_mode, interpolate_numeric_value);
    }
    let from = from?;
    let to = to?;
    if is_color {
        interpolate_color_value(from, to, progress)
    } else {
        interpolate_numeric_value(from, to, progress)
    }
}

fn interpolate_transform_value(
    kind: TransformKind,
    values: Option<&[String]>,
    from: Option<&str>,
    to: Option<&str>,
    progress: f64,
    calc_mode: CalcMode,
) -> Option<String> {
    let components = if let Some(values) = values {
        interpolate_value_list(values, progress, calc_mode, |start, end, local| {
            interpolate_number_list(start, end, local)
        })?
    } else {
        interpolate_number_list(from?, to?, progress)?
    };
    Some(match kind {
        TransformKind::Translate => {
            if components.len() > 1 {
                format!(
                    "translate({} {})",
                    format_number(components[0]),
                    format_number(components[1])
                )
            } else {
                format!("translate({})", format_number(components[0]))
            }
        }
        TransformKind::Scale => {
            if components.len() > 1 {
                format!(
                    "scale({} {})",
                    format_number(components[0]),
                    format_number(components[1])
                )
            } else {
                format!("scale({})", format_number(components[0]))
            }
        }
        TransformKind::Rotate => match components.len() {
            0 => return None,
            1 => format!("rotate({})", format_number(components[0])),
            2 => format!(
                "rotate({} {} 0)",
                format_number(components[0]),
                format_number(components[1])
            ),
            _ => format!(
                "rotate({} {} {})",
                format_number(components[0]),
                format_number(components[1]),
                format_number(components[2])
            ),
        },
    })
}

fn interpolate_value_list<T, F>(
    values: &[String],
    progress: f64,
    calc_mode: CalcMode,
    interpolate_pair: F,
) -> Option<T>
where
    F: Fn(&str, &str, f64) -> Option<T>,
{
    if values.len() < 2 {
        return None;
    }
    let scaled = progress.clamp(0.0, 1.0) * (values.len() - 1) as f64;
    let idx = scaled.floor() as usize;
    let next_idx = idx.min(values.len() - 2) + 1;
    let local = if calc_mode == CalcMode::Discrete {
        0.0
    } else {
        scaled - idx as f64
    };
    if calc_mode == CalcMode::Discrete && idx >= values.len() - 1 {
        return interpolate_pair(&values[values.len() - 2], &values[values.len() - 1], 1.0);
    }
    interpolate_pair(&values[idx.min(values.len() - 2)], &values[next_idx], local)
}

fn interpolate_numeric_value(start: &str, end: &str, progress: f64) -> Option<String> {
    let (start_value, start_suffix) = parse_numeric_with_suffix(start)?;
    let (end_value, end_suffix) = parse_numeric_with_suffix(end)?;
    if start_suffix != end_suffix {
        return None;
    }
    let value = start_value + (end_value - start_value) * progress.clamp(0.0, 1.0);
    Some(format!("{}{}", format_number(value), start_suffix))
}

fn interpolate_number_list(start: &str, end: &str, progress: f64) -> Option<Vec<f64>> {
    let start = parse_number_list(start)?;
    let end = parse_number_list(end)?;
    if start.len() != end.len() || start.is_empty() {
        return None;
    }
    Some(
        start
            .iter()
            .zip(end.iter())
            .map(|(s, e)| s + (e - s) * progress.clamp(0.0, 1.0))
            .collect(),
    )
}

fn interpolate_color_value(start: &str, end: &str, progress: f64) -> Option<String> {
    let start = parse_color(start)?;
    let end = parse_color(end)?;
    let lerp = |a: u8, b: u8| -> u8 {
        (a as f64 + (b as f64 - a as f64) * progress.clamp(0.0, 1.0)).round() as u8
    };
    Some(format!(
        "#{:02x}{:02x}{:02x}",
        lerp(start[0], end[0]),
        lerp(start[1], end[1]),
        lerp(start[2], end[2])
    ))
}

fn parse_numeric_with_suffix(value: &str) -> Option<(f64, String)> {
    let value = value.trim();
    let split = value
        .find(|ch: char| !(ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.' | 'e' | 'E')))
        .unwrap_or(value.len());
    let (number, suffix) = value.split_at(split);
    Some((
        number.trim().parse::<f64>().ok()?,
        suffix.trim().to_string(),
    ))
}

fn parse_number_list(value: &str) -> Option<Vec<f64>> {
    let normalized = value.replace(',', " ");
    let numbers = normalized
        .split_whitespace()
        .map(|part| part.parse::<f64>().ok())
        .collect::<Option<Vec<_>>>()?;
    (!numbers.is_empty()).then_some(numbers)
}

fn parse_color(value: &str) -> Option<[u8; 3]> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        return match hex.len() {
            3 => Some([
                u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?,
                u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?,
                u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?,
            ]),
            6 => Some([
                u8::from_str_radix(&hex[0..2], 16).ok()?,
                u8::from_str_radix(&hex[2..4], 16).ok()?,
                u8::from_str_radix(&hex[4..6], 16).ok()?,
            ]),
            _ => None,
        };
    }
    if let Some(rgb) = value.strip_prefix("rgb(").and_then(|v| v.strip_suffix(')')) {
        let parts = rgb
            .split(',')
            .map(|part| part.trim().parse::<u8>().ok())
            .collect::<Option<Vec<_>>>()?;
        if parts.len() == 3 {
            return Some([parts[0], parts[1], parts[2]]);
        }
    }
    None
}

fn format_number(value: f64) -> String {
    let rounded = if value.abs() < 0.000_001 { 0.0 } else { value };
    let mut text = format!("{rounded:.4}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPACITY_SVG: &str = r##"
<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">
  <rect width="16" height="16" fill="#ff0000">
    <animate attributeName="opacity" from="0" to="1" dur="1s" fill="freeze"/>
  </rect>
</svg>
"##;

    const TRANSLATE_SVG: &str = r##"
<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">
  <rect width="4" height="4" y="6" fill="#00ff00">
    <animateTransform attributeName="transform" type="translate" from="0 0" to="8 0" dur="1s" fill="freeze"/>
  </rect>
</svg>
"##;

    fn alpha_sum(frame: &RenderedSvgFrame) -> u64 {
        frame.rgba.chunks_exact(4).map(|px| px[3] as u64).sum()
    }

    #[test]
    fn analyze_svg_detects_supported_animation_and_duration() {
        let analysis = analyze_svg_str(OPACITY_SVG).expect("analysis");
        assert!(analysis.is_animated);
        assert_eq!(analysis.duration_ns, Some(1_000_000_000));
    }

    #[test]
    fn opacity_animation_changes_rendered_alpha() {
        let start = render_svg_frame_from_str(OPACITY_SVG, 0, None).expect("start frame");
        let mid = render_svg_frame_from_str(OPACITY_SVG, 500_000_000, None).expect("mid frame");
        let end = render_svg_frame_from_str(OPACITY_SVG, 1_000_000_000, None).expect("end frame");
        assert!(alpha_sum(&start) < alpha_sum(&mid));
        assert!(alpha_sum(&mid) <= alpha_sum(&end));
    }

    #[test]
    fn translate_animation_moves_colored_pixels() {
        let start = render_svg_frame_from_str(TRANSLATE_SVG, 0, None).expect("start frame");
        let end = render_svg_frame_from_str(TRANSLATE_SVG, 1_000_000_000, None).expect("end frame");
        let start_left = start.rgba[(6 * start.width as usize + 1) * 4 + 1];
        let end_left = end.rgba[(6 * end.width as usize + 1) * 4 + 1];
        let end_right = end.rgba[(6 * end.width as usize + 9) * 4 + 1];
        assert!(start_left > end_left);
        assert!(end_right > 0);
    }
}
