use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::{config::Config, models::Highlight, progress::ProgressEvent};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How a landscape (or any) frame is fitted into the output aspect ratio.
/// Face tracking is not implemented; low-confidence subject crops are not guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReframeMode {
    Center,
    BlurredBackground,
}

impl ReframeMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Center => "Center",
            Self::BlurredBackground => "Blur fill",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Center => "center",
            Self::BlurredBackground => "blur",
        }
    }
}

impl std::str::FromStr for ReframeMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "center" | "crop" => Ok(Self::Center),
            "blur" | "blurred" | "blur-fill" => Ok(Self::BlurredBackground),
            _ => Err("reframe must be center or blur".into()),
        }
    }
}

impl std::fmt::Display for ReframeMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

pub struct RenderRequest {
    pub music_path: Option<PathBuf>,
    pub aspect_ratio: String,
    pub format: u32,
    pub reframe: ReframeMode,
    pub captions_ass: Option<String>,
    pub captions_srt: Option<String>,
    /// File stem without an extension. Unsafe values fall back to `short_NN`.
    pub file_stem: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaProbe {
    pub duration: f64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub has_video: bool,
    pub has_audio: bool,
}

pub fn render_short(
    source: &Path,
    highlight: &Highlight,
    output_dir: &Path,
    index: usize,
    request: &RenderRequest,
    emit: &mut dyn FnMut(ProgressEvent),
) -> Result<PathBuf> {
    fs::create_dir_all(output_dir)
        .with_context(|| format!("Could not create {}", output_dir.display()))?;
    let stem = safe_file_stem(index, request.file_stem.as_deref());
    let output = output_dir.join(format!("{stem}.mp4"));
    let temporary = output.with_extension("partial.mp4");
    let (ratio, width, height) = dimensions(&request.aspect_ratio, request.format)?;
    let subtitle_path = request
        .captions_ass
        .as_ref()
        .map(|_| output.with_file_name(format!("{stem}.ass")));
    if let (Some(path), Some(ass)) = (&subtitle_path, request.captions_ass.as_deref()) {
        fs::write(path, ass).with_context(|| format!("Could not write {}", path.display()))?;
    }
    if let Some(srt) = request
        .captions_srt
        .as_deref()
        .filter(|srt| !srt.is_empty())
    {
        let path = output.with_file_name(format!("{stem}.srt"));
        fs::write(&path, srt).with_context(|| format!("Could not write {}", path.display()))?;
    }
    let (complex, filter) = compose_filter(
        request.reframe,
        ratio,
        width,
        height,
        subtitle_path.as_deref(),
    );
    emit(ProgressEvent::Log(format!(
        "Rendering short {} with {} framing: {} ({:.1}s)…",
        index + 1,
        request.reframe.label(),
        highlight.title,
        highlight.duration()
    )));
    let mut command = hidden_command("ffmpeg");
    command
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .arg("-ss")
        .arg(format!("{:.3}", highlight.start_time.max(0.0)))
        .arg("-i")
        .arg(source);
    if let Some(music) = &request.music_path {
        command.args(["-stream_loop", "-1", "-i"]).arg(music);
    }
    command
        .arg("-t")
        .arg(format!("{:.3}", highlight.duration().max(1.0)));
    if request.music_path.is_some() {
        let video = if complex {
            filter
        } else {
            format!("[0:v:0]{filter}[vout]")
        };
        let duration = highlight.duration().max(1.0);
        let fade = 0.75_f64.min(duration / 2.0);
        let music = format!(
            "[1:a:0]volume=0.14,afade=t=in:d=0.15,afade=t=out:st={}:d={fade}",
            duration - fade
        );
        let graph = if probe_media(source)?.has_audio {
            format!(
                "{video};{music}[bed];[0:a:0][bed]amix=inputs=2:duration=longest:normalize=0,alimiter=limit=0.95:level=false[aout]"
            )
        } else {
            format!("{video};{music}[aout]")
        };
        command.args([
            "-filter_complex",
            &graph,
            "-map",
            "[vout]",
            "-map",
            "[aout]",
        ]);
    } else if complex {
        command
            .args(["-filter_complex", &filter])
            .args(["-map", "[vout]", "-map", "0:a:0?"]);
    } else {
        command.args(["-vf", &filter]);
    }
    command.args([
        "-sn",
        "-c:v",
        "libx264",
        "-preset",
        "fast",
        "-crf",
        "18",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        "-movflags",
        "+faststart",
    ]);
    command.arg(&temporary);
    let rendered = command
        .output()
        .context("Could not start FFmpeg. Install FFmpeg and ensure it is on PATH");
    match rendered {
        Ok(process) if process.status.success() => {
            if let Err(error) = fs::rename(&temporary, &output) {
                let _ = fs::remove_file(&temporary);
                return Err(error).with_context(|| format!("Could not save {}", output.display()));
            }
            validate_export(&output, highlight.duration(), width, height)?;
            emit(ProgressEvent::Log(format!("Saved {}", output.display())));
            Ok(output)
        }
        Ok(process) => {
            let _ = fs::remove_file(&temporary);
            bail!(
                "FFmpeg failed while rendering short {}: {}",
                index + 1,
                command_detail(&process.stderr, &process.stdout)
            )
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}

pub fn video_filter(mode: ReframeMode, ratio: f64, width: u32, height: u32) -> String {
    match mode {
        ReframeMode::Center => format!(
            "crop='min(iw,ih*{ratio:.6})':'min(ih,iw/{ratio:.6})':'(iw-ow)/2':'(ih-oh)/2',scale={width}:{height}:force_original_aspect_ratio=decrease:force_divisible_by=2,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2"
        ),
        ReframeMode::BlurredBackground => format!(
            "[0:v]scale={width}:{height}:force_original_aspect_ratio=increase,crop={width}:{height},boxblur=24:2[bg];[0:v]scale={width}:{height}:force_original_aspect_ratio=decrease[fg];[bg][fg]overlay=(W-w)/2:(H-h)/2,format=yuv420p[v]"
        ),
    }
}

fn compose_filter(
    mode: ReframeMode,
    ratio: f64,
    width: u32,
    height: u32,
    subtitles: Option<&Path>,
) -> (bool, String) {
    let filter = video_filter(mode, ratio, width, height);
    let Some(subtitles) = subtitles else {
        return match mode {
            ReframeMode::Center => (false, filter),
            ReframeMode::BlurredBackground => (true, filter.replace("[v]", "[vout]")),
        };
    };
    let subtitles = subtitle_filter(subtitles);
    match mode {
        ReframeMode::Center => (false, format!("{filter},{subtitles}")),
        ReframeMode::BlurredBackground => (true, format!("{filter};[v]{subtitles}[vout]")),
    }
}

pub fn subtitle_filter(path: &Path) -> String {
    format!("subtitles='{}':charenc=UTF-8", escape_filter_path(path))
}

fn escape_filter_path(path: &Path) -> String {
    let mut escaped = String::new();
    for character in path.to_string_lossy().replace('\\', "/").chars() {
        if matches!(character, '\\' | '\'' | ':' | ',' | ';' | '[' | ']') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

pub fn probe_media(path: &Path) -> Result<MediaProbe> {
    let output = hidden_command("ffprobe")
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .context("Could not start ffprobe. Install FFmpeg and ensure it is on PATH")?;
    if !output.status.success() {
        bail!(
            "ffprobe failed: {}",
            command_detail(&output.stderr, &output.stdout)
        );
    }
    let parsed: ProbeFile =
        serde_json::from_slice(&output.stdout).context("ffprobe returned unexpected JSON")?;
    let duration = parsed
        .format
        .and_then(|format| format.duration)
        .and_then(|duration| duration.parse::<f64>().ok())
        .filter(|duration| duration.is_finite() && *duration > 0.0)
        .context("Exported video duration was missing")?;
    let video = parsed
        .streams
        .iter()
        .find(|stream| stream.codec_type == "video");
    let has_audio = parsed
        .streams
        .iter()
        .any(|stream| stream.codec_type == "audio");
    Ok(MediaProbe {
        duration,
        width: video.and_then(|stream| stream.width),
        height: video.and_then(|stream| stream.height),
        has_video: video.is_some(),
        has_audio,
    })
}

fn validate_export(path: &Path, expected_duration: f64, width: u32, height: u32) -> Result<()> {
    let probe = match probe_media(path) {
        Ok(probe) => probe,
        Err(error) => {
            let _ = fs::remove_file(path);
            return Err(error);
        }
    };
    let problem = if !probe.has_video {
        Some("Export has no video stream".to_owned())
    } else if probe.width != Some(width) || probe.height != Some(height) {
        Some(format!(
            "Export is {}x{}, expected {width}x{height}",
            probe.width.unwrap_or(0),
            probe.height.unwrap_or(0)
        ))
    } else {
        let delta = (probe.duration - expected_duration).abs();
        // Input seeking lands on a nearby keyframe, so the file can differ slightly from the request.
        let tolerance = (expected_duration * 0.08).max(1.5);
        if delta > tolerance {
            Some(format!(
                "Export duration is {:.2}s, expected about {:.2}s",
                probe.duration, expected_duration
            ))
        } else {
            None
        }
    };
    if let Some(problem) = problem {
        let _ = fs::remove_file(path);
        bail!("{problem}");
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ProbeFile {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    #[serde(default)]
    codec_type: String,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
}

fn safe_file_stem(index: usize, requested: Option<&str>) -> String {
    let fallback = format!("short_{:02}", index + 1);
    let Some(requested) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
        return fallback;
    };
    let safe = requested.len() <= 64
        && !requested.starts_with('.')
        && requested.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        });
    if safe {
        requested.to_string()
    } else {
        fallback
    }
}

fn hidden_command(program: &str) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

fn command_detail(stderr: &[u8], stdout: &[u8]) -> String {
    let text = [stderr, stdout]
        .into_iter()
        .map(|bytes| String::from_utf8_lossy(bytes).trim().to_owned())
        .find(|text| !text.is_empty())
        .unwrap_or_else(|| "it exited without printing an error".to_owned());
    let count = text.chars().count();
    if count <= 1200 {
        text
    } else {
        text.chars().skip(count - 1200).collect()
    }
}

pub fn download_https(
    url: &str,
    destination: &Path,
    emit: &mut dyn FnMut(ProgressEvent),
) -> Result<PathBuf> {
    let url = url.parse::<reqwest::Url>().context("Invalid output URL")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("Generated media URL must use HTTPS without credentials");
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    emit(ProgressEvent::Log("Downloading MuAPI output…".into()));
    let temporary = destination.with_extension("partial.mp4");
    let result = (|| {
        let mut response = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .context("Could not configure the generated media downloader")?
            .get(url)
            .send()
            .context("Could not download generated media")?
            .error_for_status()
            .context("Generated media download returned an HTTP error")?;
        let mut file = fs::File::create(&temporary)?;
        std::io::copy(&mut response, &mut file).context("Could not save generated media")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, destination)
            .with_context(|| format!("Could not save {}", destination.display()))?;
        Ok(destination.to_path_buf())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn dimensions(aspect_ratio: &str, format: u32) -> Result<(f64, u32, u32)> {
    let parts = aspect_ratio.split(':').collect::<Vec<_>>();
    if parts.len() != 2 {
        bail!("Aspect ratio must be WIDTH:HEIGHT, for example 9:16");
    }
    let width_ratio: f64 = parts[0].parse().context("Invalid aspect ratio width")?;
    let height_ratio: f64 = parts[1].parse().context("Invalid aspect ratio height")?;
    if !width_ratio.is_finite()
        || !height_ratio.is_finite()
        || width_ratio <= 0.0
        || height_ratio <= 0.0
    {
        bail!("Aspect ratio values must be positive numbers");
    }
    let ratio = width_ratio / height_ratio;
    let short_edge = if !(360..=2160).contains(&format) {
        bail!("Format must be between 360 and 2160");
    } else {
        format
    };
    let (width, height) = if (ratio - 9.0 / 16.0).abs() < 0.02 {
        (short_edge, (short_edge as f64 * 16.0 / 9.0).round() as u32)
    } else if (ratio - 1.0).abs() < 0.02 {
        (short_edge, short_edge)
    } else if ratio > 1.0 {
        (short_edge, (short_edge as f64 / ratio).round() as u32)
    } else {
        ((short_edge as f64 * ratio).round() as u32, short_edge)
    };
    let even = |value: u32| value - value % 2;
    Ok((ratio, even(width), even(height)))
}

pub fn validate_output_dir(config: &Config) -> Result<PathBuf> {
    if config.output_dir.as_os_str().is_empty() {
        bail!("LOCAL_OUTPUT_DIR cannot be empty");
    }
    Ok(config.output_dir.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_portrait_square_and_landscape_dimensions() {
        assert_eq!(dimensions("9:16", 1080).unwrap(), (9.0 / 16.0, 1080, 1920));
        assert_eq!(dimensions("1:1", 1080).unwrap(), (1.0, 1080, 1080));
        assert_eq!(dimensions("16:9", 1080).unwrap(), (16.0 / 9.0, 1080, 608));
    }

    #[test]
    fn rejects_invalid_output_settings() {
        assert!(dimensions("9/16", 1080).is_err());
        assert!(dimensions("0:16", 1080).is_err());
        assert!(dimensions("9:16", 0).is_err());
    }

    #[test]
    fn center_and_blur_filters_are_distinct_and_paths_are_escaped() {
        let center = video_filter(ReframeMode::Center, 9.0 / 16.0, 1080, 1920);
        let blur = video_filter(ReframeMode::BlurredBackground, 9.0 / 16.0, 1080, 1920);
        assert!(center.contains("crop="));
        assert!(!center.contains("overlay"));
        assert!(blur.contains("boxblur"));
        assert!(blur.contains("overlay="));
        let (complex, with_subs) = super::compose_filter(
            ReframeMode::Center,
            9.0 / 16.0,
            1080,
            1920,
            Some(Path::new(r"C:\Videos\a b.ass")),
        );
        assert!(!complex);
        assert!(with_subs.contains("subtitles='C\\:/Videos/a b.ass':charenc=UTF-8"));
        let (complex, blurred) =
            super::compose_filter(ReframeMode::BlurredBackground, 9.0 / 16.0, 1080, 1920, None);
        assert!(complex);
        assert!(blurred.ends_with("[vout]"));
    }

    #[test]
    fn renders_a_blurred_vertical_short_when_ffmpeg_is_installed() {
        let ffmpeg = super::hidden_command("ffmpeg").arg("-version").output();
        if ffmpeg.is_err() || !ffmpeg.unwrap().status.success() {
            panic!("ffmpeg is not available; vertical export was not validated");
        }
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("shorts-render-{unique}"));
        fs::create_dir_all(&directory).unwrap();
        let source = directory.join("source.mp4");
        let generated = super::hidden_command("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=0x2244aa:s=640x360:d=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-shortest",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
            ])
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            generated.status.success(),
            "could not create the source: {}",
            String::from_utf8_lossy(&generated.stderr)
        );
        let highlight = Highlight {
            title: "Synthetic".into(),
            start_time: 0.0,
            end_time: 2.0,
            score: 1,
            hook_sentence: String::new(),
            virality_reason: String::new(),
            category: String::new(),
        };
        let request = RenderRequest {
            aspect_ratio: "9:16".into(),
            format: 360,
            reframe: ReframeMode::BlurredBackground,
            captions_ass: Some(
                "\u{feff}[Script Info]\nScriptType: v4.00+\nPlayResX: 360\nPlayResY: 640\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Caption,Nirmala UI,28,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2,0,2,20,20,40,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:02.00,Caption,,0,0,0,,தமிழ் हिन्दी\n".into(),
            ),
            captions_srt: Some("1\n00:00:00,000 --> 00:00:02,000\nதமிழ் हिन्दी\n".into()),
            music_path: None,
            file_stem: None,
        };
        let mut logs = Vec::new();
        let rendered = render_short(&source, &highlight, &directory, 0, &request, &mut |event| {
            if let ProgressEvent::Log(message) = event {
                logs.push(message);
            }
        });
        let rendered = match rendered {
            Ok(path) => path,
            Err(error) => {
                let _ = fs::remove_dir_all(&directory);
                panic!("render failed: {error:#}");
            }
        };
        let probe = probe_media(&rendered).unwrap();
        assert_eq!((probe.width, probe.height), (Some(360), Some(640)));
        assert!(probe.has_video && probe.has_audio);
        assert!((probe.duration - 2.0).abs() < 0.75, "{}", probe.duration);
        let srt = fs::read_to_string(directory.join("short_01.srt")).unwrap();
        assert!(srt.contains("தமிழ்"));
        assert!(srt.contains("हिन्दी"));
        let mixed = RenderRequest {
            music_path: Some(source.clone()),
            file_stem: Some("mixed".into()),
            ..request
        };
        let mixed = render_short(&source, &highlight, &directory, 0, &mixed, &mut |_| {}).unwrap();
        let mixed = probe_media(&mixed).unwrap();
        assert!(mixed.has_audio && mixed.has_video);
        assert!((mixed.duration - 2.0).abs() < 0.15);
        let _ = fs::remove_dir_all(directory);
        assert!(logs.iter().any(|message| message.contains("Blur fill")));
    }

    #[test]
    fn unsafe_export_names_fall_back_to_the_numbered_stem() {
        assert_eq!(super::safe_file_stem(0, None), "short_01");
        assert_eq!(super::safe_file_stem(1, Some("short_02_r3")), "short_02_r3");
        assert_eq!(super::safe_file_stem(0, Some("../secret")), "short_01");
        assert_eq!(super::safe_file_stem(0, Some("short 01")), "short_01");
    }
}
