use crate::model::project::Project;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const SNAPSHOT_XML_FILENAME: &str = "snapshot.uspxml";
const SNAPSHOT_METADATA_FILENAME: &str = "metadata.json";
const SNAPSHOT_WRITE_PATH: &str = "/tmp/ultimateslice-snapshot.uspxml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupFileEntry {
    pub path: PathBuf,
    pub name: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectSnapshotMetadata {
    pub id: String,
    pub snapshot_name: String,
    pub project_title: String,
    #[serde(default)]
    pub project_file_path: Option<String>,
    pub created_at_unix_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSnapshotEntry {
    pub metadata: ProjectSnapshotMetadata,
    pub snapshot_path: PathBuf,
    pub metadata_path: PathBuf,
    pub size_bytes: u64,
}

fn app_data_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .map(|base| base.join("ultimateslice"))
}

pub fn backup_dir() -> Option<PathBuf> {
    app_data_dir().map(|base| base.join("backups"))
}

pub fn snapshot_dir() -> Option<PathBuf> {
    app_data_dir().map(|base| base.join("snapshots"))
}

fn sanitize_backup_filename(title: &str) -> String {
    let s: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "Untitled".to_string()
    } else {
        s
    }
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn format_timestamp_for_filename(unix_secs: u64) -> String {
    let days = unix_secs / 86_400;
    let time_of_day = unix_secs % 86_400;
    let hours = time_of_day / 3_600;
    let minutes = (time_of_day % 3_600) / 60;
    let seconds = time_of_day % 60;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}{month:02}{day:02}_{hours:02}{minutes:02}{seconds:02}")
}

pub fn format_snapshot_timestamp(unix_secs: u64) -> String {
    let days = unix_secs / 86_400;
    let time_of_day = unix_secs % 86_400;
    let hours = time_of_day / 3_600;
    let minutes = (time_of_day % 3_600) / 60;
    let seconds = time_of_day % 60;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}:{seconds:02}")
}

pub fn days_to_ymd(days_since_epoch: u64) -> (u64, u64, u64) {
    let z = days_since_epoch as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u64, m, d)
}

fn prune_old_backups_in_dir(dir: &Path, title_prefix: &str, max_versions: usize) {
    let prefix_underscore = format!("{title_prefix}_");
    let mut backups: Vec<_> = std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy();
            s.starts_with(&prefix_underscore) && s.ends_with(".uspxml")
        })
        .collect();
    if backups.len() <= max_versions {
        return;
    }
    backups.sort_by(|a, b| {
        let ma = a
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        let mb = b
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        mb.cmp(&ma)
    });
    for old in backups.iter().skip(max_versions) {
        let _ = std::fs::remove_file(old.path());
    }
}

pub fn create_versioned_backup(
    xml: &str,
    project_title: &str,
    max_versions: usize,
) -> Result<PathBuf, String> {
    let Some(dir) = backup_dir() else {
        return Err("Backup directory unavailable".to_string());
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create backup dir: {e}"))?;
    let title_sanitized = sanitize_backup_filename(project_title);
    let timestamp = format_timestamp_for_filename(now_unix_secs());
    let backup_path = dir.join(format!("{title_sanitized}_{timestamp}.uspxml"));
    std::fs::write(&backup_path, xml).map_err(|e| format!("Failed to write backup: {e}"))?;
    prune_old_backups_in_dir(&dir, &title_sanitized, max_versions);
    Ok(backup_path)
}

pub fn list_backup_files() -> Vec<BackupFileEntry> {
    let dir = match backup_dir() {
        Some(d) => d,
        None => return Vec::new(),
    };
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".uspxml"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some(BackupFileEntry {
                path: e.path(),
                name: e.file_name().to_string_lossy().to_string(),
                size_bytes: meta.len(),
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        let ma = std::fs::metadata(&a.path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        let mb = std::fs::metadata(&b.path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        mb.cmp(&ma)
    });
    entries
}

fn snapshot_entry_from_dir(dir: &Path) -> Option<ProjectSnapshotEntry> {
    let metadata_path = dir.join(SNAPSHOT_METADATA_FILENAME);
    let snapshot_path = dir.join(SNAPSHOT_XML_FILENAME);
    let metadata = serde_json::from_str::<ProjectSnapshotMetadata>(
        &std::fs::read_to_string(&metadata_path).ok()?,
    )
    .ok()?;
    let size_bytes = std::fs::metadata(&snapshot_path).ok()?.len();
    Some(ProjectSnapshotEntry {
        metadata,
        snapshot_path,
        metadata_path,
        size_bytes,
    })
}

fn list_project_snapshots_in_dir(dir: &Path) -> Vec<ProjectSnapshotEntry> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|path| path.is_dir())
        .filter_map(|dir| snapshot_entry_from_dir(&dir))
        .collect();
    entries.sort_by(|a, b| {
        b.metadata
            .created_at_unix_secs
            .cmp(&a.metadata.created_at_unix_secs)
            .then_with(|| a.metadata.snapshot_name.cmp(&b.metadata.snapshot_name))
    });
    entries
}

pub fn list_project_snapshots() -> Vec<ProjectSnapshotEntry> {
    let dir = match snapshot_dir() {
        Some(d) => d,
        None => return Vec::new(),
    };
    list_project_snapshots_in_dir(&dir)
}

pub fn snapshot_matches_project(entry: &ProjectSnapshotEntry, project: &Project) -> bool {
    match (
        project.file_path.as_deref(),
        entry.metadata.project_file_path.as_deref(),
    ) {
        (Some(current_path), Some(snapshot_path)) => current_path == snapshot_path,
        (Some(_), None) => false,
        (None, Some(_)) => false,
        (None, None) => entry.metadata.project_title == project.title,
    }
}

pub fn list_project_snapshots_for_project(project: &Project) -> Vec<ProjectSnapshotEntry> {
    list_project_snapshots()
        .into_iter()
        .filter(|entry| snapshot_matches_project(entry, project))
        .collect()
}

fn create_project_snapshot_in_dir(
    root: &Path,
    project: &Project,
    xml: &str,
    snapshot_name: &str,
) -> Result<ProjectSnapshotEntry, String> {
    let snapshot_name = snapshot_name.trim();
    if snapshot_name.is_empty() {
        return Err("Snapshot name cannot be empty".to_string());
    }
    std::fs::create_dir_all(root).map_err(|e| format!("Failed to create snapshot dir: {e}"))?;
    let id = Uuid::new_v4().to_string();
    let snapshot_folder = root.join(&id);
    std::fs::create_dir_all(&snapshot_folder)
        .map_err(|e| format!("Failed to create snapshot folder: {e}"))?;

    let snapshot_path = snapshot_folder.join(SNAPSHOT_XML_FILENAME);
    let metadata_path = snapshot_folder.join(SNAPSHOT_METADATA_FILENAME);
    let metadata = ProjectSnapshotMetadata {
        id: id.clone(),
        snapshot_name: snapshot_name.to_string(),
        project_title: project.title.clone(),
        project_file_path: project.file_path.clone(),
        created_at_unix_secs: now_unix_secs(),
    };

    std::fs::write(&snapshot_path, xml).map_err(|e| format!("Failed to write snapshot: {e}"))?;
    let metadata_json = serde_json::to_string_pretty(&metadata)
        .map_err(|e| format!("Failed to encode snapshot metadata: {e}"))?;
    if let Err(e) = std::fs::write(&metadata_path, metadata_json) {
        let _ = std::fs::remove_file(&snapshot_path);
        let _ = std::fs::remove_dir(&snapshot_folder);
        return Err(format!("Failed to write snapshot metadata: {e}"));
    }

    let size_bytes = std::fs::metadata(&snapshot_path)
        .map(|m| m.len())
        .unwrap_or(xml.len() as u64);
    Ok(ProjectSnapshotEntry {
        metadata,
        snapshot_path,
        metadata_path,
        size_bytes,
    })
}

pub fn create_project_snapshot(
    project: &Project,
    xml: &str,
    snapshot_name: &str,
) -> Result<ProjectSnapshotEntry, String> {
    let Some(root) = snapshot_dir() else {
        return Err("Snapshot directory unavailable".to_string());
    };
    create_project_snapshot_in_dir(&root, project, xml, snapshot_name)
}

pub fn write_snapshot_project_xml(project: &Project) -> Result<String, String> {
    crate::fcpxml::writer::write_fcpxml_for_path(project, Path::new(SNAPSHOT_WRITE_PATH))
        .map_err(|e| format!("Snapshot write error: {e}"))
}

fn get_project_snapshot_in_dir(root: &Path, id: &str) -> Result<ProjectSnapshotEntry, String> {
    snapshot_entry_from_dir(&root.join(id)).ok_or_else(|| format!("Snapshot '{id}' not found"))
}

pub fn get_project_snapshot(id: &str) -> Result<ProjectSnapshotEntry, String> {
    let Some(root) = snapshot_dir() else {
        return Err("Snapshot directory unavailable".to_string());
    };
    get_project_snapshot_in_dir(&root, id)
}

fn delete_project_snapshot_in_dir(root: &Path, id: &str) -> Result<(), String> {
    let snapshot_folder = root.join(id);
    if !snapshot_folder.exists() {
        return Err(format!("Snapshot '{id}' not found"));
    }
    std::fs::remove_dir_all(&snapshot_folder).map_err(|e| format!("Failed to delete snapshot: {e}"))
}

pub fn delete_project_snapshot(id: &str) -> Result<(), String> {
    let Some(root) = snapshot_dir() else {
        return Err("Snapshot directory unavailable".to_string());
    };
    delete_project_snapshot_in_dir(&root, id)
}

pub fn load_fcpxml_project(path: &Path) -> Result<Project, String> {
    let xml = std::fs::read_to_string(path).map_err(|e| format!("Failed to read project: {e}"))?;
    crate::fcpxml::parser::parse_fcpxml_with_path(&xml, Some(path))
        .map_err(|e| format!("Project parse error: {e}"))
}

pub fn load_project_snapshot(id: &str) -> Result<(ProjectSnapshotEntry, Project), String> {
    let entry = get_project_snapshot(id)?;
    let project = load_fcpxml_project(&entry.snapshot_path)?;
    Ok((entry, project))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fcpxml::writer::write_fcpxml_for_path;
    use tempfile::tempdir;

    fn snapshot_entry_from_dir_at(dir: &Path) -> Option<ProjectSnapshotEntry> {
        super::snapshot_entry_from_dir(dir)
    }

    #[test]
    fn days_to_ymd_epoch_is_1970_01_01() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn snapshot_matches_saved_project_by_file_path() {
        let mut project = Project::new("Cut A");
        project.file_path = Some("/tmp/project-a.uspxml".to_string());
        let entry = ProjectSnapshotEntry {
            metadata: ProjectSnapshotMetadata {
                id: "snap-1".to_string(),
                snapshot_name: "Before notes".to_string(),
                project_title: "Different Title".to_string(),
                project_file_path: Some("/tmp/project-a.uspxml".to_string()),
                created_at_unix_secs: 1,
            },
            snapshot_path: PathBuf::from("/tmp/snapshot.uspxml"),
            metadata_path: PathBuf::from("/tmp/metadata.json"),
            size_bytes: 10,
        };
        assert!(snapshot_matches_project(&entry, &project));
    }

    #[test]
    fn snapshot_matches_unsaved_project_by_title() {
        let project = Project::new("Unsaved Cut");
        let entry = ProjectSnapshotEntry {
            metadata: ProjectSnapshotMetadata {
                id: "snap-1".to_string(),
                snapshot_name: "Milestone".to_string(),
                project_title: "Unsaved Cut".to_string(),
                project_file_path: None,
                created_at_unix_secs: 1,
            },
            snapshot_path: PathBuf::from("/tmp/snapshot.uspxml"),
            metadata_path: PathBuf::from("/tmp/metadata.json"),
            size_bytes: 10,
        };
        assert!(snapshot_matches_project(&entry, &project));
    }

    #[test]
    fn snapshot_metadata_round_trips_from_disk() {
        let dir = tempdir().expect("temp dir");
        let snapshot_dir = dir.path().join("snap-1");
        std::fs::create_dir_all(&snapshot_dir).expect("create snapshot dir");
        let metadata_path = snapshot_dir.join(SNAPSHOT_METADATA_FILENAME);
        let snapshot_path = snapshot_dir.join(SNAPSHOT_XML_FILENAME);
        let metadata = ProjectSnapshotMetadata {
            id: "snap-1".to_string(),
            snapshot_name: "Before pass".to_string(),
            project_title: "Project".to_string(),
            project_file_path: Some("/tmp/project.uspxml".to_string()),
            created_at_unix_secs: 123,
        };
        std::fs::write(
            &metadata_path,
            serde_json::to_string_pretty(&metadata).expect("metadata json"),
        )
        .expect("write metadata");
        std::fs::write(&snapshot_path, "<fcpxml />").expect("write snapshot");

        let entry = snapshot_entry_from_dir_at(&snapshot_dir).expect("snapshot entry");
        assert_eq!(entry.metadata, metadata);
        assert_eq!(entry.size_bytes, 10);
    }

    #[test]
    fn create_list_and_delete_snapshot_round_trip() {
        let dir = tempdir().expect("temp dir");
        let root = dir.path();
        let project = Project::new("Snapshot Flow");
        let xml = write_snapshot_project_xml(&project).expect("snapshot xml");

        let entry =
            create_project_snapshot_in_dir(root, &project, &xml, "Before notes").expect("create");
        assert_eq!(entry.metadata.snapshot_name, "Before notes");

        let listed = list_project_snapshots_in_dir(root);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].metadata.id, entry.metadata.id);

        let loaded_entry =
            get_project_snapshot_in_dir(root, &entry.metadata.id).expect("snapshot entry");
        let loaded_project = load_fcpxml_project(&loaded_entry.snapshot_path).expect("load");
        assert_eq!(loaded_project.title, "Snapshot Flow");

        delete_project_snapshot_in_dir(root, &entry.metadata.id).expect("delete");
        assert!(list_project_snapshots_in_dir(root).is_empty());
    }

    #[test]
    fn load_fcpxml_project_reads_written_snapshot() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("snapshot.uspxml");
        let project = Project::new("Snapshot Test");
        let xml = write_fcpxml_for_path(&project, &path).expect("write xml");
        std::fs::write(&path, xml).expect("write project file");

        let loaded = load_fcpxml_project(&path).expect("load snapshot");
        assert_eq!(loaded.title, "Snapshot Test");
    }
}
