use std::{env, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::{Value, json};
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    Gemini,
    OpenAi,
}

impl LlmProvider {
    fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "gemini" => Ok(Self::Gemini),
            "openai" | "chatgpt" => Ok(Self::OpenAi),
            _ => bail!("LLM_PROVIDER must be gemini or openai"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrProvider {
    ElevenLabs,
    Whisper,
}

impl AsrProvider {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "elevenlabs" => Ok(Self::ElevenLabs),
            "whisper" | "local" | "faster-whisper" => Ok(Self::Whisper),
            _ => bail!("ASR_PROVIDER must be elevenlabs or whisper"),
        }
    }

    #[cfg(feature = "gui")]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::ElevenLabs => "elevenlabs",
            Self::Whisper => "whisper",
        }
    }
}

#[cfg(feature = "gui")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhisperModelSize {
    Tiny,
    Base,
    Small,
    Medium,
    Large,
}

#[cfg(feature = "gui")]
impl WhisperModelSize {
    pub(crate) const ALL: [Self; 5] = [
        Self::Tiny,
        Self::Base,
        Self::Small,
        Self::Medium,
        Self::Large,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Tiny => "Tiny",
            Self::Base => "Base",
            Self::Small => "Small",
            Self::Medium => "Medium",
            Self::Large => "Large",
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Tiny => "tiny",
            Self::Base => "base",
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large-v3",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "tiny" => Some(Self::Tiny),
            "base" => Some(Self::Base),
            "small" => Some(Self::Small),
            "medium" => Some(Self::Medium),
            "large" | "large-v3" => Some(Self::Large),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub muapi_api_key: Option<String>,
    pub muapi_base_url: String,
    pub muapi_poll_interval: Duration,
    pub muapi_poll_timeout: Duration,
    pub llm_provider: LlmProvider,
    pub gemini_api_key: Option<String>,
    pub gemini_model: String,
    pub openai_api_key: Option<String>,
    pub openai_model: String,
    pub asr_provider: AsrProvider,
    pub elevenlabs_api_key: Option<String>,
    pub elevenlabs_scribe_model: String,
    pub local_whisper_model: String,
    pub local_whisper_device: String,
    pub local_whisper_vad_filter: bool,
    pub local_whisper_vad_parameters: Value,
    pub output_dir: PathBuf,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();
        let muapi_base_url = env::var("MUAPI_BASE_URL")
            .unwrap_or_else(|_| "https://api.muapi.ai/api/v1".into())
            .trim()
            .to_string();
        validate_muapi_url(&muapi_base_url)?;
        let output_dir = env::var("LOCAL_OUTPUT_DIR")
            .unwrap_or_else(|_| "output".into())
            .trim()
            .to_string();
        let output_dir = if output_dir.is_empty() {
            "output".to_string()
        } else {
            output_dir
        };
        let output_dir = expand_home(&output_dir)?;
        let output_dir = if output_dir.is_relative() {
            env::current_dir()?.join(output_dir)
        } else {
            output_dir
        };
        let vad_parameters = env::var("LOCAL_WHISPER_VAD_PARAMETERS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| {
                serde_json::from_str::<Value>(value.trim())
                    .context("LOCAL_WHISPER_VAD_PARAMETERS must be a JSON object")
            })
            .transpose()?
            .unwrap_or_else(|| {
                json!({
                    "threshold": 0.5,
                    "min_speech_duration_ms": 250,
                    "min_silence_duration_ms": 2000,
                    "speech_pad_ms": 400
                })
            });
        if !vad_parameters.is_object() {
            bail!("LOCAL_WHISPER_VAD_PARAMETERS must be a JSON object");
        }

        Ok(Self {
            muapi_api_key: env::var("MUAPI_API_KEY")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            muapi_base_url: muapi_base_url.trim_end_matches('/').to_string(),
            muapi_poll_interval: env_duration("MUAPI_POLL_INTERVAL", 5)?,
            muapi_poll_timeout: env_duration("MUAPI_POLL_TIMEOUT", 600)?,
            llm_provider: LlmProvider::parse(
                &env::var("LLM_PROVIDER").unwrap_or_else(|_| "gemini".into()),
            )?,
            gemini_api_key: env::var("GEMINI_API_KEY")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            gemini_model: env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-4-hydrogen".into()),
            openai_api_key: env::var("OPENAI_API_KEY")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            openai_model: env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into()),
            asr_provider: AsrProvider::parse(
                &env::var("ASR_PROVIDER").unwrap_or_else(|_| "elevenlabs".into()),
            )?,
            elevenlabs_api_key: env::var("ELEVENLABS_API_KEY")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            elevenlabs_scribe_model: env::var("ELEVENLABS_SCRIBE_MODEL")
                .unwrap_or_else(|_| "scribe_v1".into()),
            local_whisper_model: env::var("LOCAL_WHISPER_MODEL").unwrap_or_else(|_| "base".into()),
            local_whisper_device: env::var("LOCAL_WHISPER_DEVICE")
                .unwrap_or_else(|_| "auto".into()),
            local_whisper_vad_filter: env::var("LOCAL_WHISPER_VAD_FILTER")
                .is_ok_and(|value| value.trim().eq_ignore_ascii_case("true")),
            local_whisper_vad_parameters: vad_parameters,
            output_dir,
        })
    }

    pub fn resolve_mode(&self, mode: crate::models::Mode) -> Result<crate::models::Mode> {
        if mode == crate::models::Mode::Auto {
            Ok(if self.muapi_api_key().is_ok() {
                crate::models::Mode::Api
            } else {
                crate::models::Mode::Local
            })
        } else {
            Ok(mode)
        }
    }

    pub fn validate_mode(&self, mode: crate::models::Mode) -> Result<()> {
        let mode = self.resolve_mode(mode)?;
        if mode == crate::models::Mode::Api {
            self.muapi_api_key()?;
            return Ok(());
        }
        match self.llm_provider {
            LlmProvider::Gemini => {
                self.gemini_api_key()?;
            }
            LlmProvider::OpenAi => {
                self.openai_api_key()?;
            }
        }
        if self.asr_provider == AsrProvider::ElevenLabs {
            self.elevenlabs_api_key()?;
        }
        Ok(())
    }

    pub fn muapi_api_key(&self) -> Result<&str> {
        required_api_key(self.muapi_api_key.as_deref(), "MUAPI_API_KEY")
    }

    pub fn gemini_api_key(&self) -> Result<&str> {
        required_api_key(self.gemini_api_key.as_deref(), "GEMINI_API_KEY")
    }

    pub fn openai_api_key(&self) -> Result<&str> {
        let key = required_api_key(self.openai_api_key.as_deref(), "OPENAI_API_KEY")?;
        if !key.starts_with("sk-") {
            bail!("OPENAI_API_KEY must start with sk-");
        }
        Ok(key)
    }

    pub fn elevenlabs_api_key(&self) -> Result<&str> {
        required_api_key(self.elevenlabs_api_key.as_deref(), "ELEVENLABS_API_KEY")
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            muapi_api_key: None,
            muapi_base_url: "https://api.muapi.ai/api/v1".into(),
            muapi_poll_interval: Duration::from_secs(5),
            muapi_poll_timeout: Duration::from_secs(600),
            llm_provider: LlmProvider::Gemini,
            gemini_api_key: None,
            gemini_model: "gemini-4-hydrogen".into(),
            openai_api_key: None,
            openai_model: "gpt-4o-mini".into(),
            asr_provider: AsrProvider::ElevenLabs,
            elevenlabs_api_key: None,
            elevenlabs_scribe_model: "scribe_v1".into(),
            local_whisper_model: "base".into(),
            local_whisper_device: "auto".into(),
            local_whisper_vad_filter: false,
            local_whisper_vad_parameters: json!({
                "threshold": 0.5,
                "min_speech_duration_ms": 250,
                "min_silence_duration_ms": 2000,
                "speech_pad_ms": 400
            }),
            output_dir: PathBuf::from("output"),
        }
    }
}

fn env_duration(name: &str, default: u64) -> Result<Duration> {
    let value = env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_string());
    parse_duration(&value, name)
}

fn parse_duration(value: &str, name: &str) -> Result<Duration> {
    let seconds = value
        .trim()
        .parse::<f64>()
        .with_context(|| format!("{name} must be a non-negative number"))?;
    if !seconds.is_finite() || seconds < 0.0 {
        bail!("{name} must be a non-negative number");
    }
    Duration::try_from_secs_f64(seconds).with_context(|| format!("{name} is too large"))
}

fn expand_home(value: &str) -> Result<PathBuf> {
    if value == "~" || value.starts_with("~/") || value.starts_with("~\\") {
        let home = ["USERPROFILE", "HOME"]
            .iter()
            .find_map(|name| env::var_os(name).filter(|value| !value.is_empty()))
            .context("Could not expand LOCAL_OUTPUT_DIR because the home directory is unknown")?;
        let suffix = &value[1..];
        return Ok(PathBuf::from(home).join(suffix.trim_start_matches(['/', '\\'])));
    }
    Ok(PathBuf::from(value))
}

fn required_api_key<'a>(value: Option<&'a str>, name: &str) -> Result<&'a str> {
    let key = value
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{name} is not configured"))?;
    let placeholder = Regex::new(
        r"^(?:your_[a-z0-9_-]*key_here|xxx+|x{4,}|changeme|change[-_ ]?me|replace[-_ ]?me|placeholder|<[^>]+>)$",
    )
    .expect("API placeholder regex is valid");
    if key.len() < 16 || placeholder.is_match(&key.to_ascii_lowercase()) {
        bail!("{name} is missing or contains a placeholder value");
    }
    Ok(key)
}

fn validate_muapi_url(value: &str) -> Result<()> {
    let parsed = Url::parse(value).context("MUAPI_BASE_URL is invalid")?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("api.muapi.ai")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port_or_known_default() != Some(443)
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!(
            "MUAPI_BASE_URL must be an HTTPS api.muapi.ai URL without credentials, query, fragment, or a custom port"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Mode;

    #[test]
    fn cloud_mode_requires_only_a_muapi_key() {
        let mut config = Config {
            muapi_api_key: Some("muapi-fixture-key-123456".into()),
            ..Default::default()
        };
        assert_eq!(config.resolve_mode(Mode::Auto).unwrap(), Mode::Api);
        assert!(config.validate_mode(Mode::Api).is_ok());
        assert!(config.validate_mode(Mode::Local).is_err());
        config.muapi_api_key = None;
        assert_eq!(config.resolve_mode(Mode::Auto).unwrap(), Mode::Local);
    }

    #[test]
    fn local_mode_validates_the_selected_providers() {
        let mut config = Config {
            gemini_api_key: Some("gemini-fixture-key-123456".into()),
            elevenlabs_api_key: Some("elevenlabs-fixture-key-123456".into()),
            ..Default::default()
        };
        assert!(config.validate_mode(Mode::Local).is_ok());
        config.asr_provider = AsrProvider::Whisper;
        config.elevenlabs_api_key = None;
        assert!(config.validate_mode(Mode::Local).is_ok());
        config.llm_provider = LlmProvider::OpenAi;
        assert!(config.validate_mode(Mode::Local).is_err());
    }

    #[test]
    fn rejects_all_python_placeholder_key_forms() {
        for key in [
            "your_api_key_here",
            "xxxxxxxxxxxxxxxx",
            "change-me",
            "replace_me",
            "placeholder",
            "<real-key>",
        ] {
            assert!(required_api_key(Some(key), "TEST_KEY").is_err(), "{key}");
        }
        assert_eq!(
            required_api_key(Some("  real-fixture-key-123456  "), "TEST_KEY").unwrap(),
            "real-fixture-key-123456"
        );
    }

    #[cfg(feature = "gui")]
    #[test]
    fn whisper_model_sizes_round_trip_to_supported_model_names() {
        for (size, name) in [
            (WhisperModelSize::Tiny, "tiny"),
            (WhisperModelSize::Base, "base"),
            (WhisperModelSize::Small, "small"),
            (WhisperModelSize::Medium, "medium"),
            (WhisperModelSize::Large, "large-v3"),
        ] {
            assert_eq!(size.name(), name);
            assert_eq!(WhisperModelSize::parse(name), Some(size));
        }
        assert_eq!(
            WhisperModelSize::parse("large"),
            Some(WhisperModelSize::Large)
        );
        assert_eq!(WhisperModelSize::parse("unsupported"), None);
    }

    #[test]
    fn poll_durations_accept_fractional_seconds_and_reject_invalid_values() {
        assert_eq!(
            parse_duration("0.5", "POLL").unwrap(),
            Duration::from_millis(500)
        );
        assert_eq!(parse_duration("0", "POLL").unwrap(), Duration::ZERO);
        for value in ["-1", "NaN", "inf", "not-a-number"] {
            assert!(parse_duration(value, "POLL").is_err(), "{value}");
        }
    }
}
