use crate::model::clip::Clip;
use crate::model::media_library::{source_path_exists, MediaItem};
use crate::model::project::Project;
use crate::model::track::Track;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectHealthPathKind {
    ProxyLocal,
    ProxySidecars,
    Prerender,
    BackgroundRemoval,
    FrameInterpolation,
    VoiceEnhancement,
    ClipEmbeddings,
    AutoTags,
    ClipSearchModels,
    BackgroundRemovalModel,
    FrameInterpolationModel,
    SttModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectHealthPathCategory {
    GeneratedCache,
    InstalledModel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectHealthPathSummary {
    pub kind: ProjectHealthPathKind,
    pub category: ProjectHealthPathCategory,
    pub label: &'static str,
    pub path: String,
    pub exists: bool,
    pub file_count: usize,
    pub size_bytes: u64,
    pub cleanup_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectHealthSnapshot {
    pub offline_paths: Vec<String>,
    pub offline_project_source_count: usize,
    pub offline_library_item_count: usize,
    pub paths: Vec<ProjectHealthPathSummary>,
    pub generated_cache_bytes: u64,
    pub installed_model_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectoryUsage {
    file_count: usize,
    size_bytes: u64,
}

pub fn build_media_availability_index(
    project: &Project,
    library: &[MediaItem],
) -> HashMap<String, bool> {
    let mut availability = HashMap::new();
    for path in collect_media_source_paths(project, library) {
        availability.insert(path.clone(), source_path_exists(&path));
    }
    availability
}

pub fn missing_source_paths(project: &Project, library: &[MediaItem]) -> Vec<String> {
    let mut missing: Vec<String> = build_media_availability_index(project, library)
        .into_iter()
        .filter_map(|(path, exists)| if exists { None } else { Some(path) })
        .collect();
    missing.sort_unstable();
    missing
}

pub fn collect_snapshot(
    project: &Project,
    library: &[MediaItem],
    prerender_cache_root: Option<&Path>,
) -> ProjectHealthSnapshot {
    let media_source_paths = collect_media_source_paths(project, library);
    let offline_paths = missing_source_paths(project, library);
    let offline_set: HashSet<&str> = offline_paths.iter().map(String::as_str).collect();
    let offline_project_source_count = collect_project_source_paths(project)
        .into_iter()
        .filter(|path| offline_set.contains(path.as_str()))
        .count();
    let offline_library_item_count = library
        .iter()
        .filter(|item| {
            !item.source_path.is_empty() && offline_set.contains(item.source_path.as_str())
        })
        .count();

    let proxy_sidecar_usage =
        crate::media::proxy_cache::sidecar_proxy_usage_for_sources(&media_source_paths);

    let mut paths = vec![
        summarize_path(
            ProjectHealthPathKind::ProxyLocal,
            ProjectHealthPathCategory::GeneratedCache,
            "Managed proxy cache",
            crate::media::proxy_cache::local_proxy_cache_dir(),
            true,
        ),
        summarize_proxy_sidecars(&proxy_sidecar_usage),
        summarize_path(
            ProjectHealthPathKind::BackgroundRemoval,
            ProjectHealthPathCategory::GeneratedCache,
            "Background removal cache",
            crate::media::bg_removal_cache::cache_root_dir(),
            true,
        ),
        summarize_path(
            ProjectHealthPathKind::FrameInterpolation,
            ProjectHealthPathCategory::GeneratedCache,
            "Frame interpolation cache",
            crate::media::frame_interp_cache::cache_root_dir(),
            true,
        ),
        summarize_path(
            ProjectHealthPathKind::VoiceEnhancement,
            ProjectHealthPathCategory::GeneratedCache,
            "Voice enhancement cache",
            crate::media::voice_enhance_cache::cache_root_dir(),
            true,
        ),
        summarize_path(
            ProjectHealthPathKind::ClipEmbeddings,
            ProjectHealthPathCategory::GeneratedCache,
            "Clip embedding cache",
            crate::media::clip_embedding_cache::cache_root_dir(),
            true,
        ),
        summarize_path(
            ProjectHealthPathKind::AutoTags,
            ProjectHealthPathCategory::GeneratedCache,
            "Auto-tag cache",
            crate::media::auto_tag_cache::cache_root_dir(),
            true,
        ),
        summarize_path(
            ProjectHealthPathKind::ClipSearchModels,
            ProjectHealthPathCategory::InstalledModel,
            "Clip search models",
            crate::media::clip_embedding_cache::clip_search_model_install_dir(),
            false,
        ),
        summarize_path(
            ProjectHealthPathKind::BackgroundRemovalModel,
            ProjectHealthPathCategory::InstalledModel,
            "Background removal model",
            crate::media::bg_removal_cache::model_download_dir(),
            false,
        ),
        summarize_path(
            ProjectHealthPathKind::FrameInterpolationModel,
            ProjectHealthPathCategory::InstalledModel,
            "Frame interpolation model",
            crate::media::frame_interp_cache::model_install_dir(),
            false,
        ),
        summarize_path(
            ProjectHealthPathKind::SttModel,
            ProjectHealthPathCategory::InstalledModel,
            "Speech-to-text models",
            crate::media::stt_cache::stt_model_dir(),
            false,
        ),
    ];
    if let Some(root) = prerender_cache_root {
        paths.push(summarize_path(
            ProjectHealthPathKind::Prerender,
            ProjectHealthPathCategory::GeneratedCache,
            "Background prerender cache",
            root.to_path_buf(),
            true,
        ));
    }

    let generated_cache_bytes = paths
        .iter()
        .filter(|path| path.category == ProjectHealthPathCategory::GeneratedCache)
        .map(|path| path.size_bytes)
        .sum();
    let installed_model_bytes = paths
        .iter()
        .filter(|path| path.category == ProjectHealthPathCategory::InstalledModel)
        .map(|path| path.size_bytes)
        .sum();

    ProjectHealthSnapshot {
        offline_paths,
        offline_project_source_count,
        offline_library_item_count,
        paths,
        generated_cache_bytes,
        installed_model_bytes,
    }
}

pub fn purge_path(path: &Path) -> Result<(), String> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return std::fs::create_dir_all(path)
            .map_err(|err| format!("failed to create {}: {err}", path.display()));
    };
    if metadata.is_dir() {
        std::fs::remove_dir_all(path)
            .map_err(|err| format!("failed to remove {}: {err}", path.display()))?;
        std::fs::create_dir_all(path)
            .map_err(|err| format!("failed to recreate {}: {err}", path.display()))?;
        return Ok(());
    }
    std::fs::remove_file(path).map_err(|err| format!("failed to remove {}: {err}", path.display()))
}

pub fn relevant_media_source_paths(project: &Project, library: &[MediaItem]) -> HashSet<String> {
    collect_media_source_paths(project, library)
}

fn summarize_path(
    kind: ProjectHealthPathKind,
    category: ProjectHealthPathCategory,
    label: &'static str,
    path: PathBuf,
    cleanup_supported: bool,
) -> ProjectHealthPathSummary {
    let usage = directory_usage(&path);
    ProjectHealthPathSummary {
        kind,
        category,
        label,
        path: path.to_string_lossy().to_string(),
        exists: path.exists(),
        file_count: usage.file_count,
        size_bytes: usage.size_bytes,
        cleanup_supported,
    }
}

fn summarize_proxy_sidecars(
    usage: &crate::media::proxy_cache::ProxySidecarUsage,
) -> ProjectHealthPathSummary {
    ProjectHealthPathSummary {
        kind: ProjectHealthPathKind::ProxySidecars,
        category: ProjectHealthPathCategory::GeneratedCache,
        label: "Alongside-media proxy cache",
        path: format_proxy_sidecar_paths(&usage.directories),
        exists: !usage.directories.is_empty(),
        file_count: usage.file_count,
        size_bytes: usage.size_bytes,
        cleanup_supported: true,
    }
}

fn format_proxy_sidecar_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "UltimateSlice.cache/ beside project source media".to_string();
    }
    let mut lines: Vec<String> = paths
        .iter()
        .take(3)
        .map(|path| path.to_string_lossy().to_string())
        .collect();
    if paths.len() > 3 {
        lines.push(format!("…and {} more", paths.len() - 3));
    }
    lines.join("\n")
}

fn directory_usage(path: &Path) -> DirectoryUsage {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return DirectoryUsage {
            file_count: 0,
            size_bytes: 0,
        };
    };
    if metadata.is_file() {
        return DirectoryUsage {
            file_count: 1,
            size_bytes: metadata.len(),
        };
    }
    if !metadata.is_dir() {
        return DirectoryUsage {
            file_count: 0,
            size_bytes: 0,
        };
    }
    let mut usage = DirectoryUsage {
        file_count: 0,
        size_bytes: 0,
    };
    let Ok(entries) = std::fs::read_dir(path) else {
        return usage;
    };
    for entry in entries.flatten() {
        let child = directory_usage(&entry.path());
        usage.file_count += child.file_count;
        usage.size_bytes += child.size_bytes;
    }
    usage
}

fn collect_media_source_paths(project: &Project, library: &[MediaItem]) -> HashSet<String> {
    let mut paths = collect_project_source_paths(project);
    paths.extend(
        library
            .iter()
            .filter(|item| !item.source_path.is_empty())
            .map(|item| item.source_path.clone()),
    );
    paths
}

fn collect_project_source_paths(project: &Project) -> HashSet<String> {
    let mut paths = HashSet::new();
    collect_track_source_paths(&project.tracks, &mut paths);
    paths
}

fn collect_track_source_paths(tracks: &[Track], paths: &mut HashSet<String>) {
    for track in tracks {
        for clip in &track.clips {
            collect_clip_source_paths(clip, paths);
        }
    }
}

fn collect_clip_source_paths(clip: &Clip, paths: &mut HashSet<String>) {
    if !clip.source_path.is_empty() {
        paths.insert(clip.source_path.clone());
    }
    if let Some(ref compound_tracks) = clip.compound_tracks {
        collect_track_source_paths(compound_tracks, paths);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::clip::Clip;
    use crate::model::media_library::MediaItem;
    use crate::model::project::Project;
    use crate::model::track::Track;

    #[test]
    fn missing_source_paths_include_nested_compound_sources() {
        let mut project = Project::new("Health");
        project.tracks.clear();
        let mut outer = Track::new_video("V1");
        let mut compound = Clip::new_compound(
            0,
            vec![{
                let mut inner = Track::new_video("Inner");
                inner.clips.push(Clip::new(
                    "nested.mp4",
                    1_000_000_000,
                    0,
                    crate::model::clip::ClipKind::Video,
                ));
                inner
            }],
        );
        compound.label = "Compound".into();
        compound.id = "compound".into();
        outer.clips.push(compound);
        project.tracks.push(outer);

        let library = vec![MediaItem::new("top.mp4", 1_000_000_000)];
        let missing = missing_source_paths(&project, &library);

        assert!(missing.iter().any(|path| path == "nested.mp4"));
        assert!(missing.iter().any(|path| path == "top.mp4"));
    }

    #[test]
    fn snapshot_sums_generated_and_model_bytes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let generated = temp.path().join("generated");
        let models = temp.path().join("models");
        std::fs::create_dir_all(&generated).expect("generated dir");
        std::fs::create_dir_all(&models).expect("models dir");
        std::fs::write(generated.join("a.bin"), vec![0u8; 10]).expect("generated file");
        std::fs::write(models.join("m.onnx"), vec![0u8; 20]).expect("model file");

        let generated_summary = summarize_path(
            ProjectHealthPathKind::AutoTags,
            ProjectHealthPathCategory::GeneratedCache,
            "Generated",
            generated,
            true,
        );
        let model_summary = summarize_path(
            ProjectHealthPathKind::ClipSearchModels,
            ProjectHealthPathCategory::InstalledModel,
            "Model",
            models,
            false,
        );

        assert_eq!(generated_summary.size_bytes, 10);
        assert_eq!(generated_summary.file_count, 1);
        assert_eq!(model_summary.size_bytes, 20);
        assert_eq!(model_summary.file_count, 1);
    }
}
