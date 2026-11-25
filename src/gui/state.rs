//! GUI state, validation, and the generation worker bridge.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use rune::{FileDialogOptions, Player, PlayerState, TextEdit, Tx, Waker};
use url::Url;

#[cfg(test)]
use crate::credentials::MemoryCredentialStore;
use crate::{
    captions::CaptionPreset,
    captions::plan_captions,
    clip::ReframeMode,
    clip::{RenderRequest, dimensions, render_short},
    config::{AsrProvider, Config, LlmProvider, WhisperModelSize},
    credentials::{
        ApiKey, CredentialKey, CredentialStore, StoredCredentials, WindowsCredentialStore,
    },
    download::{VIDEO_EXTENSIONS, validate_youtube_url},
    edit::{format_seconds, parse_trim},
    models::{GenerationResult, Highlight, Mode},
    pipeline::{GenerateOptions, generate_with},
    progress::ProgressEvent,
    projects::ProjectSummary,
};

const LOG_LIMIT: usize = 300;
const MESSAGE_LIMIT: usize = 4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Aspect {
    Portrait,
    Square,
    Landscape,
}

impl Aspect {
    pub(super) const ALL: [Self; 3] = [Self::Portrait, Self::Square, Self::Landscape];

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Portrait => "9:16",
            Self::Square => "1:1",
            Self::Landscape => "16:9",
        }
    }

    pub(super) const fn value(self) -> &'static str {
        self.label()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Panel {
    Results,
    Activity,
}

#[derive(Debug, Clone)]
struct ResolvedSource {
    original: String,
    path: PathBuf,
}

struct PreviewEvent {
    url: String,
    result: Result<PathBuf, String>,
}

struct EditOutcome {
    index: usize,
    result: Result<EditedClip, String>,
}

struct EditedClip {
    highlight: Highlight,
    path: PathBuf,
    note: Option<String>,
}

pub(super) struct GuiState {
    pub(super) source: TextEdit,
    pub(super) num_clips: TextEdit,
    pub(super) target_duration: TextEdit,
    pub(super) language: TextEdit,
    pub(super) prompt: TextEdit,
    pub(super) gemini_key: TextEdit,
    pub(super) elevenlabs_key: TextEdit,
    pub(super) openai_key: TextEdit,
    pub(super) muapi_key: TextEdit,
    pub(super) key_removals: [bool; 4],
    pub(super) mode: Mode,
    pub(super) aspect: Aspect,
    pub(super) format: u32,
    pub(super) analysis_format: u32,
    pub(super) burn_captions: bool,
    pub(super) caption_preset: CaptionPreset,
    pub(super) reframe: ReframeMode,
    pub(super) text_hook: bool,
    pub(super) recent_projects: Vec<ProjectSummary>,
    persist_projects: bool,
    pub(super) asr_provider: AsrProvider,
    pub(super) whisper_model: WhisperModelSize,
    pub(super) advanced: bool,
    pub(super) settings_open: bool,
    pub(super) onboarding: bool,
    pub(super) panel: Panel,
    pub(super) running: bool,
    pub(super) stage: String,
    pub(super) progress: f32,
    pub(super) logs: VecDeque<String>,
    pub(super) result: Option<GenerationResult>,
    pub(super) error: Option<String>,
    pub(super) notice: Option<String>,
    pub(super) config: Config,
    pub(super) config_error: Option<String>,
    pub(super) credentials: Box<dyn CredentialStore>,
    pub(super) receiver: Option<Receiver<ProgressEvent>>,
    preview_receiver: Receiver<PreviewEvent>,
    preview_sender: Sender<PreviewEvent>,
    preview_cache: HashMap<String, PathBuf>,
    preview_in_flight: HashSet<String>,
    preview_errors: HashMap<String, String>,
    remote_source: Option<String>,
    media_waker: Option<Waker>,
    resolved_source: Option<ResolvedSource>,
    pub(super) source_player: Option<Player>,
    pub(super) source_preview_error: Option<String>,
    pub(super) selected_short: Option<usize>,
    pub(super) edit_start: TextEdit,
    pub(super) edit_end: TextEdit,
    pub(super) edit_hook: TextEdit,
    pub(super) music_style: TextEdit,
    pub(super) exporting_clip: Option<usize>,
    edit_revision: u32,
    edit_receiver: Option<Receiver<EditOutcome>>,
    pub(super) short_player: Option<Player>,
    pub(super) short_preview_error: Option<String>,
    media_states: [Option<PlayerState>; 2],
}

impl GuiState {
    pub(super) fn new() -> Self {
        let credentials: Box<dyn CredentialStore> = Box::new(WindowsCredentialStore::new());
        let stored = StoredCredentials::load(credentials.as_ref());
        let onboarding = stored
            .as_ref()
            .map_or(true, |stored| !stored.onboarding_complete);
        let (mut config, config_error) = match Config::from_env() {
            Ok(config) => (config, None),
            Err(error) => (
                Config::default(),
                Some(clean_message(&format!("{error:#}"))),
            ),
        };
        if let Ok(stored) = &stored {
            stored.apply_to(&mut config);
        }
        let mut state = Self::with_store(config, config_error, credentials, onboarding);
        state.persist_projects = true;
        state.refresh_recent_projects();
        if let Err(error) = stored {
            state.set_error(format!("Could not load saved settings: {error:#}"));
        }
        state
    }

    #[cfg(test)]
    pub(super) fn with_config(config: Config, config_error: Option<String>) -> Self {
        Self::with_store(
            config,
            config_error,
            Box::new(MemoryCredentialStore::default()),
            false,
        )
    }

    pub(super) fn with_store(
        config: Config,
        config_error: Option<String>,
        credentials: Box<dyn CredentialStore>,
        onboarding: bool,
    ) -> Self {
        let (preview_sender, preview_receiver) = mpsc::channel();
        let asr_provider = config.asr_provider;
        let whisper_model =
            WhisperModelSize::parse(&config.local_whisper_model).unwrap_or(WhisperModelSize::Base);
        let mut state = Self {
            source: TextEdit::new(""),
            num_clips: TextEdit::new("3"),
            target_duration: TextEdit::new("30"),
            language: TextEdit::new(""),
            prompt: TextEdit::new(""),
            gemini_key: TextEdit::new("").secure(),
            elevenlabs_key: TextEdit::new("").secure(),
            openai_key: TextEdit::new("").secure(),
            muapi_key: TextEdit::new("").secure(),
            key_removals: [false; 4],
            mode: Mode::Local,
            aspect: Aspect::Portrait,
            format: 1080,
            analysis_format: 360,
            burn_captions: true,
            caption_preset: CaptionPreset::Bold,
            reframe: ReframeMode::Center,
            text_hook: false,
            recent_projects: Vec::new(),
            persist_projects: false,
            asr_provider,
            whisper_model,
            advanced: false,
            settings_open: false,
            onboarding,
            panel: Panel::Results,
            running: false,
            stage: "Ready to generate".into(),
            progress: 0.0,
            logs: VecDeque::new(),
            result: None,
            error: None,
            notice: None,
            config,
            config_error,
            credentials,
            receiver: None,
            preview_receiver,
            preview_sender,
            preview_cache: HashMap::new(),
            preview_in_flight: HashSet::new(),
            preview_errors: HashMap::new(),
            remote_source: None,
            media_waker: None,
            resolved_source: None,
            source_player: None,
            source_preview_error: None,
            selected_short: None,
            edit_start: TextEdit::new(""),
            edit_end: TextEdit::new(""),
            edit_hook: TextEdit::new(""),
            music_style: TextEdit::new(""),
            exporting_clip: None,
            edit_revision: 0,
            edit_receiver: None,
            short_player: None,
            short_preview_error: None,
            media_states: [None, None],
        };
        if onboarding {
            let first_key = state.first_required_api_key();
            state.api_key_input_mut(first_key).focused = true;
        } else {
            state.source.focused = true;
        }
        state
    }

    pub(super) fn configuration_message(&self) -> Option<String> {
        self.config_error.clone().or_else(|| {
            self.config
                .validate_mode(self.mode)
                .err()
                .map(|error| clean_message(&format!("{error:#}")))
        })
    }

    pub(super) fn output_dir(&self) -> &Path {
        &self.config.output_dir
    }

    pub(super) fn api_key_input(&self, key: ApiKey) -> &TextEdit {
        match key {
            ApiKey::Gemini => &self.gemini_key,
            ApiKey::ElevenLabs => &self.elevenlabs_key,
            ApiKey::OpenAi => &self.openai_key,
            ApiKey::MuApi => &self.muapi_key,
        }
    }

    pub(super) fn api_key_input_mut(&mut self, key: ApiKey) -> &mut TextEdit {
        match key {
            ApiKey::Gemini => &mut self.gemini_key,
            ApiKey::ElevenLabs => &mut self.elevenlabs_key,
            ApiKey::OpenAi => &mut self.openai_key,
            ApiKey::MuApi => &mut self.muapi_key,
        }
    }

    pub(super) fn api_key_is_configured(&self, key: ApiKey) -> bool {
        key.get(&self.config).is_some()
    }

    pub(super) fn api_key_is_removed(&self, key: ApiKey) -> bool {
        self.key_removals[key.index()]
    }

    pub(super) fn api_key_is_required(&self, key: ApiKey) -> bool {
        let mode = if self.onboarding {
            self.onboarding_mode()
        } else {
            self.config.resolve_mode(self.mode).unwrap_or(self.mode)
        };
        if mode == Mode::Api {
            return key == ApiKey::MuApi;
        }
        match key {
            ApiKey::Gemini => self.config.llm_provider == LlmProvider::Gemini,
            ApiKey::OpenAi => self.config.llm_provider == LlmProvider::OpenAi,
            ApiKey::ElevenLabs => self.asr_provider == AsrProvider::ElevenLabs,
            ApiKey::MuApi => false,
        }
    }

    pub(super) fn onboarding_mode(&self) -> Mode {
        self.config_with_api_key_drafts()
            .map(|config| {
                if config.validate_mode(Mode::Local).is_ok() {
                    Mode::Local
                } else if config.validate_mode(Mode::Api).is_ok() {
                    Mode::Api
                } else {
                    self.mode
                }
            })
            .unwrap_or(self.mode)
    }

    pub(super) fn first_required_api_key(&self) -> ApiKey {
        ApiKey::ALL
            .into_iter()
            .find(|key| self.api_key_is_required(*key))
            .unwrap_or(ApiKey::Gemini)
    }

    pub(super) fn onboarding_is_ready(&self) -> bool {
        self.config_with_api_key_drafts().is_ok_and(|config| {
            config.validate_mode(Mode::Local).is_ok() || config.validate_mode(Mode::Api).is_ok()
        })
    }

    pub(super) fn has_api_key_draft(&self, key: ApiKey) -> bool {
        nonempty(&self.api_key_input(key).text).is_some()
    }

    pub(super) fn api_key_is_invalid(&self, key: ApiKey) -> bool {
        let mut next = self.config.clone();
        key.set(&mut next, self.effective_api_key(key));
        key.get(&next).is_some_and(|_| key.validate(&next).is_err())
    }

    pub(super) fn invalid_api_key(&self) -> Option<ApiKey> {
        ApiKey::ALL
            .into_iter()
            .find(|key| self.api_key_is_invalid(*key))
    }

    pub(super) fn remove_api_key_draft(&mut self, key: ApiKey) {
        if self.running {
            return;
        }
        self.api_key_input_mut(key).replace_text(String::new());
        self.key_removals[key.index()] = true;
        self.clear_feedback();
    }

    pub(super) fn open_settings(&mut self) {
        if self.running || self.onboarding {
            return;
        }
        self.settings_open = true;
        self.clear_api_key_drafts();
        self.asr_provider = self.config.asr_provider;
        self.whisper_model = WhisperModelSize::parse(&self.config.local_whisper_model)
            .unwrap_or(WhisperModelSize::Base);
        let first_key = self.first_required_api_key();
        self.api_key_input_mut(first_key).focused = true;
        self.clear_feedback();
    }

    pub(super) fn close_settings(&mut self) {
        if self.running {
            return;
        }
        self.settings_open = false;
        self.clear_api_key_drafts();
        self.asr_provider = self.config.asr_provider;
        self.whisper_model = WhisperModelSize::parse(&self.config.local_whisper_model)
            .unwrap_or(WhisperModelSize::Base);
        self.clear_feedback();
    }

    pub(super) fn save_settings(&mut self) {
        self.save_api_keys(false);
    }

    pub(super) fn finish_onboarding(&mut self) {
        if self.onboarding {
            self.save_api_keys(true);
        }
    }

    fn save_api_keys(&mut self, require_ready: bool) {
        if self.running {
            return;
        }
        let next = match self.config_with_api_key_drafts() {
            Ok(config) => config,
            Err(error) => {
                self.set_error(format!("{error:#}"));
                return;
            }
        };
        let next_mode = if require_ready {
            if next.validate_mode(Mode::Local).is_ok() {
                Mode::Local
            } else if next.validate_mode(Mode::Api).is_ok() {
                Mode::Api
            } else {
                self.set_error(
                    "Add the required local provider keys or a MuAPI key for cloud generation.",
                );
                return;
            }
        } else {
            self.mode
        };

        for key in ApiKey::ALL {
            let value = key.get(&next).unwrap_or("");
            if let Err(error) = self.credentials.write(CredentialKey::Api(key), value) {
                self.set_error(format!("{error:#}"));
                return;
            }
        }
        if let Err(error) = self
            .credentials
            .write(CredentialKey::AsrProvider, self.asr_provider.name())
        {
            self.set_error(format!("{error:#}"));
            return;
        }
        if let Err(error) = self
            .credentials
            .write(CredentialKey::LocalWhisperModel, self.whisper_model.name())
        {
            self.set_error(format!("{error:#}"));
            return;
        }
        if let Err(error) = self
            .credentials
            .write(CredentialKey::OnboardingComplete, "complete")
        {
            self.set_error(format!("{error:#}"));
            return;
        }

        self.config = next;
        self.mode = next_mode;
        self.onboarding = false;
        self.settings_open = false;
        self.clear_api_key_drafts();
        self.source.focused = true;
        self.notify(if require_ready {
            "Settings saved. You're ready to generate shorts."
        } else {
            "Transcription and API key settings saved."
        });
    }

    fn config_with_api_key_drafts(&self) -> Result<Config> {
        let mut next = self.config.clone();
        next.asr_provider = self.asr_provider;
        next.local_whisper_model = self.whisper_model.name().into();
        for key in ApiKey::ALL {
            key.set(&mut next, self.effective_api_key(key));
            if self.has_api_key_draft(key) {
                key.validate(&next)?;
            }
        }
        Ok(next)
    }

    fn effective_api_key(&self, key: ApiKey) -> Option<String> {
        if let Some(draft) = nonempty(&self.api_key_input(key).text) {
            return Some(draft);
        }
        if self.api_key_is_removed(key) {
            return None;
        }
        key.get(&self.config).map(str::to_owned)
    }

    fn clear_api_key_drafts(&mut self) {
        for edit in [
            &mut self.gemini_key,
            &mut self.elevenlabs_key,
            &mut self.openai_key,
            &mut self.muapi_key,
        ] {
            edit.replace_text(String::new());
            edit.focused = false;
        }
        self.key_removals = [false; 4];
    }

    pub(super) fn set_source(&mut self, path: PathBuf) {
        self.source
            .replace_text(path.to_string_lossy().into_owned());
        self.resolved_source = None;
        self.remote_source = None;
        self.sync_source_preview();
        self.clear_feedback();
    }

    pub(super) fn setup(state: &mut Self, waker: Waker) {
        state.media_waker = Some(waker.clone());
        state.sync_source_preview();
        state.sync_short_preview();
        for player in [&state.source_player, &state.short_player]
            .into_iter()
            .flatten()
        {
            player.set_waker(waker.clone());
        }
    }

    pub(super) fn source_preview_message(&self) -> Option<String> {
        if self.source_player.is_some() {
            return None;
        }
        if let Some(error) = &self.source_preview_error {
            return Some(error.clone());
        }
        if let Some(remote) = &self.remote_source {
            if self.preview_in_flight.contains(remote) {
                return Some("Downloading cloud source for preview…".into());
            }
            if let Some(error) = self.preview_errors.get(remote) {
                return Some(format!("Cloud source preview failed: {error}"));
            }
            return Some(
                "Download a local copy to preview the cloud source here, or open it online.".into(),
            );
        }
        let source = self.source.text.trim();
        if source.is_empty() {
            return Some("Choose a local video to preview it here.".into());
        }
        if validate_youtube_url(source).is_ok() {
            return Some(if self.running {
                "Resolving the YouTube source for preview…".into()
            } else {
                "YouTube preview appears after generation resolves the source.".into()
            });
        }
        Some("Preview supports local MP4, MOV, MKV, and WebM files.".into())
    }

    pub(super) fn remote_source_url(&self) -> Option<String> {
        self.remote_source.clone()
    }

    pub(super) fn source_preview_is_loading(&self) -> bool {
        self.remote_source
            .as_ref()
            .is_some_and(|url| self.preview_in_flight.contains(url))
    }

    pub(super) fn download_source_preview(&mut self) {
        let Some(url) = self.remote_source.clone() else {
            return;
        };
        self.preview_errors.remove(&url);
        self.ensure_remote_preview(&url);
        self.source_preview_error = None;
        self.sync_source_preview();
    }

    pub(super) fn open_remote_source(&mut self) {
        let Some(url) = self.remote_source.clone() else {
            return;
        };
        match open_remote_url(&url) {
            Ok(()) => self.notify("Opened the cloud source."),
            Err(error) => self.set_error(error),
        }
    }

    pub(super) fn short_preview_is_loading(&self) -> bool {
        self.selected_short_url()
            .is_some_and(|url| self.preview_in_flight.contains(url))
    }

    fn selected_short_url(&self) -> Option<&str> {
        self.result
            .as_ref()?
            .shorts
            .get(self.selected_short?)?
            .clip_url
            .as_deref()
    }

    pub(super) fn select_short(&mut self, index: usize) {
        let Some(result) = &self.result else {
            return;
        };
        let Some(short) = result.shorts.get(index) else {
            return;
        };
        if short.error.is_some() || short.clip_url.is_none() {
            return;
        }
        if let Some(player) = &mut self.short_player {
            player.pause();
        }
        let switching = self.selected_short != Some(index);
        self.selected_short = Some(index);
        if switching {
            self.fill_trim_fields();
        }
        self.short_preview_error = None;
        if let Some(url) = self.selected_short_url().map(str::to_owned) {
            self.preview_errors.remove(&url);
        }
        self.sync_short_preview();
    }

    pub(super) fn toggle_short_preview(&mut self, index: usize) {
        if self.selected_short == Some(index) {
            if let Some(player) = &mut self.short_player {
                player.pause();
            }
            self.selected_short = None;
            self.short_player = None;
            self.short_preview_error = None;
            return;
        }
        self.select_short(index);
    }

    fn requested_source_path(&mut self) -> Result<Option<PathBuf>, String> {
        let source = self.source.text.trim();
        if source.is_empty() {
            return Ok(None);
        }
        if let Some(resolved) = &self.resolved_source
            && resolved.original == source
        {
            return preview_file(&resolved.path);
        }
        if source.starts_with("file://") {
            let url = Url::parse(source).map_err(|_| "Invalid local file URL.".to_string())?;
            let path = url
                .to_file_path()
                .map_err(|_| "Invalid local file URL.".to_string())?;
            return preview_file(&path);
        }
        if source.contains("://") {
            return if validate_youtube_url(source).is_ok() {
                if let Some(path) = self
                    .remote_source
                    .as_ref()
                    .and_then(|url| self.preview_cache.get(url))
                {
                    preview_file(path)
                } else {
                    Ok(None)
                }
            } else {
                Err("Remote previews are available only for supported YouTube URLs.".into())
            };
        }
        preview_file(Path::new(source))
    }

    fn sync_source_preview(&mut self) -> bool {
        let previous_error = self.source_preview_error.clone();
        let requested = self.requested_source_path();
        let same_player = requested.as_ref().is_ok_and(|requested| {
            requested.as_ref().is_some_and(|path| {
                self.source_player.as_ref().is_some_and(|player| {
                    matches!(player.source(), rune::MediaSource::File(current) if current == path)
                })
            })
        });
        let mut changed = false;
        match requested {
            Ok(Some(path)) => {
                if self.source_preview_error.is_some() {
                    self.source_preview_error = None;
                    changed = true;
                }
                if !same_player {
                    if let Some(player) = &mut self.source_player {
                        player.pause();
                    }
                    self.source_player = Some(self.make_player(&path));
                    changed = true;
                }
            }
            Ok(None) => {
                if self.source_player.take().is_some() || self.source_preview_error.take().is_some()
                {
                    changed = true;
                }
            }
            Err(message) => {
                if self.source_player.take().is_some() {
                    changed = true;
                }
                if self.source_preview_error.as_deref() != Some(&message) {
                    self.source_preview_error = Some(message);
                    changed = true;
                }
            }
        }
        changed || previous_error != self.source_preview_error
    }

    fn requested_short_path(&mut self) -> Result<Option<PathBuf>, String> {
        let Some(index) = self.selected_short else {
            return Ok(None);
        };
        let Some(path) = self
            .result
            .as_ref()
            .and_then(|result| result.shorts.get(index))
            .and_then(|short| short.clip_url.clone())
        else {
            return Ok(None);
        };
        if path.contains("://") {
            validate_https_media_url(&path)?;
            if let Some(cached) = self.preview_cache.get(&path) {
                return preview_file(cached);
            }
            if let Some(error) = self.preview_errors.get(&path) {
                return Err(error.clone());
            }
            self.ensure_remote_preview(&path);
            return Ok(None);
        }
        preview_file(Path::new(&path))
    }

    fn sync_short_preview(&mut self) -> bool {
        let previous_error = self.short_preview_error.clone();
        let requested = self.requested_short_path();
        let same_player = requested.as_ref().is_ok_and(|requested| {
            requested.as_ref().is_some_and(|path| {
                self.short_player.as_ref().is_some_and(|player| {
                    matches!(player.source(), rune::MediaSource::File(current) if current == path)
                })
            })
        });
        let mut changed = false;
        match requested {
            Ok(Some(path)) => {
                if self.short_preview_error.is_some() {
                    self.short_preview_error = None;
                    changed = true;
                }
                if !same_player {
                    if let Some(player) = &mut self.short_player {
                        player.pause();
                    }
                    self.short_player = Some(self.make_player(&path));
                    changed = true;
                }
            }
            Ok(None) => {
                if self.short_player.take().is_some() || self.short_preview_error.take().is_some() {
                    changed = true;
                }
            }
            Err(message) => {
                if self.short_player.take().is_some() {
                    changed = true;
                }
                if self.short_preview_error.as_deref() != Some(&message) {
                    self.short_preview_error = Some(message);
                    changed = true;
                }
            }
        }
        changed || previous_error != self.short_preview_error
    }

    fn sync_previews(&mut self) -> bool {
        let source_changed = self.sync_source_preview();
        let short_changed = self.sync_short_preview();
        source_changed || short_changed
    }

    fn ensure_remote_preview(&mut self, url: &str) {
        if self.preview_cache.contains_key(url)
            || self.preview_errors.contains_key(url)
            || self.preview_in_flight.contains(url)
        {
            return;
        }
        let destination = match preview_cache_path(&self.config.output_dir, url) {
            Ok(path) => path,
            Err(error) => {
                self.preview_errors
                    .insert(url.to_owned(), clean_message(&error));
                return;
            }
        };
        if destination
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
        {
            self.preview_cache.insert(url.to_owned(), destination);
            return;
        }
        self.preview_in_flight.insert(url.to_owned());
        let download_url = url.to_owned();
        let sender = self.preview_sender.clone();
        let worker = std::thread::Builder::new()
            .name("shorts-preview-download".into())
            .spawn(move || {
                let result = crate::clip::download_https(&download_url, &destination, &mut |_| {})
                    .map_err(|error| clean_message(&format!("{error:#}")));
                let _ = sender.send(PreviewEvent {
                    url: download_url,
                    result,
                });
            });
        if let Err(error) = worker {
            self.preview_in_flight.remove(url);
            self.preview_errors.insert(
                url.to_owned(),
                clean_message(&format!("Could not start the preview download: {error}")),
            );
        }
    }

    fn drain_preview_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.preview_receiver.try_recv() {
            changed = true;
            self.preview_in_flight.remove(&event.url);
            match event.result {
                Ok(path) => {
                    self.preview_errors.remove(&event.url);
                    self.preview_cache.insert(event.url, path);
                }
                Err(error) => {
                    self.preview_errors.insert(event.url, error);
                }
            }
        }
        changed
    }

    fn make_player(&self, path: &Path) -> Player {
        let player = Player::file(path);
        player.set_looping(false);
        if let Some(waker) = &self.media_waker {
            player.set_waker(waker.clone());
        }
        player.preload();
        player
    }

    fn media_needs_tick(&self) -> bool {
        [&self.source_player, &self.short_player]
            .into_iter()
            .flatten()
            .any(|player| matches!(player.state(), PlayerState::Loading | PlayerState::Playing))
    }

    pub(super) fn choose_source(&mut self, tx: &mut Tx) {
        if self.running {
            return;
        }
        let options = FileDialogOptions {
            title: "Choose a source video".into(),
            ..FileDialogOptions::default()
        }
        .filter("Video files", VIDEO_EXTENSIONS)
        .filter("All files", &["*"]);
        tx.open_file(options, |state: &mut GuiState, result, _| match result {
            Ok(Some(path)) if !state.running => state.set_source(path),
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => state.set_error(format!("Could not choose a source video: {error}")),
        });
    }

    pub(super) fn choose_output_folder(&mut self, tx: &mut Tx) {
        if self.running {
            return;
        }
        let directory = self.output_dir().to_path_buf();
        let options = FileDialogOptions {
            title: "Choose an output folder".into(),
            directory: Some(directory),
            ..FileDialogOptions::default()
        };
        tx.open_folder(options, |state: &mut GuiState, result, _| match result {
            Ok(Some(path)) if !state.running => {
                state.config.output_dir = path;
                if state.persist_projects {
                    state.refresh_recent_projects();
                }
                state.clear_feedback();
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => state.set_error(format!("Could not choose an output folder: {error}")),
        });
    }

    pub(super) fn set_mode(&mut self, mode: Mode) {
        if !self.running {
            self.mode = mode;
            self.clear_feedback();
        }
    }

    pub(super) fn set_asr_provider(&mut self, provider: AsrProvider) {
        if !self.running {
            self.asr_provider = provider;
            self.clear_feedback();
        }
    }

    pub(super) fn set_whisper_model(&mut self, model: WhisperModelSize) {
        if !self.running {
            self.whisper_model = model;
            self.clear_feedback();
        }
    }

    pub(super) fn set_aspect(&mut self, aspect: Aspect) {
        if !self.running {
            self.aspect = aspect;
            self.clear_feedback();
        }
    }

    pub(super) fn set_format(&mut self, format: u32) {
        if !self.running {
            self.format = format;
            self.clear_feedback();
        }
    }

    pub(super) fn set_analysis_format(&mut self, format: u32) {
        if !self.running {
            self.analysis_format = format;
            self.clear_feedback();
        }
    }

    pub(super) fn set_burn_captions(&mut self, enabled: bool) {
        if !self.running {
            self.burn_captions = enabled;
            self.clear_feedback();
        }
    }

    pub(super) fn set_caption_preset(&mut self, preset: CaptionPreset) {
        if !self.running {
            self.caption_preset = preset;
            self.clear_feedback();
        }
    }

    pub(super) fn set_reframe(&mut self, reframe: ReframeMode) {
        if !self.running {
            self.reframe = reframe;
            self.clear_feedback();
        }
    }

    pub(super) fn set_text_hook(&mut self, enabled: bool) {
        if !self.running {
            self.text_hook = enabled;
            self.clear_feedback();
        }
    }

    pub(super) fn set_duration_preset(&mut self, index: usize) {
        if self.running {
            return;
        }
        if let Some(seconds) = ["25", "45", "75"].get(index) {
            self.target_duration.replace_text((*seconds).to_owned());
            self.clear_feedback();
        }
    }

    pub(super) fn duration_preset_index(&self) -> Option<usize> {
        match self.target_duration.text.trim() {
            "25" => Some(0),
            "45" => Some(1),
            "75" => Some(2),
            _ => None,
        }
    }

    pub(super) fn open_recent_project(&mut self, index: usize) {
        if self.running {
            return;
        }
        let Some(project) = self.recent_projects.get(index).cloned() else {
            return;
        };
        self.clear_feedback();
        self.source.replace_text(project.source);
        self.resolved_source = None;
        self.remote_source = remote_media_url(&project.result.source_video_url);
        self.result = Some(project.result);
        self.progress = 1.0;
        self.stage = "Opened saved project".into();
        self.panel = Panel::Results;
        self.selected_short = self.result.as_ref().and_then(|result| {
            result
                .shorts
                .iter()
                .position(|short| short.error.is_none() && short.clip_url.is_some())
        });
        self.short_preview_error = None;
        self.source_preview_error = None;
        self.fill_trim_fields();
        self.sync_source_preview();
        self.sync_short_preview();
        self.notify(
            "Opened a saved project. Rendered files must still be in their original location.",
        );
    }

    fn refresh_recent_projects(&mut self) {
        match crate::projects::list_projects(self.output_dir()) {
            Ok(projects) => self.recent_projects = projects,
            Err(error) => {
                self.recent_projects.clear();
                self.push_log(format!("Could not read saved projects: {error:#}"));
            }
        }
    }

    pub(super) fn toggle_advanced(&mut self) {
        if !self.running {
            self.advanced = !self.advanced;
        }
    }

    pub(super) fn set_panel(&mut self, panel: Panel) {
        self.panel = panel;
    }

    pub(super) fn clear_feedback(&mut self) {
        self.error = None;
        self.notice = None;
    }

    pub(super) fn set_error(&mut self, message: impl Into<String>) {
        let message = clean_message(&message.into());
        self.error = Some(message.clone());
        self.notice = None;
        self.push_log(format!("Error: {message}"));
    }

    pub(super) fn notify(&mut self, message: impl Into<String>) {
        let message = clean_message(&message.into());
        self.notice = Some(message.clone());
        self.push_log(message);
    }

    pub(super) fn start_generation(&mut self) {
        if self.running || self.onboarding || self.settings_open || self.exporting_clip.is_some() {
            return;
        }
        self.clear_feedback();
        let options = match self.generation_options() {
            Ok(options) => options,
            Err(error) => {
                self.set_error(format!("{error:#}"));
                return;
            }
        };
        if let Some(error) = &self.config_error {
            self.set_error(error.clone());
            return;
        }
        if let Err(error) = self.config.validate_mode(self.mode) {
            self.set_error(format!("{error:#}"));
            return;
        }

        self.result = None;
        self.resolved_source = None;
        self.remote_source = None;
        self.selected_short = None;
        self.short_preview_error = None;
        if let Some(player) = &mut self.short_player {
            player.pause();
        }
        self.short_player = None;
        self.logs.clear();
        self.lock_inputs();
        self.running = true;
        self.stage = "Starting generation".into();
        self.progress = 0.0;
        self.panel = Panel::Activity;
        self.push_log(format!(
            "Starting {} generation: {} clip{}, target {} seconds.",
            match self.mode {
                Mode::Local => "local",
                Mode::Api => "MuAPI cloud",
                Mode::Auto => "automatic",
            },
            options.num_clips,
            if options.num_clips == 1 { "" } else { "s" },
            options.target_duration
        ));

        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        let config = self.config.clone();
        let worker = std::thread::Builder::new()
            .name("shorts-generation".into())
            .spawn(move || {
                let outcome = generate_with(options, &config, |event| {
                    let _ = sender.send(event);
                });
                let final_event = match outcome {
                    Ok(result) => ProgressEvent::Finished(result),
                    Err(error) => ProgressEvent::Failed(format!("{error:#}")),
                };
                let _ = sender.send(final_event);
            });
        if let Err(error) = worker {
            self.running = false;
            self.receiver = None;
            self.stage = "Generation failed".into();
            self.set_error(format!("Could not start the generation worker: {error}"));
        }
    }

    fn lock_inputs(&mut self) {
        for edit in [
            &mut self.source,
            &mut self.num_clips,
            &mut self.target_duration,
            &mut self.language,
            &mut self.prompt,
            &mut self.edit_start,
            &mut self.edit_end,
            &mut self.edit_hook,
            &mut self.gemini_key,
            &mut self.elevenlabs_key,
            &mut self.openai_key,
            &mut self.muapi_key,
        ] {
            edit.focused = false;
        }
    }

    fn generation_options(&self) -> Result<GenerateOptions> {
        let source = self.source.text.trim().to_owned();
        if source.is_empty() {
            bail!("Choose a source video or paste an HTTPS YouTube URL first.");
        }
        let num_clips = self
            .num_clips
            .text
            .trim()
            .parse::<usize>()
            .context("Clip count must be a whole number.")?;
        if !(1..=30).contains(&num_clips) {
            bail!("Clip count must be between 1 and 30.");
        }
        let target_duration = self
            .target_duration
            .text
            .trim()
            .parse::<f64>()
            .context("Target duration must be a number.")?;
        if !target_duration.is_finite() || !(5.0..=600.0).contains(&target_duration) {
            bail!("Target duration must be between 5 and 600 seconds.");
        }
        let language = nonempty(&self.language.text);
        let prompt = nonempty(&self.prompt.text);
        Ok(GenerateOptions {
            source,
            mode: self.mode,
            num_clips,
            target_duration,
            aspect_ratio: self.aspect.value().into(),
            format: self.format,
            analysis_format: self.analysis_format,
            language,
            prompt,
            output_json: None,
            burn_captions: self.burn_captions,
            caption_preset: self.caption_preset,
            reframe: self.reframe,
            text_hook: self.text_hook,
        })
    }

    pub(super) fn drain_events(&mut self) -> bool {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(receiver) = &self.receiver {
            for _ in 0..512 {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        let had_events = !events.is_empty();
        for event in events {
            self.apply_progress(event);
        }
        if disconnected && self.running {
            self.running = false;
            self.stage = "Generation failed".into();
            self.panel = Panel::Activity;
            self.set_error("The generation worker stopped unexpectedly.");
        }
        if !self.running {
            self.receiver = None;
        }
        disconnected || had_events
    }

    pub(super) fn apply_progress(&mut self, event: ProgressEvent) {
        match event {
            ProgressEvent::Log(message) => self.push_log(message),
            ProgressEvent::SourceReady(path) => {
                self.resolved_source = Some(ResolvedSource {
                    original: self.source.text.trim().to_owned(),
                    path,
                });
                self.source_preview_error = None;
                self.sync_source_preview();
            }
            ProgressEvent::Stage { label, fraction } => {
                self.stage = clean_message(&label);
                self.progress = fraction.clamp(0.0, 1.0);
            }
            ProgressEvent::Finished(result) => {
                self.running = false;
                self.progress = 1.0;
                self.stage = "Complete".into();
                self.panel = Panel::Results;
                let summary = result_summary(&result);
                self.push_log(summary.clone());
                self.notice = Some(summary);
                if self.persist_projects {
                    match crate::projects::save_project(
                        self.output_dir(),
                        self.source.text.trim(),
                        &result,
                    ) {
                        Ok(path) => {
                            self.refresh_recent_projects();
                            self.push_log(format!("Saved project {}", path.display()));
                        }
                        Err(error) => {
                            self.push_log(format!("Could not save the project: {error:#}"));
                        }
                    }
                }
                self.remote_source = remote_media_url(&result.source_video_url);
                self.result = Some(result);
                self.source_preview_error = None;
                self.sync_source_preview();
                self.selected_short = self.result.as_ref().and_then(|result| {
                    result
                        .shorts
                        .iter()
                        .position(|short| short.error.is_none() && short.clip_url.is_some())
                });
                self.short_preview_error = None;
                self.fill_trim_fields();
                self.sync_short_preview();
            }
            ProgressEvent::Failed(message) => {
                self.running = false;
                self.stage = "Generation failed".into();
                self.panel = Panel::Activity;
                self.set_error(message);
            }
        }
    }

    fn push_log(&mut self, message: impl Into<String>) {
        let message = clean_message(&message.into());
        if message.is_empty() {
            return;
        }
        self.logs.push_back(message);
        while self.logs.len() > LOG_LIMIT {
            self.logs.pop_front();
        }
    }

    pub(super) fn status_detail(&self) -> String {
        if let Some(notice) = &self.notice {
            return notice.clone();
        }
        if self.running {
            return "Generation runs in the background. You can follow each stage in Activity."
                .into();
        }
        self.result.as_ref().map(result_summary).unwrap_or_else(|| {
            "Choose a source, adjust the settings, and generate your shorts.".into()
        })
    }

    pub(super) fn open_output_folder(&mut self) {
        let path = self.output_dir().to_path_buf();
        if let Err(error) = fs::create_dir_all(&path) {
            self.set_error(format!("Could not create the output folder: {error}"));
            return;
        }
        if let Err(error) = explorer_command().arg(path).spawn() {
            self.set_error(format!("Could not open the output folder: {error}"));
        } else {
            self.notify("Opened the output folder.");
        }
    }

    pub(super) fn open_clip(&mut self, path: &str) {
        if path.contains("://") {
            match open_remote_url(path) {
                Ok(()) => self.notify("Opened the generated clip."),
                Err(error) => self.set_error(error),
            }
            return;
        }
        let path = Path::new(path);
        if !path.is_file() {
            self.set_error(
                "The clip file is no longer available. Generate it again or copy its path.",
            );
            return;
        }
        if let Err(error) = explorer_command().arg(path).spawn() {
            self.set_error(format!("Could not open the clip: {error}"));
        } else {
            self.notify("Opened the generated clip.");
        }
    }

    pub(super) fn reexport_selected_clip(&mut self) {
        if self.running || self.exporting_clip.is_some() || self.onboarding || self.settings_open {
            return;
        }
        let Some(index) = self.selected_short else {
            self.set_error("Select a clip before exporting it again.");
            return;
        };
        let Some(result) = self.result.clone() else {
            self.set_error("There is no generated clip to export.");
            return;
        };
        let Some(short) = result.shorts.get(index) else {
            self.set_error("That clip is no longer in the project.");
            return;
        };
        let (start, end) = match parse_trim(
            &self.edit_start.text,
            &self.edit_end.text,
            result.transcript.duration,
        ) {
            Ok(bounds) => bounds,
            Err(error) => {
                self.set_error(format!("{error:#}"));
                return;
            }
        };
        let source = match self.local_export_source() {
            Ok(path) => path,
            Err(message) => {
                self.set_error(message);
                return;
            }
        };
        let music = match nonempty(&self.music_style.text) {
            Some(style) => match self.config.gemini_api_key() {
                Ok(key) => Some((key.to_owned(), style)),
                Err(_) => {
                    self.set_error("Add a Gemini API key in Settings to generate music.");
                    return;
                }
            },
            None => None,
        };
        let mut highlight = short.highlight.clone();
        highlight.start_time = start;
        highlight.end_time = end;
        highlight.hook_sentence = self.edit_hook.text.trim().to_owned();
        self.edit_revision = self.edit_revision.saturating_add(1);
        let stem = format!("short_{:02}_r{}", index + 1, self.edit_revision);
        let aspect = self.aspect.value().to_owned();
        let format = self.format;
        let reframe = self.reframe;
        let preset = self.caption_preset;
        let burn_captions = self.burn_captions;
        let text_hook = self.text_hook;
        let output_dir = self.output_dir().to_path_buf();
        let transcript = result.transcript.clone();
        self.exporting_clip = Some(index);
        self.clear_feedback();
        self.stage = if music.is_some() {
            "Generating music and exporting the clip"
        } else {
            "Exporting the trimmed clip"
        }
        .into();
        self.push_log(format!(
            "Exporting clip {} from {:.2}s to {:.2}s.",
            index + 1,
            start,
            end
        ));
        let (sender, receiver) = mpsc::channel();
        self.edit_receiver = Some(receiver);
        let worker = std::thread::Builder::new()
            .name("shorts-reexport".into())
            .spawn(move || {
                let outcome = export_trimmed_clip(
                    &source,
                    &output_dir,
                    &transcript,
                    TrimExport {
                        music,
                        index,
                        stem,
                        highlight,
                        aspect,
                        format,
                        reframe,
                        preset,
                        burn_captions,
                        text_hook,
                    },
                );
                let _ = sender.send(EditOutcome {
                    index,
                    result: outcome,
                });
            });
        if let Err(error) = worker {
            self.exporting_clip = None;
            self.edit_receiver = None;
            self.set_error(format!("Could not start the clip export: {error}"));
        }
    }

    fn fill_trim_fields(&mut self) {
        self.music_style.replace_text(String::new());
        let Some(index) = self.selected_short else {
            return;
        };
        let Some(short) = self
            .result
            .as_ref()
            .and_then(|result| result.shorts.get(index))
        else {
            return;
        };
        self.edit_start
            .replace_text(format_seconds(short.highlight.start_time));
        self.edit_end
            .replace_text(format_seconds(short.highlight.end_time));
        self.edit_hook
            .replace_text(short.highlight.hook_sentence.clone());
    }

    fn local_export_source(&self) -> Result<PathBuf, String> {
        let mut candidates = Vec::new();
        if let Some(resolved) = &self.resolved_source {
            candidates.push(resolved.path.clone());
        }
        if let Some(result) = &self.result
            && let Some(path) = local_media_path(&result.source_video_url)
        {
            candidates.push(path);
        }
        if let Some(path) = local_media_path(self.source.text.trim()) {
            candidates.push(path);
        }
        candidates.into_iter().find(|path| path.is_file()).ok_or_else(|| {
            "Re-export needs the local source file. Cloud clips can be previewed, but a new trim is rendered from a file on this PC.".into()
        })
    }

    fn drain_edit_events(&mut self) -> bool {
        let Some(receiver) = self.edit_receiver.take() else {
            return false;
        };
        let mut changed = false;
        let mut disconnected = false;
        loop {
            match receiver.try_recv() {
                Ok(event) => {
                    changed = true;
                    self.apply_edit(event);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    changed = true;
                    break;
                }
            }
        }
        if disconnected && self.exporting_clip.is_some() {
            self.exporting_clip = None;
            self.set_error("The clip export stopped unexpectedly.");
        }
        if self.exporting_clip.is_some() {
            self.edit_receiver = Some(receiver);
        }
        changed
    }

    fn apply_edit(&mut self, event: EditOutcome) {
        self.exporting_clip = None;
        match event.result {
            Ok(edited) => {
                if let Some(short) = self
                    .result
                    .as_mut()
                    .and_then(|result| result.shorts.get_mut(event.index))
                {
                    short.highlight = edited.highlight;
                    short.clip_url = Some(edited.path.to_string_lossy().into_owned());
                    short.error = None;
                }
                if let Some(player) = &mut self.short_player {
                    player.pause();
                }
                self.short_player = None;
                self.short_preview_error = None;
                self.sync_short_preview();
                if let Some(note) = edited.note {
                    self.push_log(note);
                }
                self.stage = "Exported the trimmed clip".into();
                if self.persist_projects
                    && let Some(result) = self.result.clone()
                {
                    match crate::projects::save_project(
                        self.output_dir(),
                        self.source.text.trim(),
                        &result,
                    ) {
                        Ok(_) => self.refresh_recent_projects(),
                        Err(error) => {
                            self.push_log(format!("Could not save the project: {error:#}"));
                        }
                    }
                }
                self.notify("Exported the trimmed clip.");
            }
            Err(message) => self.set_error(message),
        }
    }

    pub(super) fn poll(state: &mut Self, _time: f64, tx: &mut Tx) -> bool {
        let mut changed = state.drain_preview_events();
        changed |= state.sync_previews();
        changed |= state.drain_events();
        changed |= state.drain_edit_events();
        changed |= state.sync_previews();
        let media_states = [&state.source_player, &state.short_player]
            .map(|player| player.as_ref().map(Player::state));
        let media_changed = state.media_states != media_states;
        state.media_states = media_states;
        let media_active = state.media_needs_tick();
        if state.running
            || state.exporting_clip.is_some()
            || media_active
            || !state.preview_in_flight.is_empty()
        {
            tx.wake_after(Duration::from_millis(100));
        }
        changed || media_changed || media_active || state.exporting_clip.is_some()
    }

    pub(super) fn on_key(state: &mut Self, event: &rune::KeyEvent, tx: &mut Tx) -> bool {
        if state.running
            || state.exporting_clip.is_some()
            || state.onboarding
            || state.settings_open
            || tx.key_repeat()
        {
            return false;
        }
        use rune::{ChordKey::Enter, ChordMod::Primary};
        let generate = rune::Chord::new(Enter).with(Primary);
        if generate.matches_tx(event, tx) {
            state.start_generation();
            true
        } else {
            false
        }
    }
}

struct TrimExport {
    music: Option<(String, String)>,
    index: usize,
    stem: String,
    highlight: Highlight,
    aspect: String,
    format: u32,
    reframe: ReframeMode,
    preset: CaptionPreset,
    burn_captions: bool,
    text_hook: bool,
}

fn export_trimmed_clip(
    source: &Path,
    output_dir: &Path,
    transcript: &crate::models::Transcript,
    job: TrimExport,
) -> Result<EditedClip, String> {
    let (_, width, height) =
        dimensions(&job.aspect, job.format).map_err(|error| format!("{error:#}"))?;
    let plan = plan_captions(
        transcript,
        &job.highlight,
        job.preset,
        width,
        height,
        job.burn_captions,
        job.text_hook,
    );
    let note = (job.text_hook && plan.as_ref().is_none_or(|plan| !plan.burned_hook)).then(|| {
        "The text hook was not burned because it does not match the transcript in this range."
            .to_owned()
    });
    let music = job
        .music
        .map(|(key, style)| crate::music::generate(&key, &style))
        .transpose()
        .map_err(|error| format!("{error:#}"))?;
    let request = RenderRequest {
        music_path: music.as_ref().map(|file| file.path().to_owned()),
        aspect_ratio: job.aspect,
        format: job.format,
        reframe: job.reframe,
        captions_ass: plan.as_ref().map(|plan| plan.ass.clone()),
        captions_srt: plan
            .as_ref()
            .map(|plan| plan.srt.clone())
            .filter(|srt| !srt.is_empty()),
        file_stem: Some(job.stem),
    };
    let path = render_short(
        source,
        &job.highlight,
        output_dir,
        job.index,
        &request,
        &mut |_| {},
    )
    .map_err(|error| format!("{error:#}"))?;
    Ok(EditedClip {
        highlight: job.highlight,
        path,
        note,
    })
}

fn local_media_path(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() || (value.contains("://") && !value.starts_with("file://")) {
        return None;
    }
    if value.starts_with("file://") {
        return Url::parse(value)
            .ok()
            .and_then(|url| url.to_file_path().ok());
    }
    Some(PathBuf::from(value))
}

fn validate_https_media_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "Invalid remote media URL.".to_string())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Remote media URLs must use HTTPS without credentials.".into());
    }
    Ok(url)
}

fn remote_media_url(value: &str) -> Option<String> {
    validate_https_media_url(value)
        .ok()
        .map(|_| value.to_owned())
}

fn open_remote_url(value: &str) -> std::result::Result<(), String> {
    let url = validate_https_media_url(value)?;
    explorer_command()
        .arg(url.as_str())
        .spawn()
        .map_err(|error| format!("Could not open the remote media URL: {error}"))?;
    Ok(())
}

fn preview_cache_path(output_dir: &Path, url: &str) -> Result<PathBuf, String> {
    let url = validate_https_media_url(url)?;
    let extension = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .and_then(|name| Path::new(name).extension())
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let extension = if VIDEO_EXTENSIONS.contains(&extension.as_str()) {
        extension
    } else {
        "mp4".to_string()
    };
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in url.as_str().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Ok(output_dir
        .join("previews")
        .join(format!("{hash:016x}.{extension}")))
}

fn explorer_command() -> Command {
    let system_root = std::env::var_os("SystemRoot").expect("Windows provides SystemRoot");
    Command::new(PathBuf::from(system_root).join("Explorer.exe"))
}

fn preview_file(path: &Path) -> Result<Option<PathBuf>, String> {
    let mut path = path
        .canonicalize()
        .map_err(|_| "Local preview unavailable: the video file does not exist.".to_string())?;
    #[cfg(windows)]
    if let Some(raw) = path.to_str()
        // Rune's Windows media backend rejects canonical paths' verbatim prefix.
        && let Some(plain) = raw.strip_prefix(r"\\?\")
        && plain.as_bytes().get(1) == Some(&b':')
    {
        path = PathBuf::from(plain);
    }
    if !path.is_file() {
        return Err("Local preview unavailable: the source is not a file.".into());
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !VIDEO_EXTENSIONS.contains(&extension.as_str()) {
        return Err("Preview supports MP4, MOV, MKV, and WebM files.".into());
    }
    Ok(Some(path))
}

fn nonempty(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn clean_message(message: &str) -> String {
    let mut chars = message.chars().peekable();
    let mut out = String::new();
    let mut len = 0;
    while len < MESSAGE_LIMIT {
        let Some(c) = chars.next() else { break };
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() || next == '~' {
                        break;
                    }
                }
            }
            continue;
        }
        if c.is_control() && !matches!(c, '\n' | '\t') {
            continue;
        }
        out.push(c);
        len += 1;
    }
    out
}

fn result_summary(result: &GenerationResult) -> String {
    let ready = result
        .shorts
        .iter()
        .filter(|short| short.error.is_none() && short.clip_url.is_some())
        .count();
    let failed = result.shorts.len() - ready;
    if failed == 0 {
        format!(
            "{ready} clip{} generated successfully.",
            if ready == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "{ready} clip{} ready; {failed} failed.",
            if ready == 1 { "" } else { "s" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captions::CaptionPreset;
    use crate::clip::ReframeMode;
    use crate::config::{AsrProvider, LlmProvider, WhisperModelSize};
    use crate::models::{Highlight, RenderedShort, Transcript};
    use crate::projects::ProjectSummary;

    const GEMINI_FIXTURE: &str = "gemini-fixture-key-123456";
    const ELEVENLABS_FIXTURE: &str = "elevenlabs-fixture-key-123456";
    const OPENAI_FIXTURE: &str = "sk-openai-fixture-key-123456";
    const MUAPI_FIXTURE: &str = "muapi-fixture-key-123456";

    fn configured_api_keys() -> Config {
        let mut config = Config::default();
        ApiKey::Gemini.set(&mut config, Some(GEMINI_FIXTURE.into()));
        ApiKey::ElevenLabs.set(&mut config, Some(ELEVENLABS_FIXTURE.into()));
        ApiKey::OpenAi.set(&mut config, Some(OPENAI_FIXTURE.into()));
        ApiKey::MuApi.set(&mut config, Some(MUAPI_FIXTURE.into()));
        config
    }

    #[test]
    fn cloud_mode_requires_only_muapi_credentials() {
        let config = Config {
            muapi_api_key: Some(MUAPI_FIXTURE.into()),
            ..Default::default()
        };
        let mut state = GuiState::with_store(
            config,
            None,
            Box::new(MemoryCredentialStore::default()),
            false,
        );
        state.set_mode(Mode::Api);
        assert!(state.api_key_is_required(ApiKey::MuApi));
        assert!(!state.api_key_is_required(ApiKey::Gemini));
        assert!(!state.api_key_is_required(ApiKey::OpenAi));
        assert!(!state.api_key_is_required(ApiKey::ElevenLabs));
        assert!(state.configuration_message().is_none());
    }

    #[test]
    fn onboarding_saves_required_keys_and_completes_the_first_run() {
        let store = MemoryCredentialStore::default();
        let mut state =
            GuiState::with_store(Config::default(), None, Box::new(store.clone()), true);

        state.finish_onboarding();
        assert!(state.onboarding);
        assert!(state.error.as_deref().unwrap().contains("MuAPI"));
        assert!(
            store
                .read(CredentialKey::OnboardingComplete)
                .unwrap()
                .is_none()
        );

        state.gemini_key.replace_text(String::from(GEMINI_FIXTURE));
        state
            .elevenlabs_key
            .replace_text(String::from(ELEVENLABS_FIXTURE));
        state.finish_onboarding();

        assert!(!state.onboarding);
        assert_eq!(ApiKey::Gemini.get(&state.config), Some(GEMINI_FIXTURE));
        assert_eq!(
            ApiKey::ElevenLabs.get(&state.config),
            Some(ELEVENLABS_FIXTURE)
        );
        assert_eq!(
            store.read(CredentialKey::OnboardingComplete).unwrap(),
            Some("complete".into())
        );
        assert!(state.gemini_key.text.is_empty());
        assert!(state.elevenlabs_key.text.is_empty());
    }

    #[test]
    fn cloud_source_preview_requires_an_explicit_download() {
        let mut state = GuiState::with_config(Config::default(), None);
        state
            .source
            .replace_text(String::from("https://youtu.be/abcdefghijk"));
        state.apply_progress(ProgressEvent::Finished(GenerationResult {
            mode: Mode::Api,
            source_video_url: "https://cdn.example/source.mp4".into(),
            transcript: Transcript::default(),
            highlights: Vec::new(),
            shorts: Vec::new(),
        }));

        assert_eq!(
            state.remote_source_url().as_deref(),
            Some("https://cdn.example/source.mp4")
        );
        assert!(state.preview_in_flight.is_empty());
        assert!(state.source_preview_message().unwrap().contains("Download"));
    }

    #[test]
    fn remote_clips_use_cached_previews_without_changing_their_urls() {
        let url = "https://cdn.example/clip.mp4";
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/gui/tests/fixtures/preview.mp4");
        let mut state = GuiState::with_config(Config::default(), None);
        state.result = Some(GenerationResult {
            mode: Mode::Api,
            source_video_url: "https://cdn.example/source.mp4".into(),
            transcript: Transcript::default(),
            highlights: Vec::new(),
            shorts: vec![RenderedShort {
                highlight: Highlight {
                    title: "Cloud clip".into(),
                    start_time: 0.0,
                    end_time: 30.0,
                    score: 90,
                    hook_sentence: "Hook".into(),
                    virality_reason: "Reason".into(),
                    category: String::new(),
                },
                clip_url: Some(url.into()),
                error: None,
            }],
        });
        state.selected_short = Some(0);
        state.preview_cache.insert(url.into(), fixture.clone());

        assert_eq!(
            state.requested_short_path().unwrap(),
            preview_file(&fixture).unwrap()
        );
        assert_eq!(
            state.result.as_ref().unwrap().shorts[0].clip_url.as_deref(),
            Some(url)
        );
        assert!(state.preview_in_flight.is_empty());
    }

    #[test]
    fn remote_preview_urls_require_https_and_have_stable_cache_paths() {
        assert!(validate_https_media_url("http://cdn.example/clip.mp4").is_err());
        assert!(validate_https_media_url("https://user:pass@cdn.example/clip.mp4").is_err());
        let first =
            preview_cache_path(Path::new("output"), "https://cdn.example/clip.mp4").unwrap();
        let second =
            preview_cache_path(Path::new("output"), "https://cdn.example/clip.mp4").unwrap();
        assert_eq!(first, second);
        assert_eq!(first.extension().and_then(|ext| ext.to_str()), Some("mp4"));
        assert_eq!(
            preview_cache_path(Path::new("output"), "https://cdn.example/download")
                .unwrap()
                .extension()
                .and_then(|ext| ext.to_str()),
            Some("mp4")
        );
    }

    #[test]
    fn onboarding_accepts_muapi_only_and_selects_cloud_mode() {
        let store = MemoryCredentialStore::default();
        let mut state =
            GuiState::with_store(Config::default(), None, Box::new(store.clone()), true);

        state.muapi_key.replace_text(String::from(MUAPI_FIXTURE));

        assert!(state.onboarding_is_ready());
        assert_eq!(state.onboarding_mode(), Mode::Api);
        assert!(state.api_key_is_required(ApiKey::MuApi));
        assert!(!state.api_key_is_required(ApiKey::Gemini));
        state.finish_onboarding();

        assert!(!state.onboarding);
        assert_eq!(state.mode, Mode::Api);
        assert!(state.configuration_message().is_none());
        assert_eq!(
            store.read(CredentialKey::Api(ApiKey::MuApi)).unwrap(),
            Some(MUAPI_FIXTURE.into())
        );
    }

    #[test]
    fn onboarding_validates_the_selected_alternative_providers() {
        let store = MemoryCredentialStore::default();
        let config = Config {
            llm_provider: LlmProvider::OpenAi,
            asr_provider: AsrProvider::Whisper,
            ..Default::default()
        };
        let mut state = GuiState::with_store(config, None, Box::new(store.clone()), true);

        state.finish_onboarding();
        assert!(state.onboarding);
        assert!(state.error.as_deref().unwrap().contains("MuAPI"));
        assert!(
            store
                .read(CredentialKey::OnboardingComplete)
                .unwrap()
                .is_none()
        );

        state.openai_key.replace_text(String::from(OPENAI_FIXTURE));
        state.finish_onboarding();

        assert!(!state.onboarding);
        assert_eq!(ApiKey::OpenAi.get(&state.config), Some(OPENAI_FIXTURE));
        assert_eq!(ApiKey::Gemini.get(&state.config), None);
        assert_eq!(ApiKey::ElevenLabs.get(&state.config), None);
        assert_eq!(
            store.read(CredentialKey::OnboardingComplete).unwrap(),
            Some("complete".into())
        );
    }

    #[test]
    fn local_whisper_selection_and_model_persist_across_reloads() {
        let store = MemoryCredentialStore::default();
        let config = Config {
            gemini_api_key: Some(GEMINI_FIXTURE.into()),
            ..Default::default()
        };
        let mut state = GuiState::with_store(config, None, Box::new(store.clone()), true);
        assert!(!state.onboarding_is_ready());

        state.set_asr_provider(AsrProvider::Whisper);
        state.set_whisper_model(WhisperModelSize::Large);
        assert!(state.onboarding_is_ready());
        assert!(!state.api_key_is_required(ApiKey::ElevenLabs));
        state.finish_onboarding();

        assert!(!state.onboarding);
        assert_eq!(state.mode, Mode::Local);
        assert_eq!(state.config.asr_provider, AsrProvider::Whisper);
        assert_eq!(state.config.local_whisper_model, "large-v3");
        assert_eq!(
            store.read(CredentialKey::AsrProvider).unwrap(),
            Some("whisper".into())
        );
        assert_eq!(
            store.read(CredentialKey::LocalWhisperModel).unwrap(),
            Some("large-v3".into())
        );

        let stored = StoredCredentials::load(&store).unwrap();
        let mut restored = Config::default();
        stored.apply_to(&mut restored);
        let mut state = GuiState::with_store(restored, None, Box::new(store.clone()), false);
        assert!(!state.onboarding);
        assert_eq!(state.asr_provider, AsrProvider::Whisper);
        assert_eq!(state.whisper_model, WhisperModelSize::Large);

        state.open_settings();
        state.set_asr_provider(AsrProvider::ElevenLabs);
        state.set_whisper_model(WhisperModelSize::Small);
        state.save_settings();
        assert_eq!(state.config.asr_provider, AsrProvider::ElevenLabs);
        assert_eq!(state.config.local_whisper_model, "small");
        assert_eq!(
            store.read(CredentialKey::AsrProvider).unwrap(),
            Some("elevenlabs".into())
        );
        assert_eq!(
            store.read(CredentialKey::LocalWhisperModel).unwrap(),
            Some("small".into())
        );

        state.open_settings();
        state.set_asr_provider(AsrProvider::Whisper);
        state.set_whisper_model(WhisperModelSize::Tiny);
        state.close_settings();
        assert_eq!(state.asr_provider, AsrProvider::ElevenLabs);
        assert_eq!(state.whisper_model, WhisperModelSize::Small);
    }

    #[test]
    fn settings_preserve_blank_keys_and_apply_replacements_and_removals() {
        let store = MemoryCredentialStore::default();
        let mut state =
            GuiState::with_store(configured_api_keys(), None, Box::new(store.clone()), false);

        state.open_settings();
        state.save_settings();
        assert_eq!(ApiKey::Gemini.get(&state.config), Some(GEMINI_FIXTURE));
        assert_eq!(
            store.read(CredentialKey::Api(ApiKey::Gemini)).unwrap(),
            Some(GEMINI_FIXTURE.into())
        );

        state.open_settings();
        state.remove_api_key_draft(ApiKey::Gemini);
        state
            .muapi_key
            .replace_text(String::from("muapi-replacement-key-123456"));
        state.save_settings();

        assert_eq!(ApiKey::Gemini.get(&state.config), None);
        assert_eq!(
            ApiKey::MuApi.get(&state.config),
            Some("muapi-replacement-key-123456")
        );
        assert_eq!(
            store.read(CredentialKey::Api(ApiKey::Gemini)).unwrap(),
            Some(String::new())
        );
    }

    #[test]
    fn invalid_key_drafts_do_not_change_configuration_or_storage() {
        let store = MemoryCredentialStore::default();
        let mut state =
            GuiState::with_store(configured_api_keys(), None, Box::new(store.clone()), false);
        state.open_settings();
        state.gemini_key.replace_text(String::from("short"));
        state.save_settings();

        assert!(state.settings_open);
        assert!(state.error.is_some());
        assert_eq!(ApiKey::Gemini.get(&state.config), Some(GEMINI_FIXTURE));
        assert!(
            store
                .read(CredentialKey::Api(ApiKey::Gemini))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn running_jobs_ignore_credential_actions() {
        let store = MemoryCredentialStore::default();
        let mut state =
            GuiState::with_store(configured_api_keys(), None, Box::new(store.clone()), false);
        state.running = true;

        state.open_settings();
        state.remove_api_key_draft(ApiKey::Gemini);
        state.save_settings();

        assert!(!state.settings_open);
        assert_eq!(ApiKey::Gemini.get(&state.config), Some(GEMINI_FIXTURE));
        assert!(
            store
                .read(CredentialKey::Api(ApiKey::Gemini))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn defaults_match_the_cli_and_local_file_paths_are_lossless_for_unicode() {
        let mut state = GuiState::with_config(Config::default(), None);
        state.set_source(PathBuf::from("C:\\Videos\\录音 日本語.mp4"));
        let options = state.generation_options().unwrap();
        assert_eq!(options.source, "C:\\Videos\\录音 日本語.mp4");
        assert_eq!(options.mode, Mode::Local);
        assert_eq!(options.num_clips, 3);
        assert_eq!(options.target_duration, 30.0);
        assert_eq!(options.aspect_ratio, "9:16");
        assert_eq!(options.format, 1080);
        assert_eq!(options.analysis_format, 360);
        assert!(options.burn_captions);
        assert_eq!(options.caption_preset, CaptionPreset::Bold);
        assert_eq!(options.reframe, ReframeMode::Center);
        assert!(!options.text_hook);
    }

    #[test]
    fn export_choices_and_saved_projects_round_trip_in_the_studio() {
        let mut state = GuiState::with_config(Config::default(), None);
        state.set_reframe(ReframeMode::BlurredBackground);
        state.set_burn_captions(false);
        state.set_text_hook(true);
        state.set_caption_preset(CaptionPreset::Cinematic);
        state.set_duration_preset(1);
        state.source.replace_text(String::from("source.mp4"));
        let options = state.generation_options().unwrap();
        assert_eq!(options.reframe, ReframeMode::BlurredBackground);
        assert!(!options.burn_captions);
        assert!(options.text_hook);
        assert_eq!(options.caption_preset, CaptionPreset::Cinematic);
        assert_eq!(options.target_duration, 45.0);
        assert_eq!(state.duration_preset_index(), Some(1));

        state.running = true;
        state.set_reframe(ReframeMode::Center);
        assert_eq!(state.reframe, ReframeMode::BlurredBackground);
        state.running = false;

        state.recent_projects = vec![ProjectSummary {
            id: "1".into(),
            saved_at_unix: 1,
            source: "C:\\Videos\\saved.mp4".into(),
            ready_clips: 1,
            result: GenerationResult {
                mode: Mode::Local,
                source_video_url: "C:\\Videos\\saved.mp4".into(),
                transcript: Default::default(),
                highlights: Vec::new(),
                shorts: Vec::new(),
            },
        }];
        state.open_recent_project(0);
        assert_eq!(state.source.text, "C:\\Videos\\saved.mp4");
        assert_eq!(state.stage, "Opened saved project");
        assert!(state.result.is_some());
        assert!(state.notice.as_deref().unwrap().contains("saved project"));
    }

    #[test]
    fn invalid_or_sourceless_trims_do_not_start_an_export() {
        let mut state = GuiState::with_config(Config::default(), None);
        state.result = Some(GenerationResult {
            mode: Mode::Local,
            source_video_url: "missing-source.mp4".into(),
            transcript: Transcript {
                duration: 30.0,
                segments: Vec::new(),
            },
            highlights: Vec::new(),
            shorts: vec![RenderedShort {
                highlight: Highlight {
                    title: "Clip".into(),
                    start_time: 2.0,
                    end_time: 12.0,
                    score: 80,
                    hook_sentence: "Hook".into(),
                    virality_reason: "Reason".into(),
                    category: String::new(),
                },
                clip_url: Some("short_01.mp4".into()),
                error: None,
            }],
        });
        state.selected_short = Some(0);
        state.edit_start.replace_text("20");
        state.edit_end.replace_text("20.40");
        state.reexport_selected_clip();
        assert!(state.exporting_clip.is_none());
        assert!(state.edit_receiver.is_none());
        assert!(state.error.as_deref().unwrap().contains("at least 1"));

        state.edit_end.replace_text("28");
        state.reexport_selected_clip();
        assert!(state.exporting_clip.is_none());
        assert!(state.error.as_deref().unwrap().contains("local source"));
    }

    #[test]
    fn invalid_form_values_do_not_start_a_worker() {
        let mut state = GuiState::with_config(Config::default(), None);
        state.start_generation();
        assert!(!state.running);
        assert!(state.receiver.is_none());
        assert!(state.error.as_deref().unwrap().contains("source video"));

        state.source.replace_text(String::from("source.mp4"));
        state.num_clips.replace_text(String::from("0"));
        state.start_generation();
        assert!(!state.running);
        assert!(state.error.as_deref().unwrap().contains("between 1 and 30"));

        state.num_clips.replace_text(String::from("3"));
        state.target_duration.replace_text(String::from("NaN"));
        state.start_generation();
        assert!(!state.running);
        assert!(
            state
                .error
                .as_deref()
                .unwrap()
                .contains("between 5 and 600")
        );
    }

    #[test]
    fn worker_events_finish_the_job_and_bind_logs() {
        let (sender, receiver) = mpsc::channel();
        let mut state = GuiState::with_config(Config::default(), None);
        state.running = true;
        state.receiver = Some(receiver);
        sender
            .send(ProgressEvent::stage("Rendering clips", 0.75))
            .unwrap();
        for index in 0..LOG_LIMIT + 5 {
            sender
                .send(ProgressEvent::Log(format!("line {index}")))
                .unwrap();
        }
        sender
            .send(ProgressEvent::Finished(GenerationResult {
                mode: Mode::Local,
                source_video_url: "source.mp4".into(),
                transcript: Default::default(),
                highlights: Vec::new(),
                shorts: Vec::new(),
            }))
            .unwrap();
        drop(sender);

        assert!(state.drain_events());
        assert!(!state.running);
        assert!(state.receiver.is_none());
        assert_eq!(state.progress, 1.0);
        assert_eq!(state.panel, Panel::Results);
        assert!(state.result.is_some());
        assert_eq!(state.logs.len(), LOG_LIMIT);
        assert!(state.logs.iter().any(|message| message == "line 304"));
        assert!(
            state
                .logs
                .back()
                .unwrap()
                .contains("generated successfully")
        );
    }

    #[test]
    fn a_disconnected_worker_becomes_a_visible_error() {
        let (sender, receiver) = mpsc::channel();
        let mut state = GuiState::with_config(Config::default(), None);
        state.running = true;
        state.receiver = Some(receiver);
        drop(sender);
        assert!(state.drain_events());
        assert!(!state.running);
        assert!(
            state
                .error
                .as_deref()
                .unwrap()
                .contains("stopped unexpectedly")
        );
    }

    #[test]
    fn a_reported_failure_is_not_replaced_by_a_disconnect_error() {
        let (sender, receiver) = mpsc::channel();
        let mut state = GuiState::with_config(Config::default(), None);
        state.running = true;
        state.receiver = Some(receiver);
        sender
            .send(ProgressEvent::Failed(
                "yt-dlp failed (exit status 1): HTTP Error 429".into(),
            ))
            .unwrap();
        drop(sender);

        assert!(state.drain_events());
        assert!(!state.running);
        assert!(state.receiver.is_none());
        assert_eq!(
            state.error.as_deref(),
            Some("yt-dlp failed (exit status 1): HTTP Error 429")
        );
        assert!(
            !state
                .logs
                .iter()
                .any(|message| message.contains("stopped unexpectedly"))
        );
    }

    #[test]
    fn log_messages_strip_controls_and_are_bounded() {
        let mut state = GuiState::with_config(Config::default(), None);
        state.push_log(format!(
            "hello\u{1b}[31m\r\nworld{}",
            "x".repeat(MESSAGE_LIMIT * 2)
        ));
        assert_eq!(state.logs.len(), 1);
        let message = state.logs.front().unwrap();
        assert!(message.starts_with("hello\nworld"));
        assert!(
            !message
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        );
        assert!(message.len() <= MESSAGE_LIMIT * 4);
    }
}
