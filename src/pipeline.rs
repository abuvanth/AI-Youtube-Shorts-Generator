use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::{
    captions::{CaptionPreset, plan_captions},
    clip::{ReframeMode, RenderRequest, dimensions, render_short, validate_output_dir},
    config::Config,
    download::{SourceVideo, resolve_source},
    highlights::get_highlights,
    llm::{llm_for, transcription_summary},
    models::{GenerationResult, Highlight, Mode, RenderedShort, Transcript},
    muapi::{MuApiClient, result_url},
    progress::ProgressEvent,
    transcribe::transcribe,
};

#[derive(Debug, Clone, Args)]
pub struct GenerateOptions {
    /// HTTPS YouTube URL or local video path
    pub source: String,
    /// Local FFmpeg pipeline or MuAPI cloud autocrop
    #[arg(long, default_value = "local", value_parser = ["local", "api", "auto"])]
    pub mode: Mode,
    #[arg(long, default_value_t = 3)]
    pub num_clips: usize,
    #[arg(long, default_value_t = 30.0)]
    pub target_duration: f64,
    #[arg(long, default_value = "9:16")]
    pub aspect_ratio: String,
    #[arg(long, default_value_t = 1080)]
    pub format: u32,
    #[arg(long, default_value_t = 360)]
    pub analysis_format: u32,
    #[arg(long)]
    pub language: Option<String>,
    #[arg(short = 'p', long)]
    pub prompt: Option<String>,
    #[arg(long)]
    pub output_json: Option<PathBuf>,
    /// Burn transcript captions into locally rendered shorts. Off by default so existing CLI output stays unchanged.
    #[arg(long, default_value_t = false)]
    pub burn_captions: bool,
    #[arg(long, default_value_t = CaptionPreset::Bold)]
    pub caption_preset: CaptionPreset,
    #[arg(long, default_value_t = ReframeMode::Center)]
    pub reframe: ReframeMode,
    /// Burn a transcript-grounded text hook. Does not synthesize speech.
    #[arg(long, default_value_t = false)]
    pub text_hook: bool,
}

impl Default for GenerateOptions {
    fn default() -> Self {
        Self {
            source: String::new(),
            mode: Mode::Local,
            num_clips: 3,
            target_duration: 30.0,
            aspect_ratio: "9:16".into(),
            format: 1080,
            analysis_format: 360,
            language: None,
            prompt: None,
            output_json: None,
            burn_captions: false,
            caption_preset: CaptionPreset::Bold,
            reframe: ReframeMode::Center,
            text_hook: false,
        }
    }
}

pub fn generate(
    options: GenerateOptions,
    config: &Config,
    emit: &mut dyn FnMut(ProgressEvent),
) -> Result<GenerationResult> {
    generate_with(options, config, emit)
}

pub fn generate_with<F>(
    options: GenerateOptions,
    config: &Config,
    emit: F,
) -> Result<GenerationResult>
where
    F: FnMut(ProgressEvent),
{
    validate_options(&options)?;
    let options = GenerateOptions {
        mode: config.resolve_mode(options.mode)?,
        ..options
    };
    config.validate_mode(options.mode)?;
    if options.mode == Mode::Api {
        return generate_api(options, config, emit);
    }
    generate_local(options, config, emit)
}

fn generate_local<F>(
    options: GenerateOptions,
    config: &Config,
    mut emit: F,
) -> Result<GenerationResult>
where
    F: FnMut(ProgressEvent),
{
    let output_dir = validate_output_dir(config)?;
    fs::create_dir_all(&output_dir).context("Could not create output directory")?;
    emit(ProgressEvent::stage("Resolving source", 0.05));
    let mut forward = |event| emit(event);
    let source = resolve_source(
        &options.source,
        config,
        options.mode,
        options.analysis_format,
        &mut forward,
    )?;
    drop(forward);
    emit(ProgressEvent::SourceReady(source.path.clone()));
    emit(ProgressEvent::stage("Transcribing", 0.15));
    let mut forward = |event| emit(event);
    let transcript = transcribe(
        &source.path,
        options.language.as_deref(),
        config,
        &mut forward,
    )?;
    drop(forward);
    let transcript = if transcript.segments.is_empty() {
        emit(ProgressEvent::Log(
            "No speech found; continuing with visual-content analysis.".into(),
        ));
        visual_content_transcript(source.duration)
    } else {
        transcript
    };
    emit(ProgressEvent::Log(transcription_summary(
        &transcript,
        config,
    )));
    emit(ProgressEvent::stage("Selecting highlights", 0.35));
    let mut forward = |event| emit(event);
    let llm = llm_for(config, options.mode, &mut forward)?;
    drop(forward);
    let video = source
        .youtube_url
        .as_deref()
        .or_else(|| source.path.to_str());
    let highlights = get_highlights(
        &*llm,
        &transcript,
        options.num_clips,
        options.target_duration,
        &options.aspect_ratio,
        options.prompt.as_deref(),
        video,
    )?;
    emit(ProgressEvent::Log(format!(
        "{} highlight{} overlap the transcript. Scores are model estimates, not view predictions.",
        highlights.len(),
        if highlights.len() == 1 { "" } else { "s" }
    )));
    emit(ProgressEvent::stage("Rendering clips", 0.6));
    let selected = top_highlights(&highlights, options.num_clips);
    let (shorts, rendered_source) = render_local(
        config,
        &source,
        &transcript,
        &selected,
        &options,
        &output_dir,
        &mut emit,
    )?;
    emit(ProgressEvent::stage("Complete", 1.0));
    Ok(GenerationResult {
        mode: options.mode,
        source_video_url: rendered_source.to_string_lossy().into_owned(),
        transcript,
        highlights,
        shorts,
    })
}

fn generate_api<F>(
    options: GenerateOptions,
    config: &Config,
    mut emit: F,
) -> Result<GenerationResult>
where
    F: FnMut(ProgressEvent),
{
    let youtube_url = crate::download::validate_youtube_url(&options.source)
        .context("MuAPI mode requires an HTTPS YouTube URL; use local mode for local files")?;
    if options.burn_captions || options.text_hook || options.reframe != ReframeMode::Center {
        emit(ProgressEvent::Log(
            "MuAPI cloud rendering does not apply local captions, text hooks, or reframing.".into(),
        ));
    }
    emit(ProgressEvent::stage("Downloading source with MuAPI", 0.05));
    let client = MuApiClient::new(config)?;
    let download_payload = youtube_download_payload(&youtube_url, options.format);
    let request_id = client.submit("youtube-download", &download_payload)?;
    let source_url = result_url(client.poll(&request_id)?.value())?;

    emit(ProgressEvent::stage("Transcribing with MuAPI", 0.15));
    let transcript = client.transcribe_whisper(&source_url, options.language.as_deref())?;
    if transcript.segments.is_empty() {
        bail!("Whisper produced no segments. The video may have no detectable speech.");
    }
    emit(ProgressEvent::Log(format!(
        "MuAPI transcript: {} segments, {:.2} seconds.",
        transcript.segments.len(),
        transcript.duration
    )));

    emit(ProgressEvent::stage("Selecting highlights", 0.35));
    let llm = llm_for(config, options.mode, &mut emit)?;
    let highlights = get_highlights(
        &*llm,
        &transcript,
        options.num_clips,
        options.target_duration,
        &options.aspect_ratio,
        options.prompt.as_deref(),
        None,
    )?;
    emit(ProgressEvent::Log(format!(
        "{} highlight{} overlap the transcript. Scores are model estimates, not view predictions.",
        highlights.len(),
        if highlights.len() == 1 { "" } else { "s" }
    )));
    emit(ProgressEvent::stage("Autocropping clips", 0.6));
    let selected = top_highlights(&highlights, options.num_clips);
    let shorts = render_api(
        &client,
        &source_url,
        &selected,
        &options.aspect_ratio,
        &mut emit,
    )?;
    emit(ProgressEvent::stage("Complete", 1.0));
    Ok(GenerationResult {
        mode: options.mode,
        source_video_url: source_url,
        transcript,
        highlights,
        shorts,
    })
}

fn visual_content_transcript(duration: f64) -> Transcript {
    let duration = duration.max(30.0);
    Transcript {
        duration,
        segments: vec![crate::models::Segment {
            start: 0.0,
            end: duration,
            text: "(visual content)".into(),
        }],
    }
}

fn youtube_download_payload(source: &str, format: u32) -> serde_json::Value {
    serde_json::json!({
        "video_url": source,
        "format": format.to_string()
    })
}

fn top_highlights(highlights: &[Highlight], limit: usize) -> Vec<Highlight> {
    let mut pool = highlights.to_vec();
    pool.sort_by_key(|highlight| std::cmp::Reverse(highlight.score));
    let mut selected = Vec::new();
    while selected.len() < limit && !pool.is_empty() {
        let best_score = pool[0].score;
        let mut pick = 0;
        if !selected.is_empty() {
            let mut best_distance = nearest_separation(&pool[0], &selected);
            for (index, candidate) in pool.iter().enumerate().skip(1) {
                if candidate.score + 8 < best_score {
                    break;
                }
                let distance = nearest_separation(candidate, &selected);
                if distance > best_distance {
                    best_distance = distance;
                    pick = index;
                }
            }
        }
        selected.push(pool.remove(pick));
    }
    selected
}

fn nearest_separation(candidate: &Highlight, selected: &[Highlight]) -> f64 {
    selected
        .iter()
        .map(|chosen| {
            let forward = candidate.start_time - chosen.end_time;
            let backward = chosen.start_time - candidate.end_time;
            forward.max(backward)
        })
        .fold(f64::INFINITY, f64::min)
}

fn validate_options(options: &GenerateOptions) -> Result<()> {
    if options.source.trim().is_empty() {
        anyhow::bail!("Source cannot be empty");
    }
    if options.num_clips == 0 {
        anyhow::bail!("Number of clips must be at least 1");
    }
    if options.num_clips > 30 {
        anyhow::bail!("Number of clips cannot exceed 30");
    }
    if !options.target_duration.is_finite() || !(5.0..=600.0).contains(&options.target_duration) {
        anyhow::bail!("Target duration must be between 5 and 600 seconds");
    }
    crate::clip::dimensions(&options.aspect_ratio, options.format)?;
    if !(240..=2160).contains(&options.analysis_format) {
        anyhow::bail!("Analysis format must be between 240 and 2160");
    }
    Ok(())
}

fn render_local<F>(
    config: &Config,
    source: &SourceVideo,
    transcript: &Transcript,
    highlights: &[Highlight],
    options: &GenerateOptions,
    output_dir: &Path,
    emit: &mut F,
) -> Result<(Vec<RenderedShort>, PathBuf)>
where
    F: FnMut(ProgressEvent),
{
    let mut rendered_source = source.clone();
    if source.youtube_url.is_some() && options.format != options.analysis_format {
        emit(ProgressEvent::Log(
            "Downloading full-resolution source for final clips…".into(),
        ));
        rendered_source =
            resolve_source(&source.original, config, Mode::Local, options.format, emit)?;
    }
    let mut shorts = Vec::new();
    for (index, highlight) in highlights.iter().enumerate() {
        let fraction = 0.6 + 0.4 * index as f32 / highlights.len().max(1) as f32;
        emit(ProgressEvent::stage(
            format!("Rendering clip {}/{}", index + 1, highlights.len()),
            fraction,
        ));
        let (_, width, height) = dimensions(&options.aspect_ratio, options.format)?;
        let plan = plan_captions(
            transcript,
            highlight,
            options.caption_preset,
            width,
            height,
            options.burn_captions,
            options.text_hook,
        );
        if options.text_hook && plan.as_ref().is_none_or(|plan| !plan.burned_hook) {
            emit(ProgressEvent::Log(format!(
                "Text hook for \"{}\" was not burned because it does not match the transcript in this clip.",
                highlight.title
            )));
        }
        let request = RenderRequest {
            aspect_ratio: options.aspect_ratio.clone(),
            format: options.format,
            reframe: options.reframe,
            captions_ass: plan.as_ref().map(|plan| plan.ass.clone()),
            captions_srt: plan
                .as_ref()
                .map(|plan| plan.srt.clone())
                .filter(|srt| !srt.is_empty()),
            music_path: None,
            file_stem: None,
        };
        let result = render_short(
            &rendered_source.path,
            highlight,
            output_dir,
            index,
            &request,
            emit,
        );
        match result {
            Ok(path) => shorts.push(RenderedShort {
                highlight: highlight.clone(),
                clip_url: Some(path.to_string_lossy().into_owned()),
                error: None,
            }),
            Err(error) => shorts.push(RenderedShort {
                highlight: highlight.clone(),
                clip_url: None,
                error: Some(format!("{error:#}")),
            }),
        }
    }
    Ok((shorts, rendered_source.path))
}

fn render_api<F>(
    client: &MuApiClient,
    source_url: &str,
    highlights: &[Highlight],
    aspect_ratio: &str,
    emit: &mut F,
) -> Result<Vec<RenderedShort>>
where
    F: FnMut(ProgressEvent),
{
    let mut shorts = Vec::new();
    for (index, highlight) in highlights.iter().enumerate() {
        emit(ProgressEvent::stage(
            format!("Autocropping clip {}/{}", index + 1, highlights.len()),
            0.6 + 0.4 * index as f32 / highlights.len().max(1) as f32,
        ));
        let result = (|| {
            let request_id = client.submit(
                "autocrop",
                &serde_json::json!({
                    "video_url": source_url,
                    "start_time": highlight.start_time,
                    "end_time": highlight.end_time,
                    "aspect_ratio": aspect_ratio,
                }),
            )?;
            let response = client.poll(&request_id)?;
            let url = result_url(response.value())?;
            Ok::<_, anyhow::Error>(url)
        })();
        match result {
            Ok(url) => shorts.push(RenderedShort {
                highlight: highlight.clone(),
                clip_url: Some(url),
                error: None,
            }),
            Err(error) => shorts.push(RenderedShort {
                highlight: highlight.clone(),
                clip_url: None,
                error: Some(format!("{error:#}")),
            }),
        }
    }
    Ok(shorts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_python_cli() {
        let options = GenerateOptions::default();
        assert_eq!(options.mode, Mode::Local);
        assert_eq!(options.num_clips, 3);
        assert_eq!(options.target_duration, 30.0);
        assert_eq!(options.aspect_ratio, "9:16");
        assert_eq!(options.format, 1080);
        assert_eq!(options.analysis_format, 360);
    }

    #[test]
    fn cloud_download_matches_the_python_muapi_contract() {
        assert_eq!(
            youtube_download_payload("https://youtu.be/abc123", 1080),
            serde_json::json!({
                "video_url": "https://youtu.be/abc123",
                "format": "1080"
            })
        );
    }

    #[test]
    fn selects_only_the_top_scored_clips_for_rendering() {
        let highlights = (0..4)
            .map(|index| Highlight {
                title: format!("clip {index}"),
                start_time: index as f64 * 40.0,
                end_time: index as f64 * 40.0 + 30.0,
                score: 60 + index * 10,
                hook_sentence: String::new(),
                virality_reason: String::new(),
                category: String::new(),
            })
            .collect::<Vec<_>>();
        let selected = top_highlights(&highlights, 2);
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].title, "clip 3");
        assert_eq!(selected[1].title, "clip 2");
        assert_eq!(highlights.len(), 4);
    }

    #[test]
    fn near_equal_scores_prefer_a_spread_across_the_timeline() {
        let highlights = vec![
            Highlight {
                title: "opening".into(),
                start_time: 0.0,
                end_time: 30.0,
                score: 90,
                hook_sentence: String::new(),
                virality_reason: String::new(),
                category: String::new(),
            },
            Highlight {
                title: "nearby".into(),
                start_time: 40.0,
                end_time: 70.0,
                score: 88,
                hook_sentence: String::new(),
                virality_reason: String::new(),
                category: String::new(),
            },
            Highlight {
                title: "later".into(),
                start_time: 500.0,
                end_time: 530.0,
                score: 86,
                hook_sentence: String::new(),
                virality_reason: String::new(),
                category: String::new(),
            },
        ];
        let selected = top_highlights(&highlights, 2);
        assert_eq!(selected[0].title, "opening");
        assert_eq!(selected[1].title, "later");
    }

    #[test]
    fn visual_content_fallback_preserves_python_duration_behavior() {
        let fallback = visual_content_transcript(12.0);
        assert_eq!(fallback.duration, 30.0);
        assert_eq!(fallback.segments[0].text, "(visual content)");
        assert_eq!(visual_content_transcript(75.0).duration, 75.0);
    }

    #[test]
    fn successful_short_json_omits_the_optional_error_field() {
        let short = RenderedShort {
            highlight: Highlight {
                title: "Clip".into(),
                start_time: 0.0,
                end_time: 30.0,
                score: 90,
                hook_sentence: "Hook".into(),
                virality_reason: "Reason".into(),
                category: String::new(),
            },
            clip_url: Some("https://cdn.example/clip.mp4".into()),
            error: None,
        };
        let value = serde_json::to_value(short).unwrap();
        assert!(value.get("error").is_none());
        assert_eq!(value["clip_url"], "https://cdn.example/clip.mp4");
    }

    #[test]
    fn rejects_invalid_generation_options() {
        let mut options = GenerateOptions {
            source: "source.mp4".into(),
            num_clips: 0,
            ..Default::default()
        };
        assert!(validate_options(&options).is_err());
        options.num_clips = 3;
        options.target_duration = f64::NAN;
        assert!(validate_options(&options).is_err());
        options.target_duration = 30.0;
        options.aspect_ratio = "invalid".into();
        assert!(validate_options(&options).is_err());
    }
}
