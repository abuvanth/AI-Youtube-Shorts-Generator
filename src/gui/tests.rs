use std::{path::PathBuf, sync::mpsc};

use rune::{Env, FileDialogKind, Runtime};

use super::{
    fitting_height,
    state::{GuiState, Panel},
    view,
};
use crate::{
    config::{AsrProvider, Config, LlmProvider, WhisperModelSize},
    credentials::{ApiKey, CredentialKey, CredentialStore, MemoryCredentialStore},
    models::{GenerationResult, Highlight, Mode, RenderedShort, Transcript},
    progress::ProgressEvent,
};

#[test]
fn fitting_height_keeps_the_window_above_a_taskbar() {
    let height = fitting_height(1080.0, 39.0, 1.25);
    assert_eq!(height, 776.0);
    assert!(
        height < 800.0,
        "a 1080px display at 125% cannot show an 800pt window"
    );

    assert_eq!(fitting_height(2160.0, 39.0, 1.25), 1640.0);
    assert_eq!(fitting_height(768.0, 31.0, 1.0), 681.0);
}

fn runtime(state: GuiState) -> Runtime<GuiState> {
    let mut runtime = Runtime::new(state, view::view, (1180.0, 800.0), Env::new(true, 1.0));
    runtime.poll = Some(GuiState::poll);
    runtime.on_key = Some(GuiState::on_key);
    runtime.frame(0.0);
    runtime
}

fn find_accessibility<'a>(node: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    if node["key"] == key {
        return Some(node);
    }
    node["children"]
        .as_array()?
        .iter()
        .find_map(|child| find_accessibility(child, key))
}

fn idle_state() -> GuiState {
    GuiState::with_config(Config::default(), None)
}

fn configured_state(store: MemoryCredentialStore, onboarding: bool) -> GuiState {
    let mut config = Config::default();
    ApiKey::Gemini.set(&mut config, Some("gemini-fixture-key-123456".into()));
    ApiKey::ElevenLabs.set(&mut config, Some("elevenlabs-fixture-key-123456".into()));
    ApiKey::OpenAi.set(&mut config, Some("sk-openai-fixture-key-123456".into()));
    ApiKey::MuApi.set(&mut config, Some("muapi-fixture-key-123456".into()));
    GuiState::with_store(config, None, Box::new(store), onboarding)
}

fn press(runtime: &mut Runtime<GuiState>, key: &str) {
    let (x, y) = runtime
        .find(key)
        .unwrap_or_else(|| panic!("{key} should be visible"));
    runtime.pointer_down(x, y);
    runtime.pointer_up(x, y);
    runtime.frame(1.0 / 60.0);
}

fn sample_result() -> GenerationResult {
    let highlight = Highlight {
        title: "A stronger opening".into(),
        start_time: 12.0,
        end_time: 42.0,
        score: 87,
        hook_sentence: "Most people miss the one detail that changes everything.".into(),
        virality_reason: "A clear curiosity gap with an immediate payoff.".into(),
        category: String::new(),
    };
    GenerationResult {
        mode: Mode::Local,
        source_video_url: "source.mp4".into(),
        transcript: Transcript {
            duration: 600.0,
            segments: Vec::new(),
        },
        highlights: vec![highlight.clone()],
        shorts: vec![
            RenderedShort {
                highlight,
                clip_url: Some("C:\\fixture\\shorts\\short_01.mp4".into()),
                error: None,
            },
            RenderedShort {
                highlight: Highlight {
                    title: "A clip that failed".into(),
                    start_time: 50.0,
                    end_time: 80.0,
                    score: 72,
                    hook_sentence: "This render did not finish.".into(),
                    virality_reason: "Kept visible so the successful clips can still be used."
                        .into(),
                    category: String::new(),
                },
                clip_url: None,
                error: Some("FFmpeg exited with an error.".into()),
            },
        ],
    }
}

fn preview_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/gui/tests/fixtures/preview.mp4")
}

#[test]
fn source_picker_preserves_unicode_paths() {
    let mut runtime = runtime(idle_state());
    press(&mut runtime, "browse-source");
    let (kind, options) = runtime
        .file_dialog()
        .expect("Browse should request a file dialog");
    assert_eq!(kind, FileDialogKind::Open);
    assert_eq!(options.title, "Choose a source video");
    assert!(
        options
            .filters
            .iter()
            .any(|(_, extensions)| extensions.iter().any(|ext| ext == "mp4"))
    );

    runtime.frame(2.0 / 60.0);
    assert!(runtime.file_dialog().is_some());
    let path = PathBuf::from("C:\\Videos\\录音 日本語.mp4");
    runtime.resolve_file_dialog(Some(path.clone())).unwrap();
    runtime.frame(3.0 / 60.0);
    assert_eq!(runtime.state.source.text, path.to_string_lossy());
}

#[test]
fn cancelling_the_source_picker_keeps_the_current_source() {
    let mut state = idle_state();
    state
        .source
        .replace_text(String::from("https://youtu.be/abcdefghijk"));
    let mut runtime = runtime(state);
    press(&mut runtime, "browse-source");
    runtime.resolve_file_dialog(None).unwrap();
    runtime.frame(2.0 / 60.0);
    assert_eq!(runtime.state.source.text, "https://youtu.be/abcdefghijk");
    assert!(runtime.state.error.is_none());
}

#[test]
fn output_folder_picker_uses_the_native_folder_dialog() {
    let mut runtime = runtime(idle_state());
    runtime.update(|state, tx| state.choose_output_folder(tx));
    runtime.frame(1.0 / 60.0);
    let (kind, options) = runtime
        .file_dialog()
        .expect("Choose should request a folder dialog");
    assert_eq!(kind, FileDialogKind::Folder);
    assert_eq!(options.title, "Choose an output folder");
    let path = PathBuf::from("C:\\Exports\\短视频");
    runtime.resolve_file_dialog(Some(path.clone())).unwrap();
    runtime.frame(2.0 / 60.0);
    assert_eq!(runtime.state.output_dir(), path.as_path());
}

#[test]
fn visible_settings_controls_update_the_generation_options() {
    let mut runtime = runtime(idle_state());
    press(&mut runtime, "mode-1");
    press(&mut runtime, "aspect-1");
    press(&mut runtime, "quality-2");
    assert_eq!(runtime.state.mode, Mode::Api);
    assert_eq!(runtime.state.aspect.label(), "1:1");
    assert_eq!(runtime.state.format, 1440);
}

#[test]
fn first_run_masks_keys_and_finishes_onboarding() {
    let store = MemoryCredentialStore::default();
    let state = GuiState::with_store(Config::default(), None, Box::new(store.clone()), true);
    let mut runtime = runtime(state);
    assert!(runtime.find("onboarding").is_some());
    assert!(runtime.find("finish-onboarding").is_some());
    assert!(!runtime.state.onboarding_is_ready());
    let initial_accessibility = runtime.accessibility_json();
    let finish = find_accessibility(&initial_accessibility, "finish-onboarding").unwrap();
    assert!(
        finish["states"]
            .as_array()
            .is_some_and(|states| states.iter().any(|state| state == "disabled"))
    );
    for (key, label) in [
        ("gemini-key-input-requirement", "Required"),
        ("elevenlabs-key-input-requirement", "Required"),
        ("openai-key-input-requirement", "Optional"),
        ("muapi-key-input-requirement", "Optional"),
    ] {
        let element = runtime
            .elements()
            .into_iter()
            .find(|element| element.key.as_deref() == Some(key))
            .unwrap_or_else(|| panic!("{key} should be laid out"));
        assert_eq!(element.label.as_deref(), Some(label));
    }

    runtime.update(|state, _| {
        state
            .gemini_key
            .replace_text(String::from("gemini-fixture-key-123456"));
        state
            .elevenlabs_key
            .replace_text(String::from("elevenlabs-fixture-key-123456"));
    });
    runtime.frame(1.0 / 60.0);
    assert!(runtime.state.onboarding_is_ready());
    let accessibility = runtime.accessibility_json();
    let tree = accessibility.to_string();
    assert!(!tree.contains("gemini-fixture-key-123456"));
    assert!(!tree.contains("elevenlabs-fixture-key-123456"));
    let input = find_accessibility(&accessibility, "gemini-key-input").unwrap();
    assert_eq!(input["role"], "password-input");

    press(&mut runtime, "finish-onboarding");

    assert!(!runtime.state.onboarding);
    assert!(runtime.find("window").is_some());
    assert_eq!(
        store.read(CredentialKey::OnboardingComplete).unwrap(),
        Some("complete".into())
    );
    let tree = runtime.accessibility_json().to_string();
    assert!(!tree.contains("gemini-fixture-key-123456"));
    assert!(!tree.contains("elevenlabs-fixture-key-123456"));
}

#[test]
fn muapi_only_onboarding_enables_cloud_generation() {
    let store = MemoryCredentialStore::default();
    let state = GuiState::with_store(Config::default(), None, Box::new(store.clone()), true);
    let mut runtime = runtime(state);
    runtime.update(|state, _| {
        state
            .muapi_key
            .replace_text(String::from("muapi-fixture-key-123456"));
    });
    runtime.frame(1.0 / 60.0);

    assert!(runtime.state.onboarding_is_ready());
    let tree = runtime.accessibility_json();
    let finish = find_accessibility(&tree, "finish-onboarding").unwrap();
    assert!(
        finish["states"]
            .as_array()
            .is_none_or(|states| !states.iter().any(|state| state == "disabled")),
        "{finish:?}"
    );
    let elements = runtime.elements();
    assert_eq!(
        elements
            .iter()
            .find(|element| element.key.as_deref() == Some("muapi-key-input-requirement"))
            .unwrap()
            .label
            .as_deref(),
        Some("Required")
    );
    assert_eq!(
        elements
            .iter()
            .find(|element| element.key.as_deref() == Some("gemini-key-input-requirement"))
            .unwrap()
            .label
            .as_deref(),
        Some("Optional")
    );
    press(&mut runtime, "finish-onboarding");
    assert_eq!(runtime.state.mode, Mode::Api);
    assert!(runtime.state.configuration_message().is_none());
}

#[test]
fn requirement_badges_and_focus_follow_the_selected_providers() {
    let config = Config {
        llm_provider: LlmProvider::OpenAi,
        asr_provider: AsrProvider::Whisper,
        ..Default::default()
    };
    let state = GuiState::with_store(
        config,
        None,
        Box::new(MemoryCredentialStore::default()),
        true,
    );
    let mut runtime = runtime(state);
    assert!(runtime.state.openai_key.focused);
    assert!(!runtime.state.gemini_key.focused);
    for (key, label) in [
        ("openai-key-input-requirement", "Required"),
        ("gemini-key-input-requirement", "Optional"),
        ("elevenlabs-key-input-requirement", "Optional"),
        ("muapi-key-input-requirement", "Optional"),
    ] {
        let element = runtime
            .elements()
            .into_iter()
            .find(|element| element.key.as_deref() == Some(key))
            .unwrap_or_else(|| panic!("{key} should be laid out"));
        assert_eq!(element.label.as_deref(), Some(label));
    }

    runtime.update(|state, _| {
        state
            .openai_key
            .replace_text(String::from("sk-openai-fixture-key-123456"));
    });
    runtime.frame(1.0 / 60.0);
    assert!(runtime.state.onboarding_is_ready());
    press(&mut runtime, "finish-onboarding");
    assert!(!runtime.state.onboarding);
}

#[test]
fn local_whisper_onboarding_offers_models_and_does_not_require_elevenlabs() {
    let store = MemoryCredentialStore::default();
    let state = GuiState::with_store(Config::default(), None, Box::new(store.clone()), true);
    let mut runtime = runtime(state);
    assert!(runtime.find("whisper-model-group").is_none());

    press(&mut runtime, "asr-provider-1");

    assert_eq!(runtime.state.asr_provider, AsrProvider::Whisper);
    assert_eq!(runtime.state.whisper_model, WhisperModelSize::Base);
    for (index, label) in ["Tiny", "Base", "Small", "Medium", "Large"]
        .into_iter()
        .enumerate()
    {
        let element = runtime
            .elements()
            .into_iter()
            .find(|element| element.key.as_deref() == Some(&format!("whisper-model-{index}")))
            .unwrap_or_else(|| panic!("whisper-model-{index} should be laid out"));
        assert_eq!(element.label.as_deref(), Some(label));
    }
    for (key, label) in [
        ("gemini-key-input-requirement", "Required"),
        ("elevenlabs-key-input-requirement", "Optional"),
    ] {
        let element = runtime
            .elements()
            .into_iter()
            .find(|element| element.key.as_deref() == Some(key))
            .unwrap();
        assert_eq!(element.label.as_deref(), Some(label));
    }

    runtime.update(|state, _| {
        state
            .gemini_key
            .replace_text(String::from("gemini-fixture-key-123456"));
    });
    runtime.frame(1.0 / 60.0);
    assert!(runtime.state.onboarding_is_ready());
    press(&mut runtime, "whisper-model-4");
    assert_eq!(runtime.state.whisper_model, WhisperModelSize::Large);
    press(&mut runtime, "finish-onboarding");

    assert!(!runtime.state.onboarding);
    assert_eq!(runtime.state.mode, Mode::Local);
    assert_eq!(runtime.state.config.asr_provider, AsrProvider::Whisper);
    assert_eq!(runtime.state.config.local_whisper_model, "large-v3");
    assert_eq!(ApiKey::ElevenLabs.get(&runtime.state.config), None);
    assert_eq!(
        store.read(CredentialKey::LocalWhisperModel).unwrap(),
        Some("large-v3".into())
    );
}

#[test]
fn settings_keep_saved_keys_hidden_and_persist_blank_drafts() {
    let store = MemoryCredentialStore::default();
    let mut runtime = runtime(configured_state(store.clone(), false));
    press(&mut runtime, "open-settings");

    assert!(runtime.find("settings").is_some());
    assert!(runtime.state.gemini_key.text.is_empty());
    assert!(runtime.state.muapi_key.text.is_empty());
    let tree = runtime.accessibility_json().to_string();
    assert!(!tree.contains("gemini-fixture-key-123456"));
    assert!(!tree.contains("sk-openai-fixture-key-123456"));
    assert!(!tree.contains("muapi-fixture-key-123456"));

    press(&mut runtime, "save-api-keys");

    assert!(!runtime.state.settings_open);
    assert_eq!(
        store.read(CredentialKey::Api(ApiKey::Gemini)).unwrap(),
        Some("gemini-fixture-key-123456".into())
    );
}

#[test]
fn settings_controls_replace_and_remove_keys() {
    let store = MemoryCredentialStore::default();
    let mut runtime = runtime(configured_state(store.clone(), false));
    press(&mut runtime, "open-settings");
    press(&mut runtime, "gemini-key-input-remove");
    runtime.update(|state, _| {
        state
            .muapi_key
            .replace_text(String::from("muapi-replacement-key-123456"));
    });
    runtime.frame(1.0 / 60.0);
    press(&mut runtime, "save-api-keys");

    assert_eq!(ApiKey::Gemini.get(&runtime.state.config), None);
    assert_eq!(
        ApiKey::MuApi.get(&runtime.state.config),
        Some("muapi-replacement-key-123456")
    );
    assert_eq!(
        store.read(CredentialKey::Api(ApiKey::Gemini)).unwrap(),
        Some(String::new())
    );
}

#[test]
fn running_jobs_do_not_open_settings() {
    let mut state = configured_state(MemoryCredentialStore::default(), false);
    state.running = true;
    let mut runtime = runtime(state);

    press(&mut runtime, "open-settings");

    assert!(!runtime.state.settings_open);
    assert!(runtime.find("settings").is_none());
}

#[test]
fn credential_controls_fit_the_minimum_window_size() {
    let onboarding = GuiState::with_store(
        Config::default(),
        None,
        Box::new(MemoryCredentialStore::default()),
        true,
    );
    let mut runtime = Runtime::new(onboarding, view::view, (940.0, 640.0), Env::new(true, 1.0));
    runtime.frame(0.0);
    for key in [
        "credential-header",
        "required-providers",
        "gemini-key-input",
        "credential-actions",
        "finish-onboarding",
    ] {
        let element = runtime
            .elements()
            .into_iter()
            .find(|element| element.key.as_deref() == Some(key))
            .unwrap_or_else(|| panic!("{key} should be laid out"));
        let [x, y, width, height] = element.rect;
        assert!(
            x >= 0.0 && y >= 0.0 && x + width <= 940.5 && y + height <= 640.5,
            "{key} exceeds the viewport: {:?}",
            element.rect
        );
    }
    for key in ["optional-providers", "openai-key-input", "muapi-key-input"] {
        let element = runtime
            .elements()
            .into_iter()
            .find(|element| element.key.as_deref() == Some(key))
            .unwrap_or_else(|| panic!("{key} should be laid out"));
        let [x, _, width, _] = element.rect;
        assert!(
            x >= 0.0 && x + width <= 940.5,
            "{key} overflows horizontally: {:?}",
            element.rect
        );
    }

    let mut settings = configured_state(MemoryCredentialStore::default(), false);
    settings.open_settings();
    let mut runtime = Runtime::new(settings, view::view, (940.0, 640.0), Env::new(true, 1.0));
    runtime.frame(0.0);
    for key in [
        "credential-header",
        "required-providers",
        "gemini-key-input",
        "credential-actions",
        "save-api-keys",
    ] {
        let element = runtime
            .elements()
            .into_iter()
            .find(|element| element.key.as_deref() == Some(key))
            .unwrap_or_else(|| panic!("{key} should be laid out"));
        let [x, y, width, height] = element.rect;
        assert!(
            x >= 0.0 && y >= 0.0 && x + width <= 940.5 && y + height <= 640.5,
            "{key} exceeds the viewport: {:?}",
            element.rect
        );
    }
}

#[test]
fn local_whisper_controls_fit_the_minimum_window_width() {
    let mut state = GuiState::with_store(
        Config::default(),
        None,
        Box::new(MemoryCredentialStore::default()),
        true,
    );
    state.set_asr_provider(AsrProvider::Whisper);
    let mut runtime = Runtime::new(state, view::view, (940.0, 640.0), Env::new(true, 1.0));
    runtime.frame(0.0);

    for key in [
        "transcription-settings",
        "asr-provider-group",
        "whisper-model-group",
        "whisper-model-0",
        "whisper-model-1",
        "whisper-model-2",
        "whisper-model-3",
        "whisper-model-4",
    ] {
        let element = runtime
            .elements()
            .into_iter()
            .find(|element| element.key.as_deref() == Some(key))
            .unwrap_or_else(|| panic!("{key} should be laid out"));
        let [x, _, width, _] = element.rect;
        assert!(
            x >= 0.0 && width > 0.0 && x + width <= 940.5,
            "{key} overflows horizontally: {:?}",
            element.rect
        );
    }
    let finish = runtime
        .elements()
        .into_iter()
        .find(|element| element.key.as_deref() == Some("finish-onboarding"))
        .unwrap();
    let [x, y, width, height] = finish.rect;
    assert!(
        x >= 0.0 && y >= 0.0 && x + width <= 940.5 && y + height <= 640.5,
        "finish-onboarding exceeds the viewport: {:?}",
        finish.rect
    );
}

#[test]
fn empty_source_is_reported_without_starting_work() {
    let mut runtime = runtime(idle_state());
    press(&mut runtime, "generate");
    assert!(!runtime.state.running);
    assert!(
        runtime
            .state
            .error
            .as_deref()
            .unwrap()
            .contains("source video")
    );
    assert!(runtime.find("error-alert").is_some());
    let progress = runtime
        .elements()
        .into_iter()
        .find(|element| element.key.as_deref() == Some("progress"))
        .unwrap();
    assert_eq!(progress.role, "progressbar");
    assert_eq!(progress.label.as_deref(), Some("Generation progress"));
}

#[test]
fn worker_events_update_progress_and_render_result_cards() {
    let (sender, receiver) = mpsc::channel();
    let mut state = idle_state();
    state.running = true;
    state.stage = "Starting generation".into();
    state.panel = Panel::Activity;
    state.receiver = Some(receiver);
    let mut runtime = runtime(state);

    sender
        .send(ProgressEvent::stage("Rendering clips", 0.7))
        .unwrap();
    sender
        .send(ProgressEvent::Log("Rendered short_01.mp4".into()))
        .unwrap();
    sender
        .send(ProgressEvent::Finished(sample_result()))
        .unwrap();
    runtime.frame(1.0 / 60.0);

    assert!(!runtime.state.running);
    assert_eq!(runtime.state.progress, 1.0);
    assert_eq!(runtime.state.panel, Panel::Results);
    assert!(runtime.find("result-0").is_some());
    assert!(runtime.find("result-0-path").is_some());
    assert!(runtime.find("result-1-error").is_some());
    assert!(runtime.find("progress-percent").is_some());
    let tree = runtime.a11y_json();
    assert!(tree.contains("A stronger opening"));
    assert!(tree.contains("Open"));
    assert!(tree.contains("Copy path"));
}

#[test]
fn running_jobs_omit_edit_and_generation_handlers() {
    let mut state = idle_state();
    state.running = true;
    state.stage = "Rendering clips".into();
    state.progress = 0.6;
    let mut runtime = runtime(state);

    assert!(runtime.find("generation-loader").is_some());
    let loader = runtime
        .elements()
        .into_iter()
        .find(|element| element.key.as_deref() == Some("generation-loader"))
        .unwrap();
    assert_eq!(loader.role, "progressbar");
    assert_eq!(loader.label.as_deref(), Some("Generation in progress"));

    let browse = runtime
        .elements()
        .into_iter()
        .find(|element| element.key.as_deref() == Some("browse-source"))
        .unwrap();
    assert!(!browse.focusable);

    press(&mut runtime, "browse-source");
    assert!(runtime.file_dialog().is_none());
    press(&mut runtime, "mode-1");
    assert_eq!(runtime.state.mode, Mode::Local);
    press(&mut runtime, "generate");
    assert!(runtime.state.running);
    assert_eq!(runtime.state.stage, "Rendering clips");
}

#[test]
fn source_editing_works_only_while_idle() {
    let mut runtime = runtime(idle_state());
    runtime.paste("https://youtu.be/abcdefghijk");
    runtime.frame(1.0 / 60.0);
    assert_eq!(runtime.state.source.text, "https://youtu.be/abcdefghijk");

    runtime.update(|state, _| state.running = true);
    runtime.frame(1.0 / 60.0);
    runtime.paste(" replacement");
    runtime.frame(1.0 / 60.0);
    assert_eq!(runtime.state.source.text, "https://youtu.be/abcdefghijk");
}

#[test]
fn local_source_preview_plays_and_pauses() {
    let mut state = idle_state();
    state
        .source
        .replace_text(preview_fixture().to_string_lossy());
    let mut runtime = runtime(state);

    assert!(runtime.find("source-preview-video").is_some());
    assert!(runtime.find("source-preview-seek").is_some());
    let player = runtime.state.source_player.clone().unwrap();
    assert!(
        matches!(player.source(), rune::MediaSource::File(path) if !path.to_string_lossy().starts_with(r"\\?\"))
    );
    assert!(!player.is_playing());

    press(&mut runtime, "source-preview-play");
    assert!(player.is_playing(), "{player:?}");
    runtime.frame(0.3);
    assert!(player.time() > 0.0);

    press(&mut runtime, "source-preview-play");
    assert!(!player.is_playing());
}

#[test]
fn youtube_source_preview_waits_for_resolution_and_shows_a_loader() {
    let (sender, receiver) = mpsc::channel();
    let mut state = idle_state();
    state
        .source
        .replace_text(String::from("https://youtu.be/abcdefghijk"));
    state.running = true;
    state.stage = "Resolving source".into();
    state.receiver = Some(receiver);
    let mut runtime = runtime(state);

    assert!(runtime.find("source-preview-loader").is_some());
    assert!(runtime.state.source_player.is_none());

    sender
        .send(ProgressEvent::SourceReady(preview_fixture()))
        .unwrap();
    runtime.frame(1.0 / 60.0);

    assert!(runtime.find("source-preview-video").is_some());
    assert!(runtime.find("source-preview-placeholder").is_none());
    assert!(runtime.state.source_player.is_some());
}

#[test]
fn missing_local_source_shows_an_inline_preview_error() {
    let mut state = idle_state();
    state
        .source
        .replace_text(String::from("C:\\missing\\source.mp4"));
    let runtime = runtime(state);

    assert!(runtime.state.source_player.is_none());
    assert!(
        runtime
            .state
            .source_preview_error
            .as_deref()
            .unwrap()
            .contains("does not exist")
    );
    assert!(runtime.state.error.is_none());
    assert!(runtime.find("source-preview-placeholder").is_some());
}

#[test]
fn finished_results_open_the_first_short_preview_and_allow_switching() {
    let (sender, receiver) = mpsc::channel();
    let mut state = idle_state();
    state.running = true;
    state.receiver = Some(receiver);
    let mut result = sample_result();
    for short in &mut result.shorts {
        short.clip_url = Some(preview_fixture().to_string_lossy().into_owned());
        short.error = None;
    }
    let mut runtime = runtime(state);

    sender.send(ProgressEvent::Finished(result)).unwrap();
    runtime.frame(1.0 / 60.0);

    assert_eq!(runtime.state.selected_short, Some(0));
    assert!(runtime.find("result-0-preview-video").is_some());
    assert!(runtime.find("result-1-preview-video").is_none());
    let player = runtime.state.short_player.clone().unwrap();
    press(&mut runtime, "result-0-preview-play");
    assert!(player.is_playing(), "{player:?}");

    runtime.update(|_, tx| tx.scroll_into_view("preview-result-1"));
    runtime.frame(1.0 / 60.0);
    let (_, before_y) = runtime.find("preview-result-1").unwrap();
    press(&mut runtime, "preview-result-1");
    let (_, after_y) = runtime.find("preview-result-1").unwrap();
    assert!((before_y - after_y).abs() < 1.0);
    assert_eq!(runtime.state.selected_short, Some(1));
    assert!(!player.is_playing());
    assert!(runtime.find("result-0-preview-video").is_none());
    assert!(runtime.find("result-1-preview-video").is_some());

    press(&mut runtime, "preview-result-1");
    assert_eq!(runtime.state.selected_short, None);
    assert!(runtime.state.short_player.is_none());
}

#[test]
fn simultaneous_previews_have_unique_keys_and_independent_controls() {
    let mut state = configured_state(MemoryCredentialStore::default(), false);
    state
        .source
        .replace_text(preview_fixture().to_string_lossy());
    let mut result = sample_result();
    result.shorts[0].clip_url = Some(preview_fixture().to_string_lossy().into_owned());
    state.result = Some(result);
    state.selected_short = Some(0);
    let mut runtime = runtime(state);
    let source = runtime.state.source_player.clone().unwrap();
    let short = runtime.state.short_player.clone().unwrap();
    let mut keys = std::collections::HashSet::new();
    for key in runtime
        .elements()
        .into_iter()
        .filter_map(|element| element.key)
    {
        assert!(keys.insert(key.clone()), "duplicate key: {key}");
    }

    press(&mut runtime, "source-preview-play");
    assert!(source.is_playing());
    assert!(!short.is_playing());
    press(&mut runtime, "result-0-preview-play");
    assert!(source.is_playing());
    assert!(short.is_playing());
    press(&mut runtime, "source-preview-play");
    assert!(!source.is_playing());
    assert!(short.is_playing());
}

#[test]
fn playback_refreshes_the_time_and_seek_fill_without_a_generation_job() {
    let mut state = idle_state();
    state
        .source
        .replace_text(preview_fixture().to_string_lossy());
    let mut runtime = runtime(state);
    press(&mut runtime, "source-preview-play");
    runtime.frame(0.6);
    runtime.frame(0.6);
    let elements = runtime.elements();
    let clock = elements
        .iter()
        .find(|element| element.key.as_deref() == Some("source-preview-time"))
        .unwrap();
    assert_eq!(clock.text.as_deref(), Some("00:01 / 00:01"));
    let fill = elements
        .iter()
        .find(|element| element.key.as_deref() == Some("source-preview-seek-fill"))
        .unwrap();
    assert!(fill.rect[2] > 0.0);
    assert!(!runtime.state.running);
}

#[test]
fn cancelled_seek_restores_the_original_position() {
    let mut state = idle_state();
    state
        .source
        .replace_text(preview_fixture().to_string_lossy());
    let mut runtime = runtime(state);
    let player = runtime.state.source_player.clone().unwrap();
    player.seek(0.2);
    runtime.frame(0.0);
    let original = player.time();
    let (x, y) = runtime.find("source-preview-seek").unwrap();
    runtime.touch(1, rune::TouchPhase::Started, x, y, 0.1);
    runtime.frame(0.1);
    runtime.touch(1, rune::TouchPhase::Moved, x + 30.0, y, 0.2);
    runtime.frame(0.2);
    assert!((player.time() - original).abs() > 0.1);
    runtime.touch(1, rune::TouchPhase::Cancelled, x + 30.0, y, 0.3);
    runtime.frame(0.3);
    assert!((player.time() - original).abs() < 0.001);
}

#[test]
fn failed_media_disables_playback_controls_and_shows_an_inline_error() {
    let mut state = idle_state();
    state.source.replace_text("C:\\fixture.mp4");
    let player = rune::Player::request("file:///C:/fixture.mp4")
        .header("X-Fixture", "unsupported-header")
        .video();
    assert!(matches!(player.state(), rune::PlayerState::Failed(_)));
    state.source_player = Some(player.clone());
    let mut runtime = Runtime::new(state, view::view, (1180.0, 800.0), Env::new(true, 1.0));
    runtime.frame(0.0);
    assert!(runtime.find("source-preview-failed").is_some());
    let tree = runtime.accessibility_json();
    for key in [
        "source-preview-play",
        "source-preview-restart",
        "source-preview-mute",
        "source-preview-seek",
    ] {
        let node = find_accessibility(&tree, key).unwrap();
        assert!(
            node["states"]
                .as_array()
                .unwrap()
                .iter()
                .any(|state| state == "disabled")
        );
    }
    press(&mut runtime, "source-preview-play");
    assert!(!player.is_playing());
    assert!(runtime.state.error.is_none());
}

#[test]
fn result_actions_and_playback_controls_fit_the_minimum_size_in_both_themes() {
    for dark in [true, false] {
        let mut state = configured_state(MemoryCredentialStore::default(), false);
        state
            .source
            .replace_text(preview_fixture().to_string_lossy());
        let mut result = sample_result();
        result.shorts[0].clip_url = Some(preview_fixture().to_string_lossy().into_owned());
        result.shorts[0].highlight.title =
            "The surprising detail that makes every opening stronger and keeps viewers watching"
                .into();
        state.result = Some(result);
        state.selected_short = Some(0);
        let mut runtime = Runtime::new(state, view::view, (940.0, 640.0), Env::new(dark, 1.0));
        runtime.poll = Some(GuiState::poll);
        runtime.frame(0.0);
        for key in [
            "preview-result-0",
            "open-result-0",
            "copy-result-0",
            "result-0-preview-play",
            "result-0-preview-seek",
            "source-preview-play",
        ] {
            let element = runtime
                .elements()
                .into_iter()
                .find(|element| element.key.as_deref() == Some(key))
                .unwrap();
            assert!(
                (element.visible[2] - element.rect[2]).abs() < 0.5,
                "{key} is clipped horizontally: {element:?}"
            );
            assert!(
                (element.visible[3] - element.rect[3]).abs() < 0.5,
                "{key} is clipped vertically: {element:?}"
            );
        }
        press(&mut runtime, "result-0-preview-play");
        assert!(runtime.state.short_player.as_ref().unwrap().is_playing());
        press(&mut runtime, "preview-result-0");
        assert_eq!(runtime.state.selected_short, None);
        press(&mut runtime, "preview-result-0");
        assert_eq!(runtime.state.selected_short, Some(0));
    }
}

#[test]
fn keyboard_arrows_select_tabs_and_segment_options() {
    let mut runtime = runtime(idle_state());
    runtime.update(|_, tx| tx.focus("tab-results", true));
    runtime.frame(0.0);
    runtime.key(rune::KeyEvent {
        key: rune::Key::Right,
        mods: Default::default(),
    });
    runtime.frame(0.0);
    assert_eq!(runtime.state.panel, Panel::Activity);
    runtime.update(|_, tx| tx.focus("mode-0", true));
    runtime.frame(0.0);
    runtime.key(rune::KeyEvent {
        key: rune::Key::Right,
        mods: Default::default(),
    });
    runtime.frame(0.0);
    assert_eq!(runtime.state.mode, Mode::Api);
}

#[test]
fn copy_feedback_is_visible_and_failed_clips_cannot_be_previewed() {
    let mut state = configured_state(MemoryCredentialStore::default(), false);
    let mut result = sample_result();
    result.shorts[1].clip_url = Some("C:\\fixture\\failed.mp4".into());
    state.result = Some(result);
    let mut runtime = runtime(state);
    press(&mut runtime, "copy-result-0");
    let notice = runtime
        .elements()
        .into_iter()
        .find(|element| element.key.as_deref() == Some("workspace-notice"))
        .unwrap();
    assert_eq!(notice.text.as_deref(), Some("Clip path copied."));
    let tree = runtime.accessibility_json();
    let preview = find_accessibility(&tree, "preview-result-1").unwrap();
    assert!(
        preview["states"]
            .as_array()
            .unwrap()
            .iter()
            .any(|state| state == "disabled")
    );
}

#[test]
fn onboarding_wheel_scrolls_optional_credentials_into_view() {
    let mut runtime = Runtime::new(
        configured_state(MemoryCredentialStore::default(), true),
        view::view,
        (940.0, 640.0),
        Env::new(true, 1.0),
    );
    runtime.poll = Some(GuiState::poll);
    runtime.frame(0.0);
    let viewport = runtime
        .elements()
        .into_iter()
        .find(|element| element.key.as_deref() == Some("credential-scroll"))
        .unwrap();
    runtime.wheel(
        viewport.rect[0] + viewport.rect[2] / 2.0,
        viewport.rect[1] + viewport.rect[3] / 2.0,
        -4000.0,
    );
    for frame in 0..90 {
        runtime.frame(frame as f64 / 60.0);
    }
    let input = runtime
        .elements()
        .into_iter()
        .find(|element| element.key.as_deref() == Some("muapi-key-input"))
        .unwrap();
    assert!(
        (input.visible[3] - input.rect[3]).abs() < 1.0,
        "MuAPI input is still clipped: {input:?}"
    );
}

#[test]
fn onboarding_explains_saved_keys_that_are_not_valid() {
    let mut config = Config::default();
    ApiKey::Gemini.set(&mut config, Some("short".into()));
    ApiKey::ElevenLabs.set(&mut config, Some("elevenlabs-fixture-key-123456".into()));
    let mut runtime = Runtime::new(
        GuiState::with_store(
            config,
            None,
            Box::new(MemoryCredentialStore::default()),
            true,
        ),
        view::view,
        (940.0, 640.0),
        Env::new(true, 1.0),
    );
    runtime.frame(0.0);

    assert!(!runtime.state.onboarding_is_ready());
    assert_eq!(runtime.state.invalid_api_key(), Some(ApiKey::Gemini));
    let elements = runtime.elements();
    let text_of = |key: &str| {
        elements
            .iter()
            .find(|element| element.key.as_deref() == Some(key))
            .unwrap_or_else(|| panic!("{key} should be laid out"))
            .text
            .clone()
    };
    assert_eq!(
        text_of("gemini-key-input-status-text").as_deref(),
        Some("Saved key is not valid - enter a new one")
    );
    assert_eq!(
        text_of("elevenlabs-key-input-status-text").as_deref(),
        Some("Saved securely")
    );
    assert_eq!(
        text_of("credential-footer-message").as_deref(),
        Some("The saved Gemini API key is not valid. Enter a new key to replace it.")
    );
    assert!(
        !elements
            .iter()
            .any(|element| element.text.as_deref() == Some("short")),
        "the saved key must stay hidden"
    );
}

#[test]
fn invalid_optional_saved_keys_do_not_block_onboarding() {
    let mut config = Config::default();
    ApiKey::Gemini.set(&mut config, Some("gemini-fixture-key-123456".into()));
    ApiKey::ElevenLabs.set(&mut config, Some("elevenlabs-fixture-key-123456".into()));
    ApiKey::MuApi.set(&mut config, Some("short".into()));
    let runtime = runtime(GuiState::with_store(
        config,
        None,
        Box::new(MemoryCredentialStore::default()),
        true,
    ));

    assert!(runtime.state.onboarding_is_ready());
    assert_eq!(runtime.state.invalid_api_key(), Some(ApiKey::MuApi));
    let texts = runtime
        .elements()
        .into_iter()
        .filter_map(|element| element.text)
        .collect::<Vec<_>>();
    assert!(
        texts
            .iter()
            .any(|text| text == "You're ready to generate. Optional keys can be added later."),
        "an invalid optional key must not block onboarding"
    );
    assert!(
        texts
            .iter()
            .any(|text| text == "Saved key is not valid - enter a new one"),
        "the invalid optional key must still be flagged"
    );
}

#[test]
fn cloud_results_copy_urls_and_offer_an_explicit_source_preview_download() {
    let mut state = configured_state(MemoryCredentialStore::default(), false);
    let mut result = sample_result();
    result.mode = Mode::Api;
    result.source_video_url = "https://cdn.example/source.mp4".into();
    result.shorts[0].clip_url = None;
    result.shorts[0].error = Some("Fixture clip is unavailable.".into());
    state
        .source
        .replace_text(String::from("https://youtu.be/abcdefghijk"));
    state.apply_progress(ProgressEvent::Finished(result));
    let mut result = state.result.take().unwrap();
    result.shorts[0].clip_url = Some("https://cdn.example/clip.mp4".into());
    result.shorts[0].error = None;
    state.result = Some(result);
    state.selected_short = None;
    let mut runtime = runtime(state);

    assert!(runtime.find("download-source-preview").is_some());
    assert!(runtime.find("open-source-preview").is_some());
    press(&mut runtime, "copy-result-0");
    let notice = runtime
        .elements()
        .into_iter()
        .find(|element| element.key.as_deref() == Some("workspace-notice"))
        .unwrap();
    assert_eq!(notice.text.as_deref(), Some("Clip URL copied."));
    let elements = runtime.elements();
    assert_eq!(
        elements
            .iter()
            .find(|element| element.key.as_deref() == Some("open-result-0"))
            .unwrap()
            .label
            .as_deref(),
        Some("Open URL")
    );
    assert_eq!(
        elements
            .iter()
            .find(|element| element.key.as_deref() == Some("copy-result-0"))
            .unwrap()
            .label
            .as_deref(),
        Some("Copy URL")
    );
}

#[test]
#[ignore = "renders local GPU snapshots without a desktop window"]
fn render_studio_fixtures() {
    for (name, size, theme) in [
        ("studio-dark", (1180.0, 800.0), "dark"),
        ("studio-light", (1180.0, 800.0), "light"),
        ("studio-minimum", (940.0, 640.0), "dark"),
        ("onboarding-dark", (940.0, 640.0), "dark"),
        ("onboarding-light", (940.0, 640.0), "light"),
        ("onboarding-whisper-dark", (1180.0, 800.0), "dark"),
        ("settings-light", (940.0, 640.0), "light"),
        ("settings-whisper-light", (940.0, 640.0), "light"),
        ("results-dark", (1180.0, 800.0), "dark"),
        ("results-minimum", (940.0, 640.0), "light"),
        ("running-dark", (940.0, 640.0), "dark"),
    ] {
        let mut state = configured_state(MemoryCredentialStore::default(), false);
        if name.starts_with("onboarding") {
            state = GuiState::with_store(
                Config::default(),
                None,
                Box::new(MemoryCredentialStore::default()),
                true,
            );
            if name.contains("whisper") {
                state.set_asr_provider(AsrProvider::Whisper);
                state.set_whisper_model(WhisperModelSize::Large);
            }
        } else if name.starts_with("settings") {
            state.open_settings();
            if name.contains("whisper") {
                state.set_asr_provider(AsrProvider::Whisper);
                state.set_whisper_model(WhisperModelSize::Large);
            }
        } else if name.starts_with("results") {
            let mut result = sample_result();
            result.shorts[0].clip_url = Some(preview_fixture().to_string_lossy().into_owned());
            result.shorts[0].highlight.title = "The surprising detail that makes every opening stronger and keeps viewers watching".into();
            state
                .source
                .replace_text(preview_fixture().to_string_lossy());
            state.result = Some(result);
            state.selected_short = Some(0);
            state.stage = "Your shorts are ready".into();
            state.progress = 1.0;
        } else if name.starts_with("running") {
            state.source.replace_text("https://youtu.be/abcdefghijk");
            state.running = true;
            state.stage = "Finding strong hooks".into();
            state.progress = 0.4;
        }
        let mut spec = rune::AppSpec::new("Studio fixture", size, idle_state, view::view);
        spec.poll = Some(GuiState::poll);
        let app = rune::App::new(spec).init(move || state);
        let snap = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("target/{name}.png"));
        let args = vec![
            "--snap".into(),
            snap.to_string_lossy().into_owned(),
            "--theme".into(),
            theme.into(),
            "--scale".into(),
            "1".into(),
            "--at".into(),
            "0.2".into(),
        ];
        rune::tooling::headless(app, rune::tooling::Args::parse_from(args));
        assert!(snap.is_file());
    }
}

#[test]
fn single_line_inputs_are_vertically_centered() {
    for (state, key) in [
        (idle_state(), "source-input"),
        (
            GuiState::with_store(
                Config::default(),
                None,
                Box::new(MemoryCredentialStore::default()),
                true,
            ),
            "gemini-key-input",
        ),
    ] {
        let runtime = runtime(state);
        let elements = runtime.elements();
        let input = elements
            .iter()
            .find(|element| element.key.as_deref() == Some(key))
            .unwrap();
        let field_key = format!("{key}-field");
        let field = elements
            .iter()
            .find(|element| element.key.as_deref() == Some(&field_key))
            .unwrap();
        let input_center = input.rect[1] + input.rect[3] / 2.0;
        let field_center = field.rect[1] + field.rect[3] / 2.0;
        assert!((input_center - field_center).abs() < 0.5);
        assert!(input.rect[3] < field.rect[3]);
    }
}

#[test]
fn primary_controls_fit_the_minimum_window_size() {
    let state = idle_state();
    let mut runtime = Runtime::new(state, view::view, (940.0, 640.0), Env::new(true, 1.0));
    runtime.frame(0.0);
    let elements = runtime.elements();
    for key in [
        "source-input",
        "browse-source",
        "generate",
        "output-panel",
        "workflow-guide",
    ] {
        let element = elements
            .iter()
            .find(|element| element.key.as_deref() == Some(key))
            .unwrap_or_else(|| panic!("{key} should be laid out"));
        let [x, y, width, height] = element.rect;
        assert!(
            x >= 0.0 && y >= 0.0,
            "{key} starts outside the viewport: {:?}",
            element.rect
        );
        assert!(
            x + width <= 940.5 && y + height <= 640.5,
            "{key} exceeds the viewport: {:?}",
            element.rect
        );
        assert!(width > 0.0 && height > 0.0, "{key} has no visible area");
    }
}
