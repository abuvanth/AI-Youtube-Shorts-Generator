use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use reqwest::{StatusCode, blocking::Client};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::{
    config::Config,
    models::{Segment, Transcript},
    progress::exponential_backoff,
};

const URL_KEYS: [&str; 6] = [
    "video_url",
    "url",
    "output_url",
    "result_url",
    "audio_url",
    "download_url",
];
const OUTPUT_CONTAINER_KEYS: [&str; 4] = ["outputs", "output", "result", "data"];
const TEXT_KEYS: [&str; 7] = [
    "outputs", "output", "text", "response", "result", "content", "data",
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Pending,
    Processing,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(transparent)]
pub struct JobResult(Value);

impl JobResult {
    pub fn value(&self) -> &Value {
        &self.0
    }
}

impl From<Value> for JobResult {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

#[derive(Debug, Serialize)]
pub struct SubmittedJob {
    pub request_id: String,
}

#[derive(Debug, Deserialize)]
struct JobResponse {
    request_id: Option<Value>,
    id: Option<Value>,
}

impl JobResponse {
    fn into_request_id(self) -> Result<String> {
        self.request_id
            .as_ref()
            .and_then(nonempty_job_id)
            .or_else(|| self.id.as_ref().and_then(nonempty_job_id))
            .ok_or_else(|| anyhow::anyhow!("MuAPI response did not include a request ID"))
    }
}

#[derive(Debug, Clone)]
pub struct MuApiClient {
    client: Client,
    base_url: String,
    api_key: String,
    poll_interval: Duration,
    poll_timeout: Duration,
}

impl MuApiClient {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .user_agent("ai-youtube-shorts-generator/0.1")
                .build()?,
            base_url: config.muapi_base_url.trim_end_matches('/').to_string(),
            api_key: config.muapi_api_key()?.to_string(),
            poll_interval: config.muapi_poll_interval,
            poll_timeout: config.muapi_poll_timeout,
        })
    }

    pub fn submit<T: Serialize + ?Sized>(&self, endpoint: &str, payload: &T) -> Result<String> {
        let endpoint = endpoint.trim_matches('/');
        let response = self
            .post_with_retry(format!("{}/{endpoint}", self.base_url), payload, 3)?
            .json::<JobResponse>()
            .context("Could not parse MuAPI submission response")?;
        response.into_request_id()
    }

    pub fn transcribe_whisper(
        &self,
        media_url: &str,
        language: Option<&str>,
    ) -> Result<Transcript> {
        let payload = whisper_payload(media_url, language);
        let request_id = self.submit("openai-whisper", &payload)?;
        parse_whisper_transcript(self.poll(&request_id)?.value())
    }

    fn fetch_result(&self, request_id: &str) -> Result<Value> {
        let mut last_error = None;
        for attempt in 0..3 {
            let result = self
                .client
                .get(format!("{}/predictions/{request_id}/result", self.base_url))
                .timeout(Duration::from_secs(90))
                .headers(self.headers())
                .send();
            match result {
                Ok(response) => {
                    let status = response.status();
                    if !status.is_success() {
                        let body = response
                            .text()
                            .unwrap_or_else(|error| format!("<unreadable response body: {error}>"));
                        bail!(
                            "MuAPI result request returned HTTP {status}: {}",
                            self.redact_secrets(&body)
                        );
                    }
                    return response
                        .json()
                        .context("Could not parse MuAPI result response");
                }
                Err(error) if attempt < 2 => {
                    last_error = Some(error);
                    thread::sleep(Duration::from_secs(2));
                }
                Err(error) => return Err(error).context("MuAPI result request failed"),
            }
        }
        Err(last_error.unwrap()).context("MuAPI result request failed")
    }

    pub fn poll(&self, request_id: &str) -> Result<JobResult> {
        self.poll_with_timeout(request_id, self.poll_timeout)
    }

    pub fn poll_with_timeout(&self, request_id: &str, timeout: Duration) -> Result<JobResult> {
        let started = Instant::now();
        loop {
            if started.elapsed() >= timeout {
                bail!("MuAPI job timed out after {} seconds", timeout.as_secs());
            }

            let response = self.fetch_result(request_id)?;
            let status = job_status(&response)?.to_ascii_lowercase();
            match status.as_str() {
                "completed" | "succeeded" | "success" => return Ok(response.into()),
                "failed" | "error" | "cancelled" => {
                    let detail = response
                        .get("error")
                        .cloned()
                        .unwrap_or_else(|| Value::String("Unknown MuAPI error".into()));
                    bail!(
                        "MuAPI job failed: {}",
                        self.redact_secrets(&detail.to_string())
                    );
                }
                "pending" | "queued" | "processing" | "in_progress" | "running" => {
                    let remaining = timeout.saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        bail!("MuAPI job timed out after {} seconds", timeout.as_secs());
                    }
                    thread::sleep(self.poll_interval.min(remaining));
                }
                status => bail!(
                    "MuAPI returned an unknown job status: {}",
                    self.redact_secrets(status)
                ),
            }
        }
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "x-api-key",
            self.api_key.parse().expect("validated MuAPI key"),
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().expect("valid content type"),
        );
        headers
    }

    fn post_with_retry<T: Serialize + ?Sized>(
        &self,
        url: String,
        payload: &T,
        retries: u32,
    ) -> Result<reqwest::blocking::Response> {
        with_retries(
            retries,
            || {
                let response = match self
                    .client
                    .post(&url)
                    .headers(self.headers())
                    .json(payload)
                    .send()
                {
                    Ok(response) => response,
                    Err(error) => {
                        return Err(AttemptError {
                            error: anyhow::anyhow!(
                                "MuAPI POST {url} failed: {}",
                                self.redact_secrets(&error.without_url().to_string())
                            ),
                            retryable: true,
                        });
                    }
                };

                if response.status().is_success() {
                    return Ok(response);
                }

                let status = response.status();
                let body = response
                    .text()
                    .unwrap_or_else(|error| format!("<unreadable response body: {error}>"));
                Err(AttemptError {
                    error: anyhow::anyhow!(
                        "MuAPI POST {url} failed with HTTP {status}: {}",
                        self.redact_secrets(&body)
                    ),
                    retryable: is_retryable(status),
                })
            },
            thread::sleep,
        )
    }

    fn redact_secrets(&self, text: &str) -> String {
        text.replace(&self.api_key, "[REDACTED]")
    }
}

struct AttemptError {
    error: anyhow::Error,
    retryable: bool,
}

fn with_retries<T>(
    retries: u32,
    mut send: impl FnMut() -> std::result::Result<T, AttemptError>,
    mut sleep: impl FnMut(Duration),
) -> Result<T> {
    if retries == 0 {
        bail!("MuAPI POST requires at least one attempt");
    }

    let mut attempt = 0;
    loop {
        attempt += 1;
        match send() {
            Ok(value) => return Ok(value),
            Err(failure) if failure.retryable && attempt < retries => {
                sleep(exponential_backoff(
                    attempt - 1,
                    Duration::from_secs(1),
                    Duration::from_secs(4),
                ));
            }
            Err(failure) => return Err(failure.error),
        }
    }
}

fn is_retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT
        || status.is_server_error()
}

fn nonempty_job_id(value: &Value) -> Option<String> {
    match value {
        Value::String(id) if !id.trim().is_empty() => Some(id.clone()),
        Value::Number(id)
            if id.as_u64() != Some(0) && id.as_i64() != Some(0) && id.as_f64() != Some(0.0) =>
        {
            Some(id.to_string())
        }
        _ => None,
    }
}

fn job_status(value: &Value) -> Result<&str> {
    match value.get("status") {
        None | Some(Value::Null) => Ok("pending"),
        Some(Value::String(status)) if status.is_empty() => Ok("pending"),
        Some(Value::String(status)) => Ok(status),
        Some(status) => bail!("MuAPI returned a non-string job status: {status}"),
    }
}

pub fn result_url(value: &Value) -> Result<String> {
    for key in OUTPUT_CONTAINER_KEYS {
        if let Some(url) = value.get(key).and_then(find_url) {
            return Ok(url);
        }
    }
    find_url(value)
        .ok_or_else(|| anyhow::anyhow!("MuAPI result did not contain an HTTPS output URL"))
}

fn find_url(value: &Value) -> Option<String> {
    match value {
        Value::String(url) => https_url(url),
        Value::Array(values) => values.iter().find_map(find_url),
        Value::Object(values) => {
            for key in URL_KEYS {
                if let Some(url) = values.get(key).and_then(find_url) {
                    return Some(url);
                }
            }
            for key in OUTPUT_CONTAINER_KEYS {
                if let Some(url) = values.get(key).and_then(find_url) {
                    return Some(url);
                }
            }
            values.iter().find_map(|(key, value)| {
                if is_error_key(key)
                    || URL_KEYS.contains(&key.as_str())
                    || OUTPUT_CONTAINER_KEYS.contains(&key.as_str())
                {
                    None
                } else {
                    find_url(value)
                }
            })
        }
        _ => None,
    }
}

fn https_url(value: &str) -> Option<String> {
    let value = value.trim();
    let parsed = Url::parse(value).ok()?;
    (parsed.scheme() == "https" && parsed.host_str().is_some()).then_some(value.to_string())
}

pub fn result_text(value: &Value) -> Result<String> {
    find_text(value).ok_or_else(|| anyhow::anyhow!("MuAPI result did not contain generated text"))
}

fn find_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Array(values) => values.iter().find_map(find_text),
        Value::Object(values) => {
            for key in TEXT_KEYS {
                if let Some(text) = values.get(key).and_then(find_text) {
                    return Some(text);
                }
            }
            values.iter().find_map(|(key, value)| {
                if is_error_key(key) || TEXT_KEYS.contains(&key.as_str()) {
                    None
                } else {
                    find_text(value)
                }
            })
        }
        _ => None,
    }
}

fn is_error_key(key: &str) -> bool {
    key.to_ascii_lowercase().contains("error")
}

fn whisper_payload(media_url: &str, language: Option<&str>) -> Value {
    let mut payload = serde_json::json!({
        "audio_url": media_url,
        "response_format": "verbose_json",
    });
    if let Some(language) = language.map(str::trim).filter(|value| !value.is_empty()) {
        payload["language"] = Value::String(language.to_string());
    }
    payload
}

fn parse_whisper_transcript(value: &Value) -> Result<Transcript> {
    let verbose = extract_verbose_payload(value)?;
    let segments = match verbose
        .get("segments")
        .context("MuAPI Whisper response did not include segments")?
    {
        Value::Null => Vec::new(),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                let fields = value
                    .as_object()
                    .context("MuAPI Whisper segment must be an object")?;
                let start = fields
                    .get("start")
                    .map(|value| parse_transcript_number(value, "segment start"))
                    .transpose()?
                    .unwrap_or(0.0);
                let end = fields
                    .get("end")
                    .map(|value| parse_transcript_number(value, "segment end"))
                    .transpose()?
                    .unwrap_or(0.0);
                let text = match fields.get("text") {
                    None | Some(Value::Null) => String::new(),
                    Some(Value::String(text)) => text.trim().to_string(),
                    _ => bail!("MuAPI Whisper segment text must be a string"),
                };
                Ok(Segment { start, end, text })
            })
            .collect::<Result<Vec<_>>>()?,
        _ => bail!("MuAPI Whisper segments must be an array"),
    };

    let fallback_duration = segments.last().map_or(0.0, |segment| segment.end);
    let duration = match verbose.get("duration") {
        None | Some(Value::Null) => fallback_duration,
        Some(Value::String(text)) if text.trim().is_empty() => fallback_duration,
        Some(value) => {
            let duration = parse_transcript_number(value, "duration")?;
            if duration == 0.0 {
                fallback_duration
            } else {
                duration
            }
        }
    };

    Ok(Transcript { duration, segments })
}

fn parse_transcript_number(value: &Value, field: &str) -> Result<f64> {
    let number = match value {
        Value::Number(number) => number
            .as_f64()
            .with_context(|| format!("MuAPI Whisper {field} is not a valid number"))?,
        Value::String(text) => text
            .trim()
            .parse::<f64>()
            .with_context(|| format!("MuAPI Whisper {field} is not a valid number"))?,
        _ => bail!("MuAPI Whisper {field} must be a number"),
    };
    if !number.is_finite() {
        bail!("MuAPI Whisper {field} must be finite");
    }
    Ok(number)
}

fn extract_verbose_payload(value: &Value) -> Result<Value> {
    let root = coerce_verbose(value).unwrap_or_else(|| value.clone());
    for key in ["output", "result", "outputs"] {
        if let Some(payload) = root.get(key).and_then(find_verbose_payload) {
            return Ok(payload);
        }
    }
    if root
        .as_object()
        .is_some_and(|fields| fields.contains_key("segments"))
    {
        return Ok(root);
    }
    bail!("Could not find Whisper segments in MuAPI response")
}

fn find_verbose_payload(value: &Value) -> Option<Value> {
    let value = coerce_verbose(value)?;
    if value
        .as_object()
        .is_some_and(|fields| fields.contains_key("segments"))
    {
        return Some(value);
    }
    value
        .as_array()
        .and_then(|values| values.iter().find_map(find_verbose_payload))
}

fn coerce_verbose(value: &Value) -> Option<Value> {
    match value {
        Value::Object(_) | Value::Array(_) => Some(value.clone()),
        Value::String(raw) => serde_json::from_str::<Value>(raw)
            .ok()
            .filter(|value| value.is_object() || value.is_array()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn post_retry_helper_caps_total_attempts_and_preserves_backoff() {
        let mut attempts = 0;
        let mut delays = Vec::new();
        let error = with_retries(
            3,
            || {
                attempts += 1;
                Err::<(), _>(AttemptError {
                    error: anyhow::anyhow!("HTTP 503: service unavailable"),
                    retryable: true,
                })
            },
            |delay| delays.push(delay),
        )
        .unwrap_err();

        assert_eq!(attempts, 3);
        assert_eq!(delays, vec![Duration::from_secs(1), Duration::from_secs(2)]);
        assert!(error.to_string().contains("503"));
        assert!(error.to_string().contains("service unavailable"));
    }

    #[test]
    fn post_retry_helper_stops_immediately_on_nonretryable_http_errors() {
        let mut attempts = 0;
        let mut delays = Vec::new();
        let error = with_retries(
            3,
            || {
                attempts += 1;
                Err::<(), _>(AttemptError {
                    error: anyhow::anyhow!("HTTP 400: invalid payload"),
                    retryable: false,
                })
            },
            |delay| delays.push(delay),
        )
        .unwrap_err();

        assert_eq!(attempts, 1);
        assert!(delays.is_empty());
        assert!(error.to_string().contains("400"));
    }

    #[test]
    fn post_retry_helper_returns_success_within_the_attempt_limit() {
        let mut attempts = 0;
        let result = with_retries(
            3,
            || {
                attempts += 1;
                if attempts == 3 {
                    Ok("submitted")
                } else {
                    Err(AttemptError {
                        error: anyhow::anyhow!("temporary transport error"),
                        retryable: true,
                    })
                }
            },
            |_| {},
        );

        assert_eq!(result.unwrap(), "submitted");
        assert_eq!(attempts, 3);
    }

    #[test]
    fn retryable_statuses_match_transport_policy() {
        for status in [
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
        ] {
            assert!(is_retryable(status));
        }
        assert!(!is_retryable(StatusCode::BAD_REQUEST));
        assert!(!is_retryable(StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn extracts_nested_output_urls() {
        assert_eq!(
            result_url(&json!({"data": {"video": {"url": "https://cdn.example/video.mp4"}}}))
                .unwrap(),
            "https://cdn.example/video.mp4"
        );
        assert!(result_url(&json!({"data": {"url": "http://cdn.example/video.mp4"}})).is_err());
    }

    #[test]
    fn null_containers_do_not_shadow_output_urls() {
        assert_eq!(
            result_url(&json!({
                "data": null,
                "output": null,
                "result": {"video_url": "https://cdn.example/result.mp4"}
            }))
            .unwrap(),
            "https://cdn.example/result.mp4"
        );
        assert_eq!(
            result_url(&json!({"data": null, "url": "https://cdn.example/top.mp4"})).unwrap(),
            "https://cdn.example/top.mp4"
        );
    }

    #[test]
    fn extracts_outputs_and_never_uses_error_urls() {
        assert_eq!(
            result_url(&json!({
                "error": {"url": "https://error.example/not-an-output"},
                "outputs": [{"download_url": "https://cdn.example/output.mp4"}]
            }))
            .unwrap(),
            "https://cdn.example/output.mp4"
        );
        assert!(result_url(&json!({"error": {"url": "https://error.example/failure"}})).is_err());
    }

    #[test]
    fn extracts_nested_generated_text() {
        assert_eq!(
            result_text(&json!({"output": [{"content": "hello"}]})).unwrap(),
            "hello"
        );
        assert!(result_text(&json!({"output": {"score": 1}})).is_err());
    }

    #[test]
    fn text_extraction_skips_empty_values_and_returns_complete_text() {
        let complete = "First line\nSecond line";
        assert_eq!(
            result_text(&json!({
                "outputs": [{"text": "   "}, {"response": {"content": complete}}],
                "data": null
            }))
            .unwrap(),
            complete
        );
        assert_eq!(
            result_text(&json!({"output": null, "text": "fallback"})).unwrap(),
            "fallback"
        );
        assert!(result_text(&json!({"output": [null, "", "  "]})).is_err());
    }

    #[test]
    fn job_results_preserve_the_complete_raw_response() {
        let raw = json!({
            "status": "completed",
            "request_id": 42,
            "outputs": [{"url": "https://cdn.example/video.mp4"}],
            "url": "https://cdn.example/top-level.mp4",
            "metadata": {"custom": true}
        });
        let result = JobResult::from(raw.clone());

        assert_eq!(result.value(), &raw);
        assert_eq!(serde_json::to_value(result).unwrap(), raw);
    }

    #[test]
    fn submission_accepts_string_and_numeric_job_ids() {
        let string_id: JobResponse =
            serde_json::from_value(json!({"request_id": "job-1"})).unwrap();
        assert_eq!(string_id.into_request_id().unwrap(), "job-1");

        let numeric_id: JobResponse = serde_json::from_value(json!({"request_id": 12345})).unwrap();
        assert_eq!(numeric_id.into_request_id().unwrap(), "12345");

        let fallback_id: JobResponse =
            serde_json::from_value(json!({"request_id": 0, "id": 67890})).unwrap();
        assert_eq!(fallback_id.into_request_id().unwrap(), "67890");
    }

    #[test]
    fn whisper_payload_uses_verbose_json_and_optional_language() {
        assert_eq!(
            whisper_payload("https://cdn.example/audio.mp3", None),
            json!({
                "audio_url": "https://cdn.example/audio.mp3",
                "response_format": "verbose_json"
            })
        );
        assert_eq!(
            whisper_payload("https://cdn.example/audio.mp3", Some(" en ")),
            json!({
                "audio_url": "https://cdn.example/audio.mp3",
                "response_format": "verbose_json",
                "language": "en"
            })
        );
    }

    #[test]
    fn parses_json_string_whisper_transcripts() {
        let transcript = parse_whisper_transcript(&json!({
            "output": "{\"duration\":4.5,\"segments\":[{\"start\":0.5,\"end\":2.0,\"text\":\" Hello \"},{\"start\":2.0,\"end\":4.5,\"text\":\"world\"}]}"
        }))
        .unwrap();

        assert_eq!(transcript.duration, 4.5);
        assert_eq!(transcript.segments.len(), 2);
        assert_eq!(transcript.segments[0].text, "Hello");
        assert_eq!(transcript.segments[1].end, 4.5);
    }

    #[test]
    fn parses_whisper_outputs_and_falls_back_to_the_last_segment_end() {
        let transcript = parse_whisper_transcript(&json!({
            "output": null,
            "outputs": [{"segments": [{"start": 0, "end": 3.25, "text": "speech"}]}]
        }))
        .unwrap();

        assert_eq!(transcript.duration, 3.25);
        assert_eq!(transcript.segments[0].start, 0.0);
        assert_eq!(transcript.segments[0].text, "speech");
    }

    #[test]
    fn missing_whisper_segments_are_an_error() {
        assert!(parse_whisper_transcript(&json!({"output": {"duration": 5.0}})).is_err());
        assert!(parse_whisper_transcript(&json!({"result": "not json"})).is_err());
    }
}
