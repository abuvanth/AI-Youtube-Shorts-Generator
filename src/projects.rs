//! Saved generation projects. API keys are never part of this record.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::models::GenerationResult;

const PROJECT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectFile {
    version: u32,
    id: String,
    saved_at_unix: u64,
    source: String,
    result: GenerationResult,
}

#[derive(Debug, Clone)]
pub struct ProjectSummary {
    pub id: String,
    pub saved_at_unix: u64,
    pub source: String,
    pub ready_clips: usize,
    pub result: GenerationResult,
}

pub fn save_project(output_dir: &Path, source: &str, result: &GenerationResult) -> Result<PathBuf> {
    let directory = output_dir.join("projects");
    fs::create_dir_all(&directory)
        .with_context(|| format!("Could not create {}", directory.display()))?;
    let saved_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let id = format!("{nanos}");
    let record = ProjectFile {
        version: PROJECT_VERSION,
        id: id.clone(),
        saved_at_unix,
        source: source.trim().to_owned(),
        result: result.clone(),
    };
    let json = serde_json::to_string_pretty(&record).context("Could not encode the project")?;
    let destination = directory.join(format!("{id}.json"));
    let temporary = directory.join(format!("{id}.json.partial"));
    let write_result = (|| {
        fs::write(&temporary, format!("{json}\n"))
            .with_context(|| format!("Could not write {}", temporary.display()))?;
        fs::rename(&temporary, &destination)
            .with_context(|| format!("Could not save {}", destination.display()))?;
        Ok(destination.clone())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

pub fn list_projects(output_dir: &Path) -> Result<Vec<ProjectSummary>> {
    let directory = output_dir.join("projects");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(&directory)
        .with_context(|| format!("Could not read {}", directory.display()))?;
    let mut projects = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".json")
            || name.contains("..")
            || !entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            continue;
        }
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => continue,
        };
        let record: ProjectFile = match serde_json::from_str(&text) {
            Ok(record) => record,
            Err(_) => continue,
        };
        if record.version != PROJECT_VERSION {
            continue;
        }
        let ready_clips = record
            .result
            .shorts
            .iter()
            .filter(|short| short.error.is_none() && short.clip_url.is_some())
            .count();
        projects.push(ProjectSummary {
            id: record.id,
            saved_at_unix: record.saved_at_unix,
            source: record.source,
            ready_clips,
            result: record.result,
        });
    }
    projects.sort_by(|left, right| {
        right
            .saved_at_unix
            .cmp(&left.saved_at_unix)
            .then_with(|| right.id.cmp(&left.id))
    });
    projects.truncate(8);
    Ok(projects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Highlight, Mode, RenderedShort, Transcript};

    fn result() -> GenerationResult {
        GenerationResult {
            mode: Mode::Local,
            source_video_url: "C:\\Videos\\source.mp4".into(),
            transcript: Transcript {
                duration: 12.0,
                segments: Vec::new(),
            },
            highlights: Vec::new(),
            shorts: vec![RenderedShort {
                highlight: Highlight {
                    title: "Saved clip".into(),
                    start_time: 1.0,
                    end_time: 4.0,
                    score: 70,
                    hook_sentence: "Saved".into(),
                    virality_reason: "Kept".into(),
                    category: String::new(),
                },
                clip_url: Some("C:\\Videos\\short_01.mp4".into()),
                error: None,
            }],
        }
    }

    #[test]
    fn projects_round_trip_without_secrets_and_skip_invalid_files() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("shorts-projects-{unique}"));
        fs::create_dir_all(&directory).unwrap();
        let saved = save_project(&directory, "C:\\Videos\\录音.mp4", &result()).unwrap();
        let json = fs::read_to_string(&saved).unwrap();
        assert!(!json.to_ascii_lowercase().contains("api_key"));
        assert!(!json.contains("GEMINI"));
        assert!(json.contains("录音.mp4"));
        fs::write(directory.join("projects").join("notes.txt"), "ignore").unwrap();
        fs::write(directory.join("projects").join("broken.json"), "{").unwrap();
        fs::write(
            directory.join("projects").join("future.json"),
            r#"{"version":99,"id":"x","saved_at_unix":1,"source":"nope","result":{}}"#,
        )
        .unwrap();

        let listed = list_projects(&directory).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].source, "C:\\Videos\\录音.mp4");
        assert_eq!(listed[0].ready_clips, 1);
        assert_eq!(listed[0].result.shorts[0].highlight.title, "Saved clip");
        let _ = fs::remove_dir_all(directory);
    }
}
