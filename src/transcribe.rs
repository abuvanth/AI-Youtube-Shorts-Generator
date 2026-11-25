use std::{
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use regex::Regex;
use reqwest::blocking::{Client, multipart};
use serde::Deserialize;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::{
    config::{AsrProvider, Config},
    download::probe_duration,
    models::{Segment, Transcript},
    progress::ProgressEvent,
};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn transcribe(
    source: &Path,
    language: Option<&str>,
    config: &Config,
    emit: &mut dyn FnMut(ProgressEvent),
) -> Result<Transcript> {
    match config.asr_provider {
        AsrProvider::ElevenLabs => transcribe_elevenlabs(source, language, config, emit),
        AsrProvider::Whisper => transcribe_whisper(source, language, config, emit),
    }
}

fn source_stem(source: &Path) -> Result<&str> {
    source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .context("Source has an invalid filename")
}

fn transcript_cache_path(source: &Path, output_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "Could not create output directory: {}",
            output_dir.display()
        )
    })?;
    let raw_stem = source_stem(source)?;
    let exact = output_dir.join(format!("{raw_stem}.srt"));
    if exact.exists() {
        return Ok(exact);
    }
    let normalized = Regex::new(r"_(?:360|480|720|1080|1440|2160)$")
        .unwrap()
        .replace(raw_stem, "");
    Ok(output_dir.join(format!("{normalized}.srt")))
}

fn remove_invalid_cache(path: &Path) {
    let _ = fs::remove_file(path);
}

fn cached_transcript(source: &Path, output_dir: &Path) -> Result<Option<Transcript>> {
    let path = transcript_cache_path(source, output_dir)?;
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("Could not inspect transcript cache: {}", path.display())
            });
        }
    };
    if !metadata.is_file() || metadata.len() == 0 {
        remove_invalid_cache(&path);
        return Ok(None);
    }

    let source_modified = fs::metadata(source)
        .with_context(|| format!("Could not inspect source media: {}", source.display()))?
        .modified()?;
    let cache_modified = match metadata.modified() {
        Ok(modified) => modified,
        Err(_) => {
            remove_invalid_cache(&path);
            return Ok(None);
        }
    };
    if cache_modified < source_modified {
        remove_invalid_cache(&path);
        return Ok(None);
    }

    let transcript = match fs::read_to_string(&path) {
        Ok(content) => parse_srt(&content),
        Err(_) => {
            remove_invalid_cache(&path);
            return Ok(None);
        }
    };
    match transcript {
        Ok(transcript)
            if !transcript.segments.is_empty()
                && transcript.duration.is_finite()
                && transcript.duration > 0.0 =>
        {
            Ok(Some(transcript))
        }
        _ => {
            remove_invalid_cache(&path);
            Ok(None)
        }
    }
}

fn transcribe_elevenlabs(
    source: &Path,
    language: Option<&str>,
    config: &Config,
    emit: &mut dyn FnMut(ProgressEvent),
) -> Result<Transcript> {
    if let Some(transcript) = cached_transcript(source, &config.output_dir)? {
        emit(ProgressEvent::Log("Using cached transcript".into()));
        return Ok(transcript);
    }

    let audio = ensure_audio(source, &config.output_dir)?;
    emit(ProgressEvent::Log(
        "Transcribing with ElevenLabs Scribe…".into(),
    ));
    let bytes = fs::read(&audio).context("Could not read audio for transcription")?;
    let file_name = audio
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("audio.mp3");
    let file = multipart::Part::bytes(bytes)
        .file_name(file_name.to_string())
        .mime_str(audio_mime(&audio))?;
    let mut form = multipart::Form::new()
        .part("file", file)
        .text("model_id", config.elevenlabs_scribe_model.clone())
        .text("tag_audio_events", "false");
    if let Some(language) = language.filter(|value| !value.trim().is_empty()) {
        form = form.text("language_code", language.trim().to_string());
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .context("Could not configure ElevenLabs HTTP client")?;
    let response = client
        .post("https://api.elevenlabs.io/v1/speech-to-text")
        .header("xi-api-key", config.elevenlabs_api_key()?)
        .multipart(form)
        .send()
        .context("ElevenLabs transcription request failed")?
        .error_for_status()
        .context("ElevenLabs transcription returned an error")?
        .json::<ElevenLabsResponse>()
        .context("Could not parse ElevenLabs transcription response")?;

    let transcript = transcript_from_elevenlabs(response);
    write_srt_cache(source, &config.output_dir, &transcript)?;
    Ok(transcript)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ElevenLabsResponse {
    text: Option<String>,
    words: Option<Vec<ElevenLabsWord>>,
    audio_duration_secs: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ElevenLabsWord {
    text: String,
    start: f64,
    end: f64,
    #[serde(rename = "type")]
    kind: Option<String>,
}

fn transcript_from_elevenlabs(response: ElevenLabsResponse) -> Transcript {
    let reported_duration = response
        .audio_duration_secs
        .filter(|duration| duration.is_finite() && *duration > 0.0);
    let segments = group_words(
        response.words.unwrap_or_default(),
        response.text.as_deref().unwrap_or_default(),
        reported_duration.unwrap_or(0.0),
    );
    let duration = reported_duration
        .unwrap_or_else(|| segments.last().map(|segment| segment.end).unwrap_or(0.0));
    Transcript { duration, segments }
}

fn round_hundredths(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end()
        .chars()
        .next_back()
        .is_some_and(|character| matches!(character, '.' | '?' | '!' | '…'))
}

fn push_grouped_segment(
    segments: &mut Vec<Segment>,
    start: f64,
    end: f64,
    words: &mut Vec<String>,
) {
    if words.is_empty() {
        return;
    }
    segments.push(Segment {
        start: round_hundredths(start),
        end: round_hundredths(end),
        text: words.join(" "),
    });
    words.clear();
}

fn group_words(words: Vec<ElevenLabsWord>, full_text: &str, duration: f64) -> Vec<Segment> {
    if words.is_empty() {
        let text = full_text.trim();
        return if !text.is_empty() && duration > 0.0 {
            vec![Segment {
                start: 0.0,
                end: duration,
                text: text.to_string(),
            }]
        } else {
            Vec::new()
        };
    }

    let mut segments = Vec::new();
    let mut current_words = Vec::new();
    let mut segment_start = 0.0;
    let mut segment_end = 0.0;
    for word in words {
        let text = word.text.trim().to_string();
        if text.is_empty() || matches!(word.kind.as_deref(), Some("spacing" | "audio_event")) {
            continue;
        }

        if !current_words.is_empty() && word.start - segment_end > 1.2 {
            push_grouped_segment(
                &mut segments,
                segment_start,
                segment_end,
                &mut current_words,
            );
        }
        if current_words.is_empty() {
            segment_start = word.start;
        }
        current_words.push(text);
        segment_end = word.end;

        let sentence_end =
            ends_sentence(current_words.last().map(String::as_str).unwrap_or_default());
        let too_long = current_words.len() >= 18 || segment_end - segment_start >= 8.0;
        if sentence_end || too_long {
            push_grouped_segment(
                &mut segments,
                segment_start,
                segment_end,
                &mut current_words,
            );
        }
    }
    push_grouped_segment(
        &mut segments,
        segment_start,
        segment_end,
        &mut current_words,
    );
    segments
}

fn audio_mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        _ => "application/octet-stream",
    }
}

fn ensure_audio(source: &Path, output_dir: &Path) -> Result<PathBuf> {
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        extension.as_str(),
        "mp3" | "wav" | "m4a" | "aac" | "flac" | "ogg"
    ) {
        return Ok(source.to_path_buf());
    }

    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "Could not create output directory: {}",
            output_dir.display()
        )
    })?;
    let audio = output_dir.join(format!("{}_asr.mp3", source_stem(source)?));
    let source_modified = fs::metadata(source)?.modified()?;
    if audio.metadata().is_ok_and(|metadata| {
        metadata.is_file()
            && metadata.len() > 0
            && metadata
                .modified()
                .is_ok_and(|modified| modified >= source_modified)
    }) {
        return Ok(audio);
    }

    let mut command = Command::new("ffmpeg");
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let result = command
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(source)
        .args([
            "-vn",
            "-acodec",
            "libmp3lame",
            "-ar",
            "16000",
            "-ac",
            "1",
            "-b:a",
            "64k",
        ])
        .arg(&audio)
        .output();
    if result.is_ok_and(|output| output.status.success()) && audio.is_file() {
        Ok(audio)
    } else {
        remove_invalid_cache(&audio);
        Ok(source.to_path_buf())
    }
}

fn transcribe_whisper(
    source: &Path,
    language: Option<&str>,
    config: &Config,
    emit: &mut dyn FnMut(ProgressEvent),
) -> Result<Transcript> {
    if let Some(transcript) = cached_transcript(source, &config.output_dir)? {
        emit(ProgressEvent::Log("Using cached transcript".into()));
        return Ok(transcript);
    }
    emit(ProgressEvent::Log(
        "Transcribing with local whisper.cpp…".into(),
    ));

    let model = resolve_whisper_model(&config.local_whisper_model, emit)?;
    let stem = source_stem(source)?;
    let output_base = config.output_dir.join(format!("{stem}.whisper"));
    let json_output = appended_suffix(&output_base, ".json");
    let alternate_json_output = config.output_dir.join(format!("{stem}.json"));
    remove_invalid_cache(&json_output);
    remove_invalid_cache(&alternate_json_output);

    let audio = ensure_wav(source, &config.output_dir)?;
    let mut command = Command::new("whisper-cli");
    command.args(["-m"]).arg(model);
    command.args(["-f"]).arg(audio);
    command.args(["-bs", "5", "-mc", "0", "-oj", "-of"]);
    command.arg(output_base);
    add_whisper_device_args(&mut command, &config.local_whisper_device)?;
    let language = language
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("auto");
    command.args(["-l"]).arg(language);
    if config.local_whisper_vad_filter {
        command.arg("--vad");
        add_whisper_vad_args(&mut command, &config.local_whisper_vad_parameters);
    }
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let output = command.output().context(
        "Could not start whisper-cli. Install whisper.cpp or set ASR_PROVIDER=elevenlabs",
    )?;
    if !output.status.success() {
        bail!(
            "whisper-cli failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let output_path = [json_output, alternate_json_output]
        .into_iter()
        .find(|path| path.is_file())
        .context("whisper-cli succeeded but did not create a JSON transcript")?;
    let json = fs::read_to_string(&output_path)
        .with_context(|| format!("Could not read whisper-cli JSON: {}", output_path.display()))?;
    let media_duration = probe_duration(source)
        .context("Could not probe media duration for the whisper.cpp transcript")?;
    let transcript = parse_whisper_json(&json, media_duration)?;
    write_srt_cache(source, &config.output_dir, &transcript)?;
    Ok(transcript)
}

fn appended_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn resolve_whisper_model(configured: &str, emit: &mut dyn FnMut(ProgressEvent)) -> Result<PathBuf> {
    let configured = configured.trim();
    if configured.is_empty() {
        bail!("LOCAL_WHISPER_MODEL is empty; set it to a model name or an existing ggml .bin file");
    }
    let direct = PathBuf::from(configured);
    if direct.is_file() {
        return Ok(direct);
    }
    if direct.components().count() > 1 {
        bail!(
            "Local whisper.cpp model file does not exist: {}",
            direct.display()
        );
    }

    let file_name = direct
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .context("LOCAL_WHISPER_MODEL does not contain a valid model name")?;
    let filename = if file_name.starts_with("ggml-") && file_name.ends_with(".bin") {
        file_name.to_string()
    } else {
        let model = file_name.strip_suffix(".bin").unwrap_or(file_name);
        format!("ggml-{model}.bin")
    };

    let mut candidates = vec![
        env::current_dir()
            .context("Could not determine the current directory for the whisper.cpp model")?
            .join(&filename),
    ];
    if let Some(cache) = env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        let cache = PathBuf::from(cache);
        candidates.push(cache.join("whisper").join(&filename));
        candidates.push(cache.join("whisper.cpp").join(&filename));
    }

    let mut homes = Vec::new();
    for variable in ["USERPROFILE", "HOME"] {
        if let Some(home) = env::var_os(variable).filter(|value| !value.is_empty()) {
            let home = PathBuf::from(home);
            if !homes.contains(&home) {
                homes.push(home);
            }
        }
    }
    for home in &homes {
        candidates.push(home.join(".cache").join("whisper").join(&filename));
        candidates.push(home.join(".cache").join("whisper.cpp").join(&filename));
        candidates.push(
            home.join("Library")
                .join("Caches")
                .join("whisper")
                .join(&filename),
        );
        candidates.push(
            home.join("Library")
                .join("Caches")
                .join("whisper.cpp")
                .join(&filename),
        );
    }
    if let Some(cache) = env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
        let cache = PathBuf::from(cache);
        candidates.push(cache.join("whisper").join(&filename));
        candidates.push(cache.join("whisper.cpp").join(&filename));
    }

    let mut checked = Vec::new();
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
        if !checked.contains(&candidate) {
            checked.push(candidate);
        }
    }
    let Some(model) = supported_whisper_model(&filename) else {
        let searched = checked
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "Local whisper.cpp model '{configured}' was not found. Set LOCAL_WHISPER_MODEL to an existing ggml .bin file. Checked: {searched}"
        );
    };
    let destination = whisper_model_cache_dir()?.join(&filename);
    emit(ProgressEvent::Log(format!(
        "Downloading whisper.cpp model '{model}'…"
    )));
    download_whisper_model(model, &destination, emit)?;
    Ok(destination)
}

fn supported_whisper_model(filename: &str) -> Option<&str> {
    const MODELS: &[&str] = &[
        "tiny",
        "tiny.en",
        "tiny-q5_1",
        "tiny.en-q5_1",
        "tiny-q8_0",
        "base",
        "base.en",
        "base-q5_1",
        "base.en-q5_1",
        "base-q8_0",
        "small",
        "small.en",
        "small.en-tdrz",
        "small-q5_1",
        "small.en-q5_1",
        "small-q8_0",
        "medium",
        "medium.en",
        "medium-q5_0",
        "medium.en-q5_0",
        "medium-q8_0",
        "large-v1",
        "large-v2",
        "large-v2-q5_0",
        "large-v2-q8_0",
        "large-v3",
        "large-v3-q5_0",
        "large-v3-turbo",
        "large-v3-turbo-q5_0",
        "large-v3-turbo-q8_0",
    ];
    let model = filename.strip_prefix("ggml-")?.strip_suffix(".bin")?;
    MODELS.contains(&model).then_some(model)
}

fn whisper_model_cache_dir() -> Result<PathBuf> {
    for (variable, child) in [
        ("LOCALAPPDATA", "whisper.cpp"),
        ("XDG_CACHE_HOME", "whisper.cpp"),
    ] {
        if let Some(cache) = env::var_os(variable).filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(cache).join(child));
        }
    }
    for variable in ["USERPROFILE", "HOME"] {
        if let Some(home) = env::var_os(variable).filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(home).join(".cache").join("whisper.cpp"));
        }
    }
    env::current_dir().context("Could not determine the whisper.cpp model cache directory")
}

fn whisper_model_url(model: &str) -> Result<reqwest::Url> {
    if supported_whisper_model(&format!("ggml-{model}.bin")) != Some(model) {
        bail!("Unsupported whisper.cpp model '{model}'");
    }
    let base = if model.ends_with("-tdrz") {
        "https://huggingface.co/akashmjn/tinydiarize-whisper.cpp/resolve/main"
    } else {
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main"
    };
    reqwest::Url::parse(&format!("{base}/ggml-{model}.bin"))
        .context("Could not construct the whisper.cpp model download URL")
}

fn download_whisper_model(
    model: &str,
    destination: &Path,
    emit: &mut dyn FnMut(ProgressEvent),
) -> Result<()> {
    if destination
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
    {
        return Ok(());
    }
    let parent = destination
        .parent()
        .context("Whisper.cpp model destination has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("Could not create {}", parent.display()))?;
    let temporary = appended_suffix(destination, ".download");
    remove_invalid_cache(&temporary);
    let client = Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .context("Could not configure the whisper.cpp model downloader")?;
    let mut response = client
        .get(whisper_model_url(model)?)
        .send()
        .context("Could not download the whisper.cpp model")?
        .error_for_status()
        .context("Whisper.cpp model download returned an HTTP error")?;
    let mut file = fs::File::create(&temporary)
        .with_context(|| format!("Could not create {}", temporary.display()))?;
    std::io::copy(&mut response, &mut file)
        .context("Could not save the whisper.cpp model download")?;
    file.sync_all()
        .context("Could not finish the whisper.cpp model download")?;
    drop(file);
    if temporary.metadata()?.len() == 0 {
        remove_invalid_cache(&temporary);
        bail!("Whisper.cpp model download was empty");
    }
    fs::rename(&temporary, destination)
        .with_context(|| format!("Could not save {}", destination.display()))?;
    emit(ProgressEvent::Log(format!(
        "Whisper.cpp model saved to {}",
        destination.display()
    )));
    Ok(())
}

fn add_whisper_device_args(command: &mut Command, configured: &str) -> Result<()> {
    let device = configured.trim().to_ascii_lowercase();
    match device.as_str() {
        "" | "auto" => {}
        "cpu" => {
            command.arg("-ng");
        }
        "cuda" => {
            command.args(["-dev", "0"]);
        }
        value => {
            let index = value
                .strip_prefix("cuda:")
                .unwrap_or(value)
                .parse::<u32>()
                .ok()
                .filter(|index| *index <= i32::MAX as u32)
                .context(
                    "LOCAL_WHISPER_DEVICE must be auto, cpu, cuda, cuda:N, or a numeric GPU index",
                )?;
            command.args(["-dev"]).arg(index.to_string());
        }
    }
    Ok(())
}

fn add_whisper_vad_args(command: &mut Command, parameters: &serde_json::Value) {
    let mappings = [
        ("threshold", "--vad-threshold", false),
        (
            "min_speech_duration_ms",
            "--vad-min-speech-duration-ms",
            true,
        ),
        (
            "min_silence_duration_ms",
            "--vad-min-silence-duration-ms",
            true,
        ),
        ("speech_pad_ms", "--vad-speech-pad-ms", true),
    ];
    for (key, flag, integer) in mappings {
        let Some(number) = parameters
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .filter(|number| number.is_finite() && *number >= 0.0)
        else {
            continue;
        };
        if integer && number.fract() != 0.0 {
            continue;
        }
        let value = if integer {
            format!("{number:.0}")
        } else {
            number.to_string()
        };
        command.arg(flag).arg(value);
    }
}

fn ensure_wav(source: &Path, output_dir: &Path) -> Result<PathBuf> {
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if extension == "wav" {
        return Ok(source.to_path_buf());
    }

    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "Could not create output directory: {}",
            output_dir.display()
        )
    })?;
    let wav = output_dir.join(format!("{}_asr.wav", source_stem(source)?));
    let source_modified = fs::metadata(source)?.modified()?;
    if wav.metadata().is_ok_and(|metadata| {
        metadata.is_file()
            && metadata.len() > 0
            && metadata
                .modified()
                .is_ok_and(|modified| modified >= source_modified)
    }) {
        return Ok(wav);
    }

    let mut command = Command::new("ffmpeg");
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let output = command
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(source)
        .args(["-vn", "-ac", "1", "-ar", "16000"])
        .arg(&wav)
        .output()
        .context("Could not start FFmpeg WAV extraction")?;
    if !output.status.success() {
        remove_invalid_cache(&wav);
        bail!(
            "FFmpeg WAV extraction failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(wav)
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WhisperJson {
    WhisperCpp { transcription: Vec<WhisperSegment> },
    OpenAi { segments: Vec<OpenAiSegment> },
}

#[derive(Debug, Deserialize)]
struct WhisperSegment {
    text: String,
    timestamps: WhisperTimestamps,
}

#[derive(Debug, Deserialize)]
struct WhisperTimestamps {
    from: String,
    to: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiSegment {
    text: String,
    start: f64,
    end: f64,
}

fn parse_whisper_json(value: &str, media_duration: f64) -> Result<Transcript> {
    let parsed: WhisperJson =
        serde_json::from_str(value).context("Could not parse whisper JSON transcript")?;
    let segments = match parsed {
        WhisperJson::WhisperCpp { transcription } => transcription
            .into_iter()
            .map(|segment| {
                Ok(Segment {
                    start: parse_whisper_time(&segment.timestamps.from)?,
                    end: parse_whisper_time(&segment.timestamps.to)?,
                    text: segment.text.trim().to_string(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
        WhisperJson::OpenAi { segments } => segments
            .into_iter()
            .map(|segment| {
                Ok(Segment {
                    start: segment.start,
                    end: segment.end,
                    text: segment.text.trim().to_string(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
    };
    let final_end = segments.last().map(|segment| segment.end).unwrap_or(0.0);
    let duration = if media_duration > final_end {
        media_duration
    } else {
        final_end
    };
    Ok(Transcript { duration, segments })
}

fn parse_whisper_time(value: &str) -> Result<f64> {
    let normalized = value.trim().replace(',', ".");
    let parts = normalized.split(':').collect::<Vec<_>>();
    if parts.len() != 3 {
        bail!("Invalid whisper.cpp timestamp: {value}");
    }
    let hours: u64 = parts[0].parse()?;
    let minutes: u64 = parts[1].parse()?;
    let seconds: f64 = parts[2].parse()?;
    let timestamp = (hours * 3600 + minutes * 60) as f64 + seconds;
    if !timestamp.is_finite() {
        bail!("Invalid whisper.cpp timestamp: {value}");
    }
    Ok(timestamp)
}

fn write_srt_cache(source: &Path, output_dir: &Path, transcript: &Transcript) -> Result<()> {
    let path = transcript_cache_path(source, output_dir)?;
    let mut output = String::new();
    for (index, segment) in transcript.segments.iter().enumerate() {
        output.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            index + 1,
            srt_time(segment.start),
            srt_time(segment.end),
            sanitize_srt_text(&segment.text)
        ));
    }
    fs::write(&path, output)
        .with_context(|| format!("Could not cache transcript: {}", path.display()))
}

fn sanitize_srt_text(text: &str) -> String {
    text.trim().replace('\r', "").replace('\n', " ")
}

fn srt_time(seconds: f64) -> String {
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let total_seconds = total_ms / 1000;
    let s = total_seconds % 60;
    let m = (total_seconds / 60) % 60;
    let h = total_seconds / 3600;
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}

fn srt_line_is_blank(line: &str) -> bool {
    line.trim_matches('\u{feff}').trim().is_empty()
}

fn parse_srt(value: &str) -> Result<Transcript> {
    let normalized = value
        .strip_prefix('\u{feff}')
        .unwrap_or(value)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let lines = normalized.lines().collect::<Vec<_>>();
    let mut segments = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        while index < lines.len() && srt_line_is_blank(lines[index]) {
            index += 1;
        }
        let block_start = index;
        while index < lines.len() && !srt_line_is_blank(lines[index]) {
            index += 1;
        }
        let mut block = lines[block_start..index]
            .iter()
            .map(|line| line.trim_matches('\u{feff}'))
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        if block.len() > 1 && !block[0].contains("-->") && block[1].contains("-->") {
            block.remove(0);
        }
        let Some(timecode) = block.first() else {
            continue;
        };
        let Some((start, end)) = timecode.split_once("-->") else {
            continue;
        };
        let text = block[1..].join("\n").trim().to_string();
        if text.is_empty() {
            continue;
        }
        let start = parse_srt_time(start)?;
        let end = parse_srt_time(end)?;
        if !start.is_finite() || !end.is_finite() || start < 0.0 || end < start {
            bail!("Invalid SRT segment timing: {timecode}");
        }
        segments.push(Segment { start, end, text });
    }
    let duration = segments.last().map(|segment| segment.end).unwrap_or(0.0);
    Ok(Transcript { duration, segments })
}

fn parse_srt_time(value: &str) -> Result<f64> {
    let normalized = value.trim().replace(',', ".");
    let parts = normalized.split(':').collect::<Vec<_>>();
    if parts.len() != 3 {
        bail!("Invalid SRT timestamp: {value}");
    }
    let hours: f64 = parts[0].parse()?;
    let minutes: f64 = parts[1].parse()?;
    let seconds: f64 = parts[2].parse()?;
    Ok(hours * 3600.0 + minutes * 60.0 + seconds)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = env::temp_dir().join(format!(
                "shorts-transcribe-{name}-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn word(text: &str, start: f64, end: f64) -> ElevenLabsWord {
        ElevenLabsWord {
            text: text.to_string(),
            start,
            end,
            kind: None,
        }
    }

    #[test]
    fn cache_names_prefer_raw_stem_then_strip_resolution() {
        let temp = TestDir::new("cache-name");
        let source = temp.path.join("downloads").join("clip_1080.mp4");
        let output = temp.path.join("output");

        assert_eq!(
            transcript_cache_path(&source, &output).unwrap(),
            output.join("clip.srt")
        );
        fs::write(output.join("clip_1080.srt"), "").unwrap();
        assert_eq!(
            transcript_cache_path(&source, &output).unwrap(),
            output.join("clip_1080.srt")
        );
    }

    #[test]
    fn invalid_fresh_cache_is_removed() {
        let temp = TestDir::new("invalid-cache");
        let source = temp.path.join("clip.mp4");
        fs::write(&source, "media").unwrap();
        let output = temp.path.join("output");
        let cache = transcript_cache_path(&source, &output).unwrap();
        fs::write(&cache, "not an SRT").unwrap();

        assert!(cached_transcript(&source, &output).unwrap().is_none());
        assert!(!cache.exists());
    }

    #[test]
    fn parses_srt_bom_and_format_variants() {
        let transcript = parse_srt(
            "\u{feff}00:00:01.250 --> 00:00:02,500\r\nLine one\r\nLine two\r\n \t\r\n7\r\n00:00:03,000-->00:00:04.000\r\nSecond",
        )
        .unwrap();
        assert_eq!(transcript.duration, 4.0);
        assert_eq!(transcript.segments.len(), 2);
        assert_eq!(transcript.segments[0].start, 1.25);
        assert_eq!(transcript.segments[0].text, "Line one\nLine two");
        assert_eq!(transcript.segments[1].start, 3.0);
    }

    #[test]
    fn text_only_elevenlabs_response_uses_reported_duration() {
        let response: ElevenLabsResponse =
            serde_json::from_str(r#"{"text":"Hello world","audio_duration_secs":12.5}"#).unwrap();
        let transcript = transcript_from_elevenlabs(response);
        assert_eq!(transcript.duration, 12.5);
        assert_eq!(transcript.segments.len(), 1);
        assert_eq!(transcript.segments[0].start, 0.0);
        assert_eq!(transcript.segments[0].end, 12.5);
        assert_eq!(transcript.segments[0].text, "Hello world");
    }

    #[test]
    fn groups_after_including_the_eighteenth_word() {
        let words = (0..20)
            .map(|index| {
                word(
                    &format!("word{index}"),
                    index as f64 * 0.25,
                    index as f64 * 0.25 + 0.25,
                )
            })
            .collect();
        let segments = group_words(words, "", 0.0);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].text.split_whitespace().count(), 18);
        assert_eq!(segments[1].text.split_whitespace().count(), 2);
    }

    #[test]
    fn groups_after_including_the_word_that_reaches_eight_seconds() {
        let segments = group_words(
            vec![
                word("first", 0.004, 3.0),
                word("second", 3.0, 7.9),
                word("third", 8.0, 8.106),
                word("fourth", 8.2, 8.4),
            ],
            "",
            0.0,
        );
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].text, "first second third");
        assert_eq!(segments[0].start, 0.0);
        assert_eq!(segments[0].end, 8.11);
    }

    #[test]
    fn ellipsis_ends_segments_and_non_spoken_words_are_skipped() {
        let mut spacing = word(" ", 0.2, 0.3);
        spacing.kind = Some("spacing".into());
        let mut event = word("[music]", 0.3, 0.4);
        event.kind = Some("audio_event".into());
        let segments = group_words(
            vec![
                word("Wait…", 0.0, 0.2),
                spacing,
                event,
                word("next", 0.4, 0.6),
                word("Really...", 0.6, 1.0),
                word("last", 1.0, 1.2),
            ],
            "",
            0.0,
        );
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].text, "Wait…");
        assert_eq!(segments[1].text, "next Really...");
        assert_eq!(segments[2].text, "last");
    }

    #[test]
    fn parses_both_whisper_json_formats_and_uses_media_duration() {
        let cpp = parse_whisper_json(
            r#"{"transcription":[{"text":" Hello ","timestamps":{"from":"00:00:01,250","to":"00:00:03.500"}}]}"#,
            10.0,
        )
        .unwrap();
        assert_eq!(cpp.duration, 10.0);
        assert_eq!(cpp.segments[0].start, 1.25);
        assert_eq!(cpp.segments[0].text, "Hello");

        let openai = parse_whisper_json(
            r#"{"segments":[{"text":" First ","start":0.5,"end":4.75},{"text":"Second","start":5.0,"end":6.0}]}"#,
            4.0,
        )
        .unwrap();
        assert_eq!(openai.duration, 6.0);
        assert_eq!(openai.segments[1].text, "Second");
    }

    #[test]
    fn uses_supported_whisper_device_flags() {
        for (configured, expected) in [
            ("auto", vec![]),
            ("cpu", vec!["-ng"]),
            ("cuda", vec!["-dev", "0"]),
            ("cuda:2", vec!["-dev", "2"]),
            ("3", vec!["-dev", "3"]),
        ] {
            let mut command = Command::new("whisper-cli");
            add_whisper_device_args(&mut command, configured).unwrap();
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().to_string())
                .collect::<Vec<_>>();
            assert_eq!(args, expected, "{configured}");
        }
        let mut command = Command::new("whisper-cli");
        assert!(add_whisper_device_args(&mut command, "cuda:invalid").is_err());
    }

    #[test]
    fn downloads_only_allowlisted_official_whisper_models() {
        assert_eq!(
            whisper_model_url("base").unwrap().as_str(),
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin"
        );
        assert_eq!(
            whisper_model_url("small.en-tdrz").unwrap().as_str(),
            "https://huggingface.co/akashmjn/tinydiarize-whisper.cpp/resolve/main/ggml-small.en-tdrz.bin"
        );
        assert!(whisper_model_url("../../other").is_err());
        assert!(supported_whisper_model("ggml-custom.bin").is_none());
    }

    #[test]
    fn maps_only_supported_vad_parameters() {
        let mut command = Command::new("whisper-cli");
        add_whisper_vad_args(
            &mut command,
            &serde_json::json!({
                "threshold": 0.5,
                "min_speech_duration_ms": 250,
                "min_silence_duration_ms": 2000,
                "speech_pad_ms": 400,
                "unknown_parameter": 123
            }),
        );
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "--vad-threshold",
                "0.5",
                "--vad-min-speech-duration-ms",
                "250",
                "--vad-min-silence-duration-ms",
                "2000",
                "--vad-speech-pad-ms",
                "400",
            ]
        );
    }

    #[test]
    fn sanitizes_multiline_srt_text() {
        assert_eq!(sanitize_srt_text(" Hello\r\nworld "), "Hello world");
    }
}
