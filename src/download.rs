use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use anyhow::{Context, Result, bail};
use url::Url;

use crate::{config::Config, models::Mode, progress::ProgressEvent};

const VIDEO_HOSTS: &[&str] = &[
    "youtube.com",
    "www.youtube.com",
    "m.youtube.com",
    "music.youtube.com",
    "youtu.be",
    "www.youtu.be",
];
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "webm"];

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone)]
pub struct SourceVideo {
    pub path: PathBuf,
    pub original: String,
    pub youtube_url: Option<String>,
    pub video_id: Option<String>,
    pub duration: f64,
}

pub fn validate_youtube_url(value: &str) -> Result<String> {
    let message =
        "Only HTTPS URLs from youtube.com or youtu.be are supported as remote video sources.";
    let value = value.trim();
    if value.is_empty() {
        bail!("{message}");
    }

    let parsed = Url::parse(value).with_context(|| message)?;
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    if parsed.scheme() != "https"
        || !VIDEO_HOSTS.contains(&host.as_str())
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port_or_known_default() != Some(443)
    {
        bail!("{message}");
    }
    Ok(value.to_string())
}

pub fn resolve_source(
    input: &str,
    config: &Config,
    _mode: Mode,
    height: u32,
    emit: &mut dyn FnMut(ProgressEvent),
) -> Result<SourceVideo> {
    let original = input.trim();
    if original.is_empty() {
        bail!("Choose a YouTube URL or a local video file");
    }
    if original.starts_with("file://") {
        let url = Url::parse(original).context("Invalid file URL")?;
        let path = url
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("Invalid local file URL"))?;
        return resolve_local(path, original, emit);
    }
    if is_youtube_url(original) {
        let youtube_url = validate_youtube_url(original)?;
        let video_id = youtube_video_id(&youtube_url)?;
        let output = cache_dir(config);
        fs_create(&output)?;
        let extension = match find_cached_video(&output, &video_id, height) {
            Some(path) => {
                emit(ProgressEvent::Log("Using cached source video".into()));
                path
            }
            None => download_youtube(&youtube_url, &video_id, height, &output, emit)?,
        };
        let duration = probe_duration(&extension)?;
        return Ok(SourceVideo {
            path: extension,
            original: original.to_string(),
            youtube_url: Some(youtube_url),
            video_id: Some(video_id),
            duration,
        });
    }

    let path = expand_local_path(PathBuf::from(original));
    if !path.exists() {
        bail!(
            "Remote video URLs must be HTTPS YouTube URLs; local file does not exist: {}",
            path.display()
        );
    }
    resolve_local(path, original, emit)
}

fn resolve_local(
    path: PathBuf,
    original: &str,
    emit: &mut dyn FnMut(ProgressEvent),
) -> Result<SourceVideo> {
    let path = path
        .canonicalize()
        .context("Could not resolve local video")?;
    if !path.is_file() {
        bail!("Source is not a file: {}", path.display());
    }
    emit(ProgressEvent::Log(format!(
        "Using local source: {}",
        path.display()
    )));
    let duration = probe_duration(&path)?;
    Ok(SourceVideo {
        path,
        original: original.to_string(),
        youtube_url: None,
        video_id: None,
        duration,
    })
}

fn expand_local_path(path: PathBuf) -> PathBuf {
    let Some(value) = path.to_str() else {
        return path;
    };
    if value != "~" && !value.starts_with("~/") && !value.starts_with("~\\") {
        return path;
    }
    let Some(home) = ["USERPROFILE", "HOME"]
        .iter()
        .find_map(|name| env::var_os(name).filter(|value| !value.is_empty()))
    else {
        return path;
    };
    PathBuf::from(home).join(value[1..].trim_start_matches(['/', '\\']))
}

fn is_youtube_url(value: &str) -> bool {
    Url::parse(value)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
        .is_some_and(|host| VIDEO_HOSTS.contains(&host.as_str()))
}

fn youtube_video_id(value: &str) -> Result<String> {
    let url = Url::parse(value)?;
    let id = if url.host_str() == Some("youtu.be") {
        url.path_segments()
            .and_then(|mut parts| parts.next())
            .filter(|part| !part.is_empty())
            .map(str::to_string)
    } else {
        url.query_pairs()
            .find(|(key, _)| key == "v")
            .map(|(_, value)| value.into_owned())
            .or_else(|| {
                let mut parts = url.path_segments()?;
                match (parts.next()?, parts.next()?) {
                    ("embed" | "shorts" | "live", id) if !id.is_empty() => Some(id.to_string()),
                    _ => None,
                }
            })
    };
    let id = id.ok_or_else(|| anyhow::anyhow!("Could not find a YouTube video ID"))?;
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("Invalid YouTube video ID");
    }
    Ok(id)
}

fn cache_dir(config: &Config) -> PathBuf {
    config.output_dir.join("cache")
}

fn fs_create(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("Could not create {}", path.display()))
}

fn find_cached_video(output: &Path, video_id: &str, height: u32) -> Option<PathBuf> {
    for stem in [
        format!("source_{video_id}_{height}"),
        format!("source_{video_id}"),
    ] {
        for extension in VIDEO_EXTENSIONS {
            let path = output.join(format!("{stem}.{extension}"));
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn download_youtube(
    url: &str,
    video_id: &str,
    height: u32,
    output: &Path,
    emit: &mut dyn FnMut(ProgressEvent),
) -> Result<PathBuf> {
    emit(ProgressEvent::Log(format!(
        "Downloading source video (max {height}p)…"
    )));
    let template = output.join(format!("source_{video_id}_{height}.%(ext)s"));
    let format = format!(
        "bestvideo[height<={height}][ext=mp4]+bestaudio[ext=m4a]/best[height<={height}][ext=mp4]/best"
    );
    let mut command = Command::new("yt-dlp");
    command.args([
        "--no-playlist",
        "--no-part",
        "--no-mtime",
        "--format",
        &format,
        "--merge-output-format",
        "mp4",
        "--output",
        &template.to_string_lossy(),
        "--print",
        "after_move:filepath",
        url,
    ]);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let output_result = command
        .output()
        .context("Could not start yt-dlp. Install it and ensure it is on PATH")?;
    if !output_result.status.success() {
        let detail = [
            String::from_utf8_lossy(&output_result.stderr),
            String::from_utf8_lossy(&output_result.stdout),
        ]
        .map(|text| text.trim().to_owned())
        .into_iter()
        .find(|text| !text.is_empty())
        .unwrap_or_else(|| "it exited without printing an error".to_owned());
        bail!("yt-dlp failed ({}): {}", output_result.status, detail);
    }
    let stdout = String::from_utf8_lossy(&output_result.stdout);
    let path = stdout.lines().last().unwrap_or_default().trim();
    if path.is_empty() {
        bail!("yt-dlp did not return an output path");
    }
    let path = PathBuf::from(path);
    if !path.is_file() {
        bail!("yt-dlp output file was not found");
    }
    Ok(path)
}

pub fn probe_duration(path: &Path) -> Result<f64> {
    let mut command = Command::new("ffprobe");
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let output = command
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .context("Could not start ffprobe. Install FFmpeg and ensure it is on PATH")?;
    if !output.status.success() {
        bail!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .context("Video duration was invalid")
        .and_then(|duration| {
            if duration.is_finite() && duration > 0.0 {
                Ok(duration)
            } else {
                bail!("Video duration must be positive")
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn youtube_validation_accepts_supported_hosts() {
        assert!(validate_youtube_url("https://youtu.be/abc123").is_ok());
        assert!(validate_youtube_url("https://www.youtube.com/watch?v=abc123").is_ok());
    }

    #[test]
    fn youtube_validation_rejects_unsafe_sources() {
        for value in [
            "http://youtu.be/abc123",
            "https://youtube.com.evil.example/abc123",
            "https://user:pass@youtube.com/watch?v=abc123",
            "https://youtube.com:444/watch?v=abc123",
            "file:///tmp/video.mp4",
        ] {
            assert!(validate_youtube_url(value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn cache_prefers_requested_resolution_then_generic_source() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("shorts-cache-{unique}"));
        std::fs::create_dir_all(&directory).unwrap();
        let generic = directory.join("source_abc.mp4");
        let requested = directory.join("source_abc_1080.mp4");
        std::fs::write(&generic, b"generic").unwrap();
        std::fs::write(&requested, b"requested").unwrap();

        assert_eq!(
            find_cached_video(&directory, "abc", 1080),
            Some(requested.clone())
        );
        assert_eq!(
            find_cached_video(&directory, "abc", 720),
            Some(generic.clone())
        );
        std::fs::remove_file(requested).unwrap();
        assert_eq!(find_cached_video(&directory, "abc", 1080), Some(generic));
        assert!(find_cached_video(&directory, "other", 1080).is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn extracts_video_id_from_all_youtube_forms() {
        assert_eq!(
            youtube_video_id("https://youtu.be/abc-123").unwrap(),
            "abc-123"
        );
        assert_eq!(
            youtube_video_id("https://youtube.com/watch?v=abc_123").unwrap(),
            "abc_123"
        );
        assert_eq!(
            youtube_video_id("https://youtube.com/shorts/abc123").unwrap(),
            "abc123"
        );
        assert!(youtube_video_id("https://youtube.com/watch?list=abc123").is_err());
    }
}
