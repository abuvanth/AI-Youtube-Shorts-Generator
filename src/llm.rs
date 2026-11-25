use std::{fs, time::Duration};

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use crate::{
    config::{AsrProvider, Config, LlmProvider},
    highlights::extract_json,
    models::{Mode, Transcript},
    muapi::{MuApiClient, result_text},
    progress::ProgressEvent,
};

pub trait Llm: Send + Sync {
    fn complete(&self, prompt: &str, video: Option<&str>) -> Result<String>;
}

pub fn llm_for(
    config: &Config,
    mode: Mode,
    emit: &mut dyn FnMut(ProgressEvent),
) -> Result<Box<dyn Llm>> {
    match mode {
        Mode::Auto => llm_for(config, config.resolve_mode(mode)?, emit),
        Mode::Api => {
            emit(ProgressEvent::Log(
                "Using MuAPI GPT for highlight extraction…".into(),
            ));
            Ok(Box::new(MuApiLlm::new(config)?))
        }
        Mode::Local => match config.llm_provider {
            LlmProvider::Gemini => {
                emit(ProgressEvent::Log(
                    "Using Gemini video understanding…".into(),
                ));
                Ok(Box::new(GeminiClient::new(config)?))
            }
            LlmProvider::OpenAi => {
                emit(ProgressEvent::Log(
                    "Using OpenAI for highlight extraction…".into(),
                ));
                Ok(Box::new(OpenAiClient::new(config)?))
            }
        },
    }
}

pub struct OpenAiClient {
    client: Client,
    api_key: String,
    model: String,
}

impl OpenAiClient {
    fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            client: http_client()?,
            api_key: config.openai_api_key()?.to_string(),
            model: config.openai_model.clone(),
        })
    }
}

impl Llm for OpenAiClient {
    fn complete(&self, prompt: &str, _video: Option<&str>) -> Result<String> {
        let response = self
            .client
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(&self.api_key)
            .json(&json!({
                "model": self.model,
                "temperature": 0.7,
                "messages": [{"role": "user", "content": prompt}],
            }))
            .send()
            .context("OpenAI request failed")?
            .error_for_status()
            .context("OpenAI returned an HTTP error")?
            .json::<OpenAiResponse>()?;
        response
            .choices
            .into_iter()
            .find_map(|choice| {
                choice
                    .message
                    .content
                    .filter(|content| !content.trim().is_empty())
            })
            .ok_or_else(|| anyhow::anyhow!("OpenAI returned no text"))
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    content: Option<String>,
}

pub struct MuApiLlm {
    client: MuApiClient,
}

impl MuApiLlm {
    fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            client: MuApiClient::new(config)?,
        })
    }
}

impl Llm for MuApiLlm {
    fn complete(&self, prompt: &str, _video: Option<&str>) -> Result<String> {
        let request_id = self
            .client
            .submit("gpt-5-mini", &json!({"prompt": prompt}))?;
        result_text(
            self.client
                .poll_with_timeout(&request_id, Duration::from_secs(300))?
                .value(),
        )
    }
}

pub struct GeminiClient {
    client: Client,
    api_key: String,
    model: String,
}

impl GeminiClient {
    fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            client: http_client()?,
            api_key: config.gemini_api_key()?.to_string(),
            model: config.gemini_model.clone(),
        })
    }
}

impl Llm for GeminiClient {
    fn complete(&self, prompt: &str, video: Option<&str>) -> Result<String> {
        let video = video.filter(|value| !value.trim().is_empty());
        if video.is_some_and(is_youtube_url) {
            match self.complete_interactions(prompt, video.unwrap()) {
                Ok(text) => return Ok(text),
                Err(error) => {
                    tracing_fallback(&error);
                }
            }
        } else if let Some(path) = video {
            match self.complete_local(prompt, path) {
                Ok(text) => return Ok(text),
                Err(error) => {
                    tracing_fallback(&error);
                }
            }
        }
        self.complete_transcript(prompt)
    }
}

impl GeminiClient {
    fn complete_interactions(&self, prompt: &str, video: &str) -> Result<String> {
        let response = self
            .client
            .post("https://generativelanguage.googleapis.com/v1beta/interactions")
            .header("x-goog-api-key", &self.api_key)
            .json(&gemini_interactions_request(&self.model, prompt, video))
            .send()
            .context("Gemini Interactions request failed")?
            .error_for_status()
            .context("Gemini Interactions returned an HTTP error")?
            .json::<Value>()?;
        gemini_response_text(response)
    }

    fn complete_local(&self, prompt: &str, path: &str) -> Result<String> {
        let path = std::path::Path::new(path);
        let metadata = fs::metadata(path).context("Could not read local video for Gemini")?;
        if metadata.len() > 2 * 1024 * 1024 * 1024 {
            bail!("Local video is too large for the Gemini Files API");
        }
        let mime_type = match path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "mp4" => "video/mp4",
            "webm" => "video/webm",
            "mov" => "video/quicktime",
            "mkv" => "video/x-matroska",
            _ => bail!("Unsupported video format for Gemini"),
        };
        let start = self
            .client
            .post("https://generativelanguage.googleapis.com/upload/v1beta/files")
            .header("x-goog-api-key", &self.api_key)
            .header("X-Goog-Upload-Protocol", "resumable")
            .header("X-Goog-Upload-Command", "start")
            .header("X-Goog-Upload-Header-Content-Length", metadata.len())
            .header("X-Goog-Upload-Header-Content-Type", mime_type)
            .json(&json!({"file": {"display_name": path.file_name().and_then(|value| value.to_str()).unwrap_or("source-video")}}))
            .send()
            .context("Could not start Gemini file upload")?
            .error_for_status()
            .context("Gemini file upload initialization failed")?;
        let upload_url = start
            .headers()
            .get("x-goog-upload-url")
            .and_then(|value| value.to_str().ok())
            .context("Gemini did not return an upload URL")?
            .to_string();
        let upload_url =
            Url::parse(&upload_url).context("Gemini returned an invalid upload URL")?;
        if upload_url.scheme() != "https"
            || !upload_url
                .host_str()
                .is_some_and(|host| host == "generativelanguage.googleapis.com")
        {
            bail!("Gemini returned an unsafe upload URL");
        }
        let bytes = fs::read(path).context("Could not read local video for Gemini")?;
        let uploaded = self
            .client
            .post(upload_url)
            .header("X-Goog-Upload-Protocol", "resumable")
            .header("X-Goog-Upload-Command", "upload, finalize")
            .header("Content-Length", bytes.len())
            .header("Content-Type", mime_type)
            .body(bytes)
            .send()
            .context("Gemini file upload failed")?
            .error_for_status()
            .context("Gemini rejected the video upload")?
            .json::<GeminiFile>()?;
        let name = uploaded.file.name;
        let result = (|| {
            let mut state = uploaded.file.state.unwrap_or_default();
            for _ in 0..120 {
                if state == "ACTIVE" {
                    let response = self
                        .client
                        .post(format!("https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent", self.model))
                        .header("x-goog-api-key", &self.api_key)
                        .json(&gemini_request(prompt, json!({"mime_type": mime_type, "file_uri": format!("https://generativelanguage.googleapis.com/v1beta/{name}")})))
                        .send()
                        .context("Gemini generateContent request failed")?
                        .error_for_status()
                        .context("Gemini generateContent returned an HTTP error")?
                        .json::<Value>()?;
                    return gemini_response_text(response);
                }
                if state == "FAILED" {
                    bail!("Gemini video processing failed");
                }
                std::thread::sleep(Duration::from_secs(5));
                state = self
                    .client
                    .get(format!(
                        "https://generativelanguage.googleapis.com/v1beta/{name}"
                    ))
                    .header("x-goog-api-key", &self.api_key)
                    .send()?
                    .error_for_status()?
                    .json::<GeminiFile>()?
                    .file
                    .state
                    .unwrap_or_default();
            }
            bail!("Gemini video processing timed out")
        })();
        let _ = self
            .client
            .delete(format!(
                "https://generativelanguage.googleapis.com/v1beta/{name}"
            ))
            .header("x-goog-api-key", &self.api_key)
            .send();
        result
    }

    fn complete_transcript(&self, prompt: &str) -> Result<String> {
        let response = self
            .client
            .post(format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
                self.model
            ))
            .header("x-goog-api-key", &self.api_key)
            .json(&gemini_request(prompt, Value::Null))
            .send()
            .context("Gemini transcript request failed")?
            .error_for_status()
            .context("Gemini transcript request returned an HTTP error")?
            .json::<Value>()?;
        gemini_response_text(response)
    }
}

fn gemini_interactions_request(model: &str, prompt: &str, video: &str) -> Value {
    json!({
        "model": model,
        "input": [
            {"type": "text", "text": prompt},
            {"type": "video", "uri": video}
        ]
    })
}

fn gemini_request(prompt: &str, video: Value) -> Value {
    let mut parts = vec![json!({"text": prompt})];
    if !video.is_null() {
        parts.push(json!({"file_data": video}));
    }
    json!({
        "contents": [{"role": "user", "parts": parts}],
        "generationConfig": {
            "temperature": 0.2,
            "responseMimeType": "application/json",
            "maxOutputTokens": 8192
        }
    })
}

#[derive(Debug, Deserialize)]
struct GeminiFile {
    file: GeminiFileInner,
}

#[derive(Debug, Deserialize)]
struct GeminiFileInner {
    name: String,
    state: Option<String>,
}

fn gemini_response_text(response: Value) -> Result<String> {
    let texts = response
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .or_else(|| {
            response
                .pointer("/output/0/content")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
        })
        .or_else(|| {
            response
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    if texts.trim().is_empty() {
        let reason = response
            .pointer("/promptFeedback/blockReason")
            .and_then(Value::as_str)
            .unwrap_or("no text returned");
        bail!("Gemini returned no usable text ({reason})");
    }
    Ok(extract_json(&texts).unwrap_or(texts))
}

fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent("ai-youtube-shorts-generator/0.1")
        .build()?)
}

fn is_youtube_url(value: &str) -> bool {
    Url::parse(value)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| {
            crate::download::validate_youtube_url(&format!("https://{host}/")).is_ok()
        })
}

fn tracing_fallback(error: &anyhow::Error) {
    eprintln!("Gemini video understanding failed; using transcript-only generation: {error:#}");
}

pub fn build_transcript_text(transcript: &Transcript) -> String {
    transcript
        .segments
        .iter()
        .map(|segment| {
            format!(
                "[{:.2}-{:.2}] {}",
                segment.start,
                segment.end,
                segment.text.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn transcription_summary(transcript: &Transcript, config: &Config) -> String {
    format!(
        "Video duration: {:.2} seconds. Transcript provider: {}. Word segments: {}.",
        transcript.duration,
        match config.asr_provider {
            AsrProvider::ElevenLabs => "ElevenLabs",
            AsrProvider::Whisper => "whisper.cpp",
        },
        transcript.segments.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gemini_generate_response() {
        let response = json!({"candidates": [{"content": {"parts": [{"text": "```json\n{\"ok\":true}\n```"}]}}]});
        assert_eq!(gemini_response_text(response).unwrap(), "{\"ok\":true}");
    }

    #[test]
    fn parses_gemini_interactions_response() {
        let response =
            json!({"output": [{"content": [{"type": "text", "text": "[{\"title\":\"clip\"}]"}]}]});
        assert_eq!(
            gemini_response_text(response).unwrap(),
            "[{\"title\":\"clip\"}]"
        );
    }

    #[test]
    fn builds_gemini_interactions_video_input() {
        let request = gemini_interactions_request(
            "gemini-4-hydrogen",
            "Find highlights",
            "https://www.youtube.com/watch?v=abc",
        );
        assert_eq!(
            request,
            json!({
                "model": "gemini-4-hydrogen",
                "input": [
                    {"type": "text", "text": "Find highlights"},
                    {"type": "video", "uri": "https://www.youtube.com/watch?v=abc"}
                ]
            })
        );
    }

    #[test]
    fn rejects_non_youtube_video_input() {
        assert!(!is_youtube_url("/tmp/source.mp4"));
        assert!(!is_youtube_url("https://example.com/video.mp4"));
    }
}
