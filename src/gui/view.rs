//! The Rune view. All generation effects stay outside the view function.

use std::{path::Path, rc::Rc};

use rune::{
    Align, Color, Cx, El, LoopSpec, Player, PlayerState, ScrollSpec, SurfaceFit, TextEdit, Tx, col,
    el, row, text, text_input,
    tok::{self, TextStyle, Theme, motion, type_},
};

use super::state::{Aspect as AspectChoice, GuiState, Panel};
use crate::{
    captions::CaptionPreset,
    clip::ReframeMode,
    config::{AsrProvider, WhisperModelSize},
    credentials::ApiKey,
    models::{Mode, RenderedShort},
};

pub(super) fn view(state: &GuiState, cx: &mut Cx<GuiState>) -> El {
    let dark = cx.system_dark();
    let theme = tok::theme(dark);
    let accent = tok::accent(1, dark);
    let warning = tok::accent(3, dark);
    let danger = tok::accent(2, dark);
    let success = tok::accent(4, dark);
    let credential_palette = CredentialPalette {
        theme,
        accent,
        success,
        danger,
    };
    cx.foreground(theme.text);
    cx.focus_ring(2.0, 3.0, accent);

    if state.onboarding {
        return credential_page(state, credential_palette, true);
    }
    if state.settings_open {
        return credential_page(state, credential_palette, false);
    }

    let configuration = state.configuration_message().map(|message| {
        row()
            .key("configuration-warning")
            .role("alert")
            .w_full()
            .px(12.0)
            .py(10.0)
            .gap(10.0)
            .radius(8.0)
            .fill(warning.alpha(0.12))
            .border(1.0, warning.alpha(0.35))
            .child(
                text(format!(
                    "Configuration issue: {message} Open Settings to update API keys. Key values are never displayed here."
                ))
                .font(type_::LABEL)
                .color(theme.text)
                .wrap()
                .grow(1.0)
                .min_w(0.0),
            )
            .child(button(
                "open-settings-warning",
                "Settings",
                theme,
                accent,
                false,
                !state.running,
                |state, _| state.open_settings(),
            ))
    });

    col()
        .key("window")
        .role("window")
        .label("AI YouTube Shorts Generator")
        .w_full()
        .h_full()
        .padding(20.0)
        .gap(18.0)
        .fill(theme.bg)
        .child(header(state, theme, accent, success, warning))
        .children(configuration)
        .child(
            row()
                .key("workspace")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .gap(18.0)
                .child(form(state, theme, accent))
                .child(output(state, theme, accent, danger)),
        )
        .children(state.notice.as_ref().map(|message| {
            text(message.clone())
                .key("workspace-notice")
                .role("status")
                .font(type_::LABEL)
                .color(theme.text_secondary)
                .wrap()
        }))
}

fn header(state: &GuiState, theme: Theme, accent: Color, success: Color, warning: Color) -> El {
    let configured = state.configuration_message().is_none();
    row()
        .key("header")
        .w_full()
        .center_y()
        .gap(14.0)
        .child(studio_mark("app-mark", accent))
        .child(
            col()
                .key("heading")
                .grow(1.0)
                .min_w(0.0)
                .gap(3.0)
                .child(
                    text("Shorts Studio")
                        .key("app-title")
                        .font(TextStyle::new(24.0, 650.0))
                        .color(theme.text),
                )
                .child(
                    text("Long videos. Strong hooks. Share-ready shorts.")
                        .font(type_::LABEL)
                        .color(theme.text_secondary)
                        .truncate(),
                ),
        )
        .child(
            row()
                .key("configuration-status")
                .role("status")
                .label(if configured {
                    "Configured"
                } else {
                    "Setup needed"
                })
                .px(11.0)
                .py(7.0)
                .gap(7.0)
                .center_y()
                .radius(16.0)
                .fill(if configured { success } else { warning }.alpha(0.08))
                .child(el().size(6.0, 6.0).radius(3.0).fill(if configured {
                    success
                } else {
                    warning
                }))
                .child(
                    text(if configured { "Ready" } else { "Setup needed" })
                        .font(type_::LABEL)
                        .color(theme.text_secondary),
                ),
        )
        .child(button(
            "open-output",
            "Output folder",
            theme,
            accent,
            false,
            true,
            |state, _| state.open_output_folder(),
        ))
        .child(button(
            "open-settings",
            "Settings",
            theme,
            accent,
            false,
            !state.running,
            |state, _| state.open_settings(),
        ))
}

fn studio_mark(key: &'static str, accent: Color) -> El {
    el().key(key)
        .size(46.0, 46.0)
        .radius(14.0)
        .fill(accent)
        .linear_gradient(
            0.7,
            [(0.0, Color::WHITE.alpha(0.20)), (1.0, Color::TRANSPARENT)],
        )
        .shadow(3.0, 12.0, accent.alpha(0.18))
        .center()
        .child(
            el().size(24.0, 24.0)
                .shape(
                    rune::Shape::new()
                        .fill(vec![[7.0, 5.0], [19.0, 12.0], [7.0, 19.0]], Color::WHITE)
                        .stroke(vec![[3.0, 4.0], [3.0, 20.0]], 2.0, Color::WHITE.alpha(0.65)),
                )
                .a11y_hidden(),
        )
}

#[derive(Clone, Copy)]
struct CredentialPalette {
    theme: Theme,
    accent: Color,
    success: Color,
    danger: Color,
}

fn credential_page(state: &GuiState, palette: CredentialPalette, onboarding: bool) -> El {
    let CredentialPalette {
        theme,
        accent,
        success,
        danger,
    } = palette;
    let title = if onboarding {
        "Choose transcription and add keys"
    } else {
        "Transcription and API keys"
    };
    let subtitle = if onboarding {
        "Choose cloud or local transcription, then add the required keys. Choices are saved for next time."
    } else {
        "Choose cloud or local transcription and manage masked API keys. Leave a key blank to keep its saved value."
    };
    let ready = !onboarding || state.onboarding_is_ready();
    let invalid_key = (!ready)
        .then(|| state.invalid_api_key())
        .flatten()
        .filter(|key| state.api_key_is_required(*key));
    let footer_hint = if !onboarding {
        "Blank fields keep saved keys. Use Remove to delete a key.".to_owned()
    } else if let Some(key) = invalid_key {
        if state.has_api_key_draft(key) {
            format!(
                "The {} looks incomplete. Paste the full key, or clear the field.",
                key.label()
            )
        } else {
            format!(
                "The saved {} is not valid. Enter a new key to replace it.",
                key.label()
            )
        }
    } else if ready {
        if state.onboarding_mode() == Mode::Api {
            "MuAPI cloud is ready. Local provider keys are optional.".to_owned()
        } else if state.asr_provider == AsrProvider::Whisper {
            "Local whisper.cpp is selected. You're ready to generate.".to_owned()
        } else {
            "You're ready to generate. Optional keys can be added later.".to_owned()
        }
    } else {
        "Add the required provider key or a MuAPI key to continue.".to_owned()
    };
    let footer_title = if onboarding && invalid_key.is_some() {
        "Check your keys"
    } else if onboarding {
        "Almost there"
    } else {
        "Your keys, your control"
    };
    let mut save = button(
        if onboarding {
            "finish-onboarding"
        } else {
            "save-api-keys"
        },
        if onboarding {
            "Save and continue"
        } else {
            "Save settings"
        },
        theme,
        accent,
        true,
        ready,
        |state, _| {
            if state.onboarding {
                state.finish_onboarding();
            } else {
                state.save_settings();
            }
        },
    )
    .h(44.0)
    .min_w(164.0)
    .px(18.0);
    if onboarding && ready {
        save = save.default_button().shadow(0.0, 12.0, accent.alpha(0.22));
    }

    let required_keys = ApiKey::ALL
        .into_iter()
        .filter(|key| state.api_key_is_required(*key))
        .collect::<Vec<_>>();
    let optional_keys = ApiKey::ALL
        .into_iter()
        .filter(|key| !state.api_key_is_required(*key))
        .collect::<Vec<_>>();
    let required_mode = if onboarding {
        state.onboarding_mode()
    } else {
        state.mode
    };
    let required_subtitle = match required_mode {
        Mode::Local => "Needed for local generation with your current provider settings.",
        Mode::Api => "Needed for MuAPI cloud generation with your current settings.",
        Mode::Auto => "Needed for the generation mode selected by your configured keys.",
    };

    col()
        .key(if onboarding { "onboarding" } else { "settings" })
        .role("window")
        .label(title)
        .w_full()
        .h_full()
        .padding(20.0)
        .gap(12.0)
        .fill(theme.bg)
        .child(
            row()
                .key("credential-header-wrap")
                .w_full()
                .justify_center()
                .child(
                    col()
                        .key("credential-header")
                        .w_full()
                        .max_w(820.0)
                        .padding(20.0)
                        .gap(18.0)
                        .radius(20.0)
                        .fill(theme.card)
                        .linear_gradient(0.0, [(0.0, accent.alpha(0.16)), (1.0, theme.card)])
                        .border(1.0, accent.alpha(0.18))
                        .shadow(0.0, 20.0, Color::BLACK.alpha(0.10))
                        .child(
                            row()
                                .w_full()
                                .center_y()
                                .gap(14.0)
                                .child(studio_mark("credential-mark", accent))
                                .child(
                                    col()
                                        .key("credential-heading")
                                        .grow(1.0)
                                        .min_w(0.0)
                                        .gap(3.0)
                                        .child(
                                            text(if onboarding {
                                                "WELCOME TO SHORTS STUDIO"
                                            } else {
                                                "WORKSPACE SETTINGS"
                                            })
                                            .font(TextStyle::new(10.0, 700.0))
                                            .color(accent),
                                        )
                                        .child(
                                            text(title)
                                                .font(TextStyle::new(24.0, 650.0))
                                                .color(theme.text),
                                        )
                                        .child(
                                            text(subtitle)
                                                .key("credential-subtitle")
                                                .font(type_::LABEL)
                                                .color(theme.text_secondary)
                                                .wrap(),
                                        ),
                                )
                                .child(
                                    row()
                                        .key("credential-storage-badge")
                                        .role("status")
                                        .label("Windows Credential Manager")
                                        .px(11.0)
                                        .py(8.0)
                                        .gap(7.0)
                                        .center_y()
                                        .radius(14.0)
                                        .fill(theme.bg)
                                        .border(1.0, theme.card_border)
                                        .child(el().size(6.0, 6.0).radius(3.0).fill(success))
                                        .child(
                                            text("Credential Manager")
                                                .font(type_::LABEL)
                                                .color(theme.text_secondary)
                                                .truncate(),
                                        ),
                                ),
                        ),
                ),
        )
        .child(
            col()
                .key("credential-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .items_center()
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .scroll_edge_fade(24.0)
                .scroll_margin(16.0)
                .child(
                    col()
                        .key("credential-card")
                        .w_full()
                        .max_w(820.0)
                        .shrink()
                        .gap(18.0)
                        .children(state.error.as_ref().map(|message| {
                            row()
                                .key("credential-error")
                                .role("alert")
                                .w_full()
                                .px(12.0)
                                .py(10.0)
                                .gap(10.0)
                                .center_y()
                                .radius(10.0)
                                .fill(danger.alpha(0.12))
                                .border(1.0, danger.alpha(0.4))
                                .child(
                                    text(message.clone())
                                        .font(type_::LABEL)
                                        .color(theme.text)
                                        .wrap()
                                        .grow(1.0)
                                        .min_w(0.0),
                                )
                                .child(button(
                                    "dismiss-credential-error",
                                    "Dismiss",
                                    theme,
                                    accent,
                                    false,
                                    true,
                                    |state, _| state.error = None,
                                ))
                        }))
                        .child(transcription_settings(state, palette))
                        .child(credential_section(
                            "required-providers",
                            "Required providers",
                            required_subtitle,
                            &required_keys,
                            true,
                            state,
                            palette,
                        ))
                        .child(credential_section(
                            "optional-providers",
                            "Optional providers",
                            "Not needed for the current workflow. You can add them later.",
                            &optional_keys,
                            false,
                            state,
                            palette,
                        )),
                ),
        )
        .child(
            row()
                .key("credential-footer-wrap")
                .w_full()
                .justify_center()
                .child(
                    row()
                        .key("credential-actions")
                        .w_full()
                        .max_w(820.0)
                        .center_y()
                        .gap(18.0)
                        .child(
                            col()
                                .key("credential-footer-hint")
                                .grow(1.0)
                                .min_w(0.0)
                                .gap(3.0)
                                .child(
                                    text(footer_title)
                                        .font(TextStyle::new(13.0, 650.0))
                                        .color(theme.text),
                                )
                                .child(
                                    text(footer_hint)
                                        .key("credential-footer-message")
                                        .font(type_::LABEL)
                                        .color(if invalid_key.is_some() {
                                            danger
                                        } else {
                                            theme.text_secondary
                                        })
                                        .wrap(),
                                ),
                        )
                        .child(
                            row()
                                .key("credential-buttons")
                                .gap(10.0)
                                .center_y()
                                .children((!onboarding).then(|| {
                                    button(
                                        "close-settings",
                                        "Back",
                                        theme,
                                        accent,
                                        false,
                                        true,
                                        |state, _| state.close_settings(),
                                    )
                                    .h(44.0)
                                }))
                                .child(save),
                        ),
                ),
        )
}

fn transcription_settings(state: &GuiState, palette: CredentialPalette) -> El {
    let CredentialPalette { theme, accent, .. } = palette;
    col()
        .key("transcription-settings")
        .w_full()
        .gap(10.0)
        .child(
            row()
                .key("transcription-header")
                .w_full()
                .center_y()
                .gap(9.0)
                .child(
                    text("Transcription")
                        .font(TextStyle::new(15.0, 650.0))
                        .color(theme.text),
                )
                .child(
                    text("Used for local generation")
                        .font(type_::LABEL)
                        .color(theme.text_secondary)
                        .wrap()
                        .grow(1.0)
                        .min_w(0.0),
                ),
        )
        .child(section_label("Provider"))
        .child(segmented(
            "asr-provider",
            &["ElevenLabs API", "Local whisper.cpp"],
            if state.asr_provider == AsrProvider::ElevenLabs {
                0
            } else {
                1
            },
            theme,
            accent,
            state.running,
            |state, index, _| {
                state.set_asr_provider(if index == 0 {
                    AsrProvider::ElevenLabs
                } else {
                    AsrProvider::Whisper
                });
            },
        ))
        .child(
            text(if state.asr_provider == AsrProvider::Whisper {
                "Runs on this PC with whisper-cli from whisper.cpp. Install it and add it to PATH; models download automatically on first use."
            } else {
                "Cloud transcription. An ElevenLabs key is required for local generation; MuAPI cloud does not use this choice."
            })
            .key("transcription-provider-hint")
            .font(type_::LABEL)
            .color(theme.text_secondary)
            .wrap(),
        )
        .children((state.asr_provider == AsrProvider::Whisper).then(|| {
            col()
                .key("whisper-model-section")
                .w_full()
                .gap(7.0)
                .child(section_label("Whisper model size"))
                .child(segmented(
                    "whisper-model",
                    &WhisperModelSize::ALL.map(WhisperModelSize::label),
                    WhisperModelSize::ALL
                        .iter()
                        .position(|model| *model == state.whisper_model)
                        .unwrap_or(1),
                    theme,
                    accent,
                    state.running,
                    |state, index, _| state.set_whisper_model(WhisperModelSize::ALL[index]),
                ))
                .child(
                    text("Tiny is fastest. Large is more accurate but needs more RAM and disk space.")
                        .font(type_::LABEL)
                        .color(theme.text_secondary)
                        .wrap(),
                )
        }))
}

fn credential_section(
    key: &'static str,
    title: &'static str,
    subtitle: &'static str,
    keys: &[ApiKey],
    required: bool,
    state: &GuiState,
    palette: CredentialPalette,
) -> El {
    let CredentialPalette { theme, .. } = palette;
    col()
        .key(key)
        .w_full()
        .gap(10.0)
        .child(
            row()
                .key(format!("{key}-header"))
                .w_full()
                .center_y()
                .gap(9.0)
                .child(
                    text(title)
                        .font(TextStyle::new(15.0, 650.0))
                        .color(theme.text),
                )
                .child(requirement_badge(format!("{key}-badge"), required, palette))
                .child(
                    text(subtitle)
                        .font(type_::LABEL)
                        .color(theme.text_secondary)
                        .wrap()
                        .grow(1.0)
                        .min_w(0.0),
                ),
        )
        .child(
            row()
                .key(format!("{key}-cards"))
                .w_full()
                .flex_wrap()
                .gap(12.0)
                .children(keys.iter().copied().map(|key| {
                    api_key_card(state, key, required, palette)
                        .grow(1.0)
                        .min_w(380.0)
                })),
        )
}

fn requirement_badge(key: String, required: bool, palette: CredentialPalette) -> El {
    let CredentialPalette { theme, accent, .. } = palette;
    let label = if required { "Required" } else { "Optional" };
    row()
        .key(key)
        .role("status")
        .label(label)
        .px(8.0)
        .py(4.0)
        .gap(5.0)
        .center_y()
        .radius(10.0)
        .fill(if required {
            accent.alpha(0.12)
        } else {
            theme.control_track
        })
        .border(
            1.0,
            if required {
                accent.alpha(0.24)
            } else {
                theme.card_border
            },
        )
        .child(el().size(5.0, 5.0).radius(2.5).fill(if required {
            accent
        } else {
            theme.text_secondary
        }))
        .child(
            text(label)
                .font(TextStyle::new(10.0, 650.0))
                .color(if required {
                    accent
                } else {
                    theme.text_secondary
                }),
        )
}

fn api_key_card(state: &GuiState, key: ApiKey, required: bool, palette: CredentialPalette) -> El {
    let CredentialPalette {
        theme,
        accent,
        success,
        danger,
    } = palette;
    let configured = state.api_key_is_configured(key);
    let removed = state.api_key_is_removed(key);
    let has_draft = state.has_api_key_draft(key);
    let invalid = state.api_key_is_invalid(key);
    let status = if removed {
        "Will be removed when you save"
    } else if has_draft && invalid {
        "Key looks incomplete - re-enter it"
    } else if has_draft {
        "New key entered - save to apply"
    } else if invalid {
        "Saved key is not valid - enter a new one"
    } else if configured {
        "Saved securely"
    } else if required {
        "Required to continue"
    } else {
        "Optional - not configured"
    };
    let status_color = if removed || invalid {
        danger
    } else if has_draft {
        accent
    } else if configured {
        success
    } else {
        theme.text_secondary
    };
    let input = line_input(
        key.input_name(),
        state.api_key_input(key),
        if configured {
            "Leave blank to keep saved key"
        } else {
            "Paste an API key"
        },
        key.label(),
        theme,
        false,
        move |state| state.api_key_input_mut(key),
    );

    col()
        .key(format!("{}-card", key.input_name()))
        .gap(11.0)
        .padding(16.0)
        .radius(14.0)
        .fill(theme.card)
        .border(
            1.0,
            if removed || (invalid && !has_draft) {
                danger.alpha(0.45)
            } else if has_draft {
                accent.alpha(0.45)
            } else {
                theme.card_border
            },
        )
        .shadow(0.0, 10.0, Color::BLACK.alpha(0.05))
        .child(
            row()
                .key(format!("{}-heading", key.input_name()))
                .w_full()
                .center_y()
                .gap(11.0)
                .child(
                    el().size(38.0, 38.0)
                        .radius(10.0)
                        .fill(accent.alpha(if required { 0.14 } else { 0.08 }))
                        .center()
                        .child(
                            text(provider_monogram(key))
                                .font(TextStyle::new(12.0, 700.0))
                                .color(accent),
                        ),
                )
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(3.0)
                        .child(
                            text(key.label())
                                .font(TextStyle::new(14.0, 650.0))
                                .color(theme.text),
                        )
                        .child(
                            text(key.description())
                                .font(type_::LABEL)
                                .color(theme.text_secondary)
                                .wrap(),
                        ),
                )
                .child(requirement_badge(
                    format!("{}-requirement", key.input_name()),
                    required,
                    palette,
                )),
        )
        .child(input)
        .child(
            row()
                .key(format!("{}-status", key.input_name()))
                .w_full()
                .gap(9.0)
                .center_y()
                .child(el().size(6.0, 6.0).radius(3.0).fill(status_color))
                .child(
                    text(status)
                        .key(format!("{}-status-text", key.input_name()))
                        .font(type_::LABEL)
                        .color(status_color)
                        .grow(1.0)
                        .min_w(0.0),
                )
                .children(if removed {
                    Some(
                        button(
                            format!("{}-keep", key.input_name()),
                            "Keep saved",
                            theme,
                            accent,
                            false,
                            true,
                            move |state, _| {
                                state.key_removals[key.index()] = false;
                                state.clear_feedback();
                            },
                        )
                        .h(30.0)
                        .px(10.0),
                    )
                } else if configured {
                    Some(
                        button(
                            format!("{}-remove", key.input_name()),
                            "Remove",
                            theme,
                            accent,
                            false,
                            true,
                            move |state, _| state.remove_api_key_draft(key),
                        )
                        .h(30.0)
                        .px(10.0),
                    )
                } else {
                    None
                }),
        )
}

fn provider_monogram(key: ApiKey) -> &'static str {
    match key {
        ApiKey::Gemini => "G",
        ApiKey::ElevenLabs => "EL",
        ApiKey::OpenAi => "AI",
        ApiKey::MuApi => "M",
    }
}

fn form(state: &GuiState, theme: Theme, accent: Color) -> El {
    let running = state.running;
    let mut generate = button(
        "generate",
        if running {
            "Generating..."
        } else {
            "Generate shorts"
        },
        theme,
        accent,
        true,
        !running,
        |state, _| state.start_generation(),
    )
    .h(48.0)
    .w_full();
    if !running {
        generate = generate
            .default_button()
            .shadow(3.0, 12.0, accent.alpha(0.16));
    }

    col()
        .key("generation-form")
        .w(364.0)
        .min_w(340.0)
        .h_full()
        .min_h(0.0)
        .gap(10.0)
        .child(
            col()
                .key("form-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .scroll_edge_fade(32.0)
                .padding(18.0)
                .gap(16.0)
                .fill(theme.card)
                .border(1.0, theme.card_border)
                .radius(16.0)
                .child(
                    col().key("form-heading").w_full().gap(4.0)
                        .child(text("Create your shorts").font(TextStyle::new(17.0, 650.0)).color(theme.text))
                        .child(text("Choose a source, then make it yours.").font(type_::LABEL).color(theme.text_secondary).wrap()),
                )
                .child(
                    col()
                        .key("source-section")
                        .w_full()
                        .gap(7.0)
                        .child(section_label("Source video"))
                        .child(
                            row()
                                .w_full()
                                .gap(8.0)
                                .child(
                                    line_input(
                                        "source-input",
                                        &state.source,
                                        "HTTPS YouTube URL or local video",
                                        "Source video",
                                        theme,
                                        running,
                                        |state| &mut state.source,
                                    )
                                    .grow(1.0)
                                    .min_w(0.0),
                                )
                                .child(button(
                                    "browse-source",
                                    "Browse",
                                    theme,
                                    accent,
                                    false,
                                    !running,
                                    |state, tx| state.choose_source(tx),
                                )),
                        )
                        .child(
                            text("Paste an HTTPS YouTube link, or choose an MP4, MOV, MKV, or WebM file.")
                                .font(type_::LABEL)
                                .color(theme.text_secondary)
                                .wrap(),
                        )
                        .children((!state.source.text.trim().is_empty()).then(|| {
                            source_preview(state, theme, accent)
                        })),
                )
                .child(el().key("form-divider-1").w_full().h(1.0).fill(theme.separator))
                .child(
                    col()
                        .key("mode-section")
                        .w_full()
                        .gap(7.0)
                        .child(section_label("Generation mode"))
                        .child(segmented(
                            "mode",
                            &["Local FFmpeg", "MuAPI Cloud"],
                            if state.mode == Mode::Local { 0 } else { 1 },
                            theme,
                            accent,
                            running,
                            |state, index, _| {
                                state.set_mode(if index == 0 { Mode::Local } else { Mode::Api });
                            },
                        ))
                        .child(
                            text(if state.mode == Mode::Local {
                                "Requires yt-dlp, FFmpeg, and ffprobe on PATH. Rendering stays on this PC."
                            } else {
                                "MuAPI handles download, transcription, highlight selection, and autocropping. Local tools are not required."
                            })
                            .font(type_::LABEL)
                            .color(theme.text_secondary)
                            .wrap(),
                        ),
                )
                .child(
                    row()
                        .key("clip-settings")
                        .w_full()
                        .equal_columns(2)
                        .gap(12.0)
                        .child(field(
                            "Clip count",
                            line_input(
                                "clip-count",
                                &state.num_clips,
                                "3",
                                "Clip count",
                                theme,
                                running,
                                |state| &mut state.num_clips,
                            ),
                        ))
                        .child(field(
                            "Target seconds",
                            line_input(
                                "target-duration",
                                &state.target_duration,
                                "30",
                                "Target duration in seconds",
                                theme,
                                running,
                                |state| &mut state.target_duration,
                            ),
                        )),
                )
                .child(
                    col()
                        .key("aspect-section")
                        .w_full()
                        .gap(7.0)
                        .child(section_label("Aspect ratio"))
                        .child(segmented(
                            "aspect",
                            &AspectChoice::ALL.map(AspectChoice::label),
                            aspect_index(state.aspect),
                            theme,
                            accent,
                            running,
                            |state, index, _| state.set_aspect(AspectChoice::ALL[index]),
                        )),
                )
                .child(
                    col()
                        .key("quality-section")
                        .w_full()
                        .gap(7.0)
                        .child(section_label("Output quality"))
                        .child(segmented(
                            "quality",
                            &["720p", "1080p", "1440p", "4K"],
                            quality_index(state.format),
                            theme,
                            accent,
                            running,
                            |state, index, _| state.set_format([720, 1080, 1440, 2160][index]),
                        )),
                )
                .child(export_controls(state, theme, accent, running))
                .child(button(
                    "advanced-toggle",
                    if state.advanced { "Hide advanced settings" } else { "Advanced settings" },
                    theme,
                    accent,
                    false,
                    !running,
                    |state, _| state.toggle_advanced(),
                ))
                .children((state.advanced).then(|| advanced_settings(state, theme, accent, running)))
                .child(
                    col()
                        .key("prompt-section")
                        .w_full()
                        .gap(7.0)
                        .child(section_label("Creative direction (optional)"))
                        .child(prompt_input(state, theme, running))
                        .child(
                            text("Guide tone, audience, or topics. The model still uses only source content.")
                                .font(type_::LABEL)
                                .color(theme.text_secondary)
                                .wrap(),
                        ),
                ),
        )
        .child(generate)
        .child(
            text("Ctrl+Enter starts generation. Settings lock while a job is running.")
                .font(type_::LABEL)
                .color(theme.text_secondary)
                .align(Align::Center)
                .wrap(),
        )
}

fn export_controls(state: &GuiState, theme: Theme, accent: Color, running: bool) -> El {
    let preset_index = CaptionPreset::ALL
        .iter()
        .position(|preset| *preset == state.caption_preset)
        .unwrap_or(1);
    let duration_index = state.duration_preset_index().unwrap_or(usize::MAX);
    col()
        .key("export-section")
        .w_full()
        .gap(12.0)
        .child(
            col()
                .w_full()
                .gap(7.0)
                .child(section_label("Length preset"))
                .child(segmented(
                    "duration-preset",
                    &["25s", "45s", "75s"],
                    duration_index,
                    theme,
                    accent,
                    running,
                    |state, index, _| state.set_duration_preset(index),
                ))
                .child(
                    text("Asks the model for clips near this length. Real moments outside it are kept.")
                        .font(type_::LABEL)
                        .color(theme.text_secondary)
                        .wrap(),
                ),
        )
        .child(
            col()
                .w_full()
                .gap(7.0)
                .child(section_label("Captions"))
                .child(segmented(
                    "captions",
                    &["Off", "On"],
                    usize::from(state.burn_captions),
                    theme,
                    accent,
                    running,
                    |state, index, _| state.set_burn_captions(index == 1),
                ))
                .child(segmented(
                    "caption-style-a",
                    &["Clean", "Bold", "Karaoke"],
                    if preset_index < 3 {
                        preset_index
                    } else {
                        usize::MAX
                    },
                    theme,
                    accent,
                    running,
                    |state, index, _| state.set_caption_preset(CaptionPreset::ALL[index]),
                ))
                .child(segmented(
                    "caption-style-b",
                    &["Podcast", "Gaming", "Cinema"],
                    if preset_index >= 3 {
                        preset_index - 3
                    } else {
                        usize::MAX
                    },
                    theme,
                    accent,
                    running,
                    |state, index, _| state.set_caption_preset(CaptionPreset::ALL[index + 3]),
                ))
                .child(
                    text("Captions come from the transcript. Karaoke highlights each phrase. Word-by-word timing is not invented.")
                        .font(type_::LABEL)
                        .color(theme.text_secondary)
                        .wrap(),
                ),
        )
        .child(
            col()
                .w_full()
                .gap(7.0)
                .child(section_label("Framing"))
                .child(segmented(
                    "reframe",
                    &["Center", "Blur fill"],
                    if state.reframe == ReframeMode::Center { 0 } else { 1 },
                    theme,
                    accent,
                    running,
                    |state, index, _| {
                        state.set_reframe(if index == 0 {
                            ReframeMode::Center
                        } else {
                            ReframeMode::BlurredBackground
                        });
                    },
                ))
                .child(
                    text("Face tracking is not available. Blur fill keeps the full frame visible instead of guessing a subject crop.")
                        .font(type_::LABEL)
                        .color(theme.text_secondary)
                        .wrap(),
                ),
        )
        .child(
            col()
                .w_full()
                .gap(7.0)
                .child(section_label("Opening"))
                .child(segmented(
                    "hook-mode",
                    &["Original", "Text hook"],
                    usize::from(state.text_hook),
                    theme,
                    accent,
                    running,
                    |state, index, _| state.set_text_hook(index == 1),
                ))
                .child(
                    text("Original starts on the spoken hook. Text hook adds that line on screen only when it matches the transcript. Spoken AI hooks are not in this build.")
                        .font(type_::LABEL)
                        .color(theme.text_secondary)
                        .wrap(),
                ),
        )
        .child(
            text(if state.mode == Mode::Local {
                "Highlight detection sends the transcript, and the YouTube URL when you imported one, to Gemini or OpenAI. Scores are editorial estimates, not view predictions. The video file stays on this PC."
            } else {
                "MuAPI cloud rendering does not apply these caption, hook, or framing controls."
            })
            .font(type_::LABEL)
            .color(theme.text_secondary)
            .wrap(),
        )
}

fn advanced_settings(state: &GuiState, theme: Theme, accent: Color, running: bool) -> El {
    col()
        .key("advanced-settings")
        .w_full()
        .gap(16.0)
        .child(
            el().key("advanced-divider")
                .w_full()
                .h(1.0)
                .fill(theme.separator),
        )
        .child(field(
            "Language code (optional)",
            line_input(
                "language",
                &state.language,
                "auto",
                "Transcription language code",
                theme,
                running,
                |state| &mut state.language,
            ),
        ))
        .child(
            col()
                .key("analysis-quality-section")
                .w_full()
                .gap(7.0)
                .child(section_label("Analysis quality"))
                .child(segmented(
                    "analysis-quality",
                    &["360p", "480p", "720p", "1080p"],
                    analysis_index(state.analysis_format),
                    theme,
                    accent,
                    running,
                    |state, index, _| {
                        state.set_analysis_format([360, 480, 720, 1080][index]);
                    },
                )),
        )
        .child(
            col()
                .key("output-folder-section")
                .w_full()
                .gap(7.0)
                .child(section_label("Output folder"))
                .child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .child(
                            row()
                                .key("output-path")
                                .grow(1.0)
                                .min_w(0.0)
                                .h(36.0)
                                .px(10.0)
                                .center_y()
                                .radius(8.0)
                                .fill(theme.bg)
                                .border(1.0, theme.card_border)
                                .child(
                                    text(state.output_dir().display().to_string())
                                        .font(type_::LABEL)
                                        .color(theme.text_secondary)
                                        .truncate()
                                        .selectable(true),
                                ),
                        )
                        .child(button(
                            "browse-output",
                            "Choose",
                            theme,
                            accent,
                            false,
                            !running,
                            |state, tx| state.choose_output_folder(tx),
                        )),
                )
                .child(
                    text("Rendered MP4 files are written here. The folder is created when needed.")
                        .font(type_::LABEL)
                        .color(theme.text_secondary)
                        .wrap(),
                ),
        )
}

fn source_preview(state: &GuiState, theme: Theme, accent: Color) -> El {
    let content = if let Some(player) = &state.source_player {
        media_player("source-preview", player, theme, accent, 176.0)
    } else {
        let loading = (state.running && state.source_preview_error.is_none())
            || state.source_preview_is_loading();
        col()
            .key("source-preview-placeholder")
            .w_full()
            .h(if state.remote_source_url().is_some() {
                174.0
            } else {
                126.0
            })
            .gap(9.0)
            .center()
            .fill(theme.bg)
            .border(1.0, theme.card_border)
            .radius(9.0)
            .child(if loading {
                loading_indicator("source-preview-loader", "Preparing preview", theme, accent)
            } else {
                el().key("source-preview-poster")
                    .size(54.0, 34.0)
                    .radius(6.0)
                    .fill(theme.control_track)
                    .border(1.0, theme.card_border)
                    .center()
                    .child(text("VIDEO").font(type_::MONO_CAPTION).color(accent))
            })
            .child(
                text(
                    state
                        .source_preview_message()
                        .unwrap_or_else(|| "Preview unavailable.".into()),
                )
                .font(type_::LABEL)
                .color(theme.text_secondary)
                .wrap()
                .max_w(320.0)
                .align(Align::Center),
            )
            .children(
                state
                    .remote_source_url()
                    .filter(|_| !state.running)
                    .map(|_| {
                        row()
                            .key("source-preview-actions")
                            .gap(8.0)
                            .center()
                            .child(button(
                                "download-source-preview",
                                "Download preview",
                                theme,
                                accent,
                                false,
                                !loading,
                                |state, _| state.download_source_preview(),
                            ))
                            .child(button(
                                "open-source-preview",
                                "Open online",
                                theme,
                                accent,
                                false,
                                true,
                                |state, _| state.open_remote_source(),
                            ))
                    }),
            )
    };

    col()
        .key("source-preview-section")
        .w_full()
        .gap(7.0)
        .child(section_label("Source preview"))
        .child(content)
}

fn short_preview(state: &GuiState, index: usize, theme: Theme, accent: Color) -> El {
    let content = if let Some(player) = &state.short_player {
        media_player(
            format!("result-{index}-preview"),
            player,
            theme,
            accent,
            260.0,
        )
    } else {
        let loading = state.short_preview_is_loading();
        col()
            .key(format!("result-{index}-preview-placeholder"))
            .w_full()
            .h(150.0)
            .gap(9.0)
            .center()
            .fill(theme.bg)
            .border(1.0, theme.card_border)
            .radius(9.0)
            .child(if loading {
                loading_indicator(
                    format!("result-{index}-preview-loader"),
                    "Downloading cloud clip",
                    theme,
                    accent,
                )
            } else {
                text(
                    state
                        .short_preview_error
                        .clone()
                        .unwrap_or_else(|| "Preview unavailable.".into()),
                )
                .font(type_::LABEL)
                .color(theme.text_secondary)
                .wrap()
                .max_w(360.0)
                .align(Align::Center)
            })
    };

    col()
        .key(format!("result-{index}-preview-section"))
        .w_full()
        .gap(7.0)
        .child(section_label("Short preview"))
        .child(content)
}

fn media_player(
    key: impl Into<String>,
    player: &Player,
    theme: Theme,
    accent: Color,
    height: f32,
) -> El {
    let key = key.into();
    let playback = player.clone();
    let frame = el()
        .key(format!("{key}-frame"))
        .w_full()
        .h(height)
        .fill(theme.bg)
        .border(1.0, theme.card_border)
        .radius(9.0)
        .child(
            el().key(format!("{key}-video"))
                .w_full()
                .h_full()
                .video(player, SurfaceFit::Fit)
                .on_background_press(move |_: &mut GuiState| toggle_player(&playback)),
        )
        .children(match player.state() {
            PlayerState::Loading => Some(
                el().key(format!("{key}-loading"))
                    .role("status")
                    .label("Loading video preview")
                    .absolute(0.0, 0.0)
                    .w_full()
                    .h_full()
                    .center()
                    .fill(theme.bg.alpha(0.82))
                    .child(loading_indicator(
                        format!("{key}-loader"),
                        "Loading preview",
                        theme,
                        accent,
                    )),
            ),
            PlayerState::Failed(message) => Some(
                el().key(format!("{key}-failed"))
                    .role("alert")
                    .absolute(0.0, 0.0)
                    .w_full()
                    .h_full()
                    .px(18.0)
                    .center()
                    .fill(theme.bg.alpha(0.9))
                    .child(
                        text(message)
                            .font(type_::LABEL)
                            .color(theme.text_secondary)
                            .wrap()
                            .max_w(380.0)
                            .align(Align::Center),
                    ),
            ),
            _ => None,
        });

    col()
        .key(key.clone())
        .w_full()
        .gap(8.0)
        .child(media_controls(&key, player, theme, accent))
        .child(frame)
}

fn media_controls(key: &str, player: &Player, theme: Theme, accent: Color) -> El {
    let duration = player.duration().filter(|duration| *duration > 0.0);
    let time = player.time().max(0.0);
    let fraction = duration.map_or(0.0, |duration| (time / duration).clamp(0.0, 1.0) as f32);
    let play = player.clone();
    let restart = player.clone();
    let mute = player.clone();
    let seek = player.clone();
    let step = player.clone();
    let limit = player.clone();
    let playing = player.is_playing();
    let playable = !matches!(player.state(), PlayerState::Failed(_));

    col()
        .key(format!("{key}-controls"))
        .w_full()
        .gap(6.0)
        .child(
            row()
                .w_full()
                .gap(6.0)
                .center_y()
                .child(button(
                    format!("{key}-play"),
                    if playing { "Pause" } else { "Play" },
                    theme,
                    accent,
                    false,
                    playable,
                    move |_, _| toggle_player(&play),
                ))
                .child(button(
                    format!("{key}-restart"),
                    "Restart",
                    theme,
                    accent,
                    false,
                    playable && duration.is_some(),
                    move |_, _| {
                        restart.seek(0.0);
                        restart.play();
                    },
                ))
                .child(button(
                    format!("{key}-mute"),
                    if player.muted() { "Unmute" } else { "Mute" },
                    theme,
                    accent,
                    false,
                    playable,
                    move |_, _| mute.set_muted(!mute.muted()),
                )),
        )
        .child(
            row()
                .w_full()
                .gap(10.0)
                .center_y()
                .child(
                    el().key(format!("{key}-seek"))
                        .role("slider")
                        .label("Playback position")
                        .range_value(
                            time.min(duration.unwrap_or(0.0)),
                            0.0,
                            duration.unwrap_or(0.0),
                        )
                        .range_step(5.0)
                        .disabled(!playable || duration.is_none())
                        .a11y_value(format!("{} seconds", time.round() as u64))
                        .grow(1.0)
                        .min_w(0.0)
                        .h(16.0)
                        .on_drag_any(move |drag, _| {
                            if drag.phase == rune::Phase::Down {
                                drag.data = seek.time() as f32;
                            }
                            if drag.cancelled() {
                                seek.seek(f64::from(drag.data));
                            } else if let Some(duration) = duration
                                && drag.w > 0.0
                            {
                                seek.seek(duration * f64::from((drag.x / drag.w).clamp(0.0, 1.0)));
                            }
                        })
                        .on_step(move |_: &mut GuiState, steps, _| {
                            if duration.is_some() {
                                step.seek(step.time() + f64::from(steps) * 5.0);
                            }
                        })
                        .on_step_limit(move |_: &mut GuiState, end, _| {
                            if let Some(duration) = duration {
                                limit.seek(if end { duration } else { 0.0 });
                            }
                        })
                        .focusable(playable && duration.is_some())
                        .child(
                            el().key(format!("{key}-seek-track"))
                                .absolute(0.0, 6.0)
                                .w_full()
                                .h(4.0)
                                .radius(2.0)
                                .fill(theme.control_track)
                                .a11y_hidden(),
                        )
                        .child(
                            el().key(format!("{key}-seek-fill"))
                                .absolute(0.0, 6.0)
                                .w_frac(fraction)
                                .h(4.0)
                                .radius(2.0)
                                .fill(accent)
                                .a11y_hidden(),
                        ),
                )
                .child(
                    text(format!(
                        "{} / {}",
                        timestamp(time),
                        duration.map_or_else(|| "--:--".into(), timestamp),
                    ))
                    .key(format!("{key}-time"))
                    .font(type_::MONO_CAPTION)
                    .color(theme.text_secondary),
                ),
        )
}

fn toggle_player(player: &Player) {
    if matches!(player.state(), PlayerState::Failed(_)) {
        return;
    }
    if player.is_playing() {
        player.pause();
    } else {
        player.play();
    }
}

fn spinner(key: impl Into<String>, theme: Theme, accent: Color, label: &'static str) -> El {
    el().key(key)
        .role("progressbar")
        .label(label)
        .size(18.0, 18.0)
        .radius(9.0)
        .border(2.0, theme.control_off)
        .child(
            el().absolute(2.0, 2.0)
                .size(14.0, 14.0)
                .radius(7.0)
                .border_sides([2.2, 0.0, 0.0, 0.0], accent)
                .looping(LoopSpec::rotation(0.9))
                .a11y_hidden(),
        )
}

fn loading_indicator(
    key: impl Into<String>,
    label: &'static str,
    theme: Theme,
    accent: Color,
) -> El {
    let key = key.into();
    let spinner_key = format!("{key}-spinner");
    row()
        .key(key)
        .gap(8.0)
        .center_y()
        .child(spinner(spinner_key, theme, accent, label))
        .child(text(label).font(type_::LABEL).color(theme.text_secondary))
}

fn output(state: &GuiState, theme: Theme, accent: Color, danger: Color) -> El {
    let percent = (state.progress.clamp(0.0, 1.0) * 100.0).round() as u32;
    let tabs = row()
        .key("panel-tabs")
        .role("tablist")
        .label("Workspace panels")
        .focus_group()
        .w_full()
        .padding(4.0)
        .radius(10.0)
        .fill(theme.bg)
        .gap(4.0)
        .child(tab_button(
            "tab-results",
            "Results",
            state.panel == Panel::Results,
            theme,
            |state, _| state.set_panel(Panel::Results),
        ))
        .child(tab_button(
            "tab-activity",
            &format!("Activity ({})", state.logs.len()),
            state.panel == Panel::Activity,
            theme,
            |state, _| state.set_panel(Panel::Activity),
        ));

    col()
        .key("output-panel")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .min_h(0.0)
        .padding(18.0)
        .gap(14.0)
        .fill(theme.card)
        .border(1.0, theme.card_border)
        .radius(16.0)
        .child(
            col()
                .key("generation-status")
                .w_full()
                .padding(14.0)
                .radius(12.0)
                .fill(theme.bg)
                .linear_gradient(0.7, [(0.0, accent.alpha(0.06)), (1.0, Color::TRANSPARENT)])
                .gap(10.0)
                .child(
                    text("PREVIEW & RESULTS")
                        .font(TextStyle::new(10.0, 650.0))
                        .color(accent),
                )
                .child(
                    row()
                        .w_full()
                        .gap(10.0)
                        .center_y()
                        .children(state.running.then(|| {
                            spinner("generation-loader", theme, accent, "Generation in progress")
                        }))
                        .child(
                            text(state.stage.clone())
                                .key("stage")
                                .font(TextStyle::new(17.0, 600.0))
                                .color(theme.text)
                                .truncate()
                                .grow(1.0)
                                .min_w(0.0),
                        )
                        .children((state.running || state.progress > 0.0).then(|| {
                            text(format!("{percent}%"))
                                .key("progress-percent")
                                .font(type_::MONO_CAPTION)
                                .color(accent)
                        })),
                )
                .child(
                    el().key("progress")
                        .role("progressbar")
                        .label("Generation progress")
                        .range_value(state.progress.clamp(0.0, 1.0) as f64, 0.0, 1.0)
                        .w_full()
                        .h(6.0)
                        .radius(3.0)
                        .fill(theme.control_track)
                        .child(
                            el().w_frac(state.progress.clamp(0.0, 1.0))
                                .h_full()
                                .radius(3.0)
                                .fill(accent),
                        ),
                )
                .child(
                    text(state.status_detail())
                        .key("status-detail")
                        .font(type_::LABEL)
                        .color(theme.text_secondary)
                        .wrap(),
                ),
        )
        .children(state.error.as_ref().map(|message| {
            row()
                .key("error-alert")
                .role("alert")
                .w_full()
                .px(10.0)
                .py(8.0)
                .gap(10.0)
                .center_y()
                .radius(8.0)
                .fill(danger.alpha(0.12))
                .border(1.0, danger.alpha(0.4))
                .child(
                    text(message.clone())
                        .font(type_::LABEL)
                        .color(theme.text)
                        .wrap()
                        .grow(1.0)
                        .min_w(0.0),
                )
                .child(button(
                    "dismiss-error",
                    "Dismiss",
                    theme,
                    accent,
                    false,
                    true,
                    |state, _| state.error = None,
                ))
        }))
        .child(tabs)
        .child(
            col()
                .key("panel-scroll")
                .role("region")
                .label(match state.panel {
                    Panel::Results => "Generation results",
                    Panel::Activity => "Generation activity",
                })
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .scroll_edge_fade(24.0)
                .child(match state.panel {
                    Panel::Results => results(state, theme, accent, danger),
                    Panel::Activity => activity(state, theme, accent, danger),
                }),
        )
}

fn results(state: &GuiState, theme: Theme, accent: Color, danger: Color) -> El {
    let Some(result) = &state.result else {
        return col()
            .key("results-empty")
            .w_full()
            .py(20.0)
            .gap(14.0)
            .center()
            .child(
                el().key("empty-poster")
                    .size(184.0, 120.0)
                    .a11y_hidden()
                    .child(
                        el().size(72.0, 90.0)
                            .absolute(4.0, 16.0)
                            .rotate(-0.12)
                            .radius(13.0)
                            .fill(accent.alpha(0.08))
                            .border(1.0, accent.alpha(0.18)),
                    )
                    .child(
                        el().size(72.0, 90.0)
                            .absolute(108.0, 16.0)
                            .rotate(0.12)
                            .radius(13.0)
                            .fill(accent.alpha(0.08))
                            .border(1.0, accent.alpha(0.18)),
                    )
                    .child(
                        col().size(86.0, 112.0)
                            .absolute(49.0, 2.0)
                            .padding(12.0)
                            .gap(8.0)
                            .radius(14.0)
                            .fill(theme.card)
                            .linear_gradient(
                                0.0,
                                [(0.0, accent.alpha(0.16)), (1.0, Color::TRANSPARENT)],
                            )
                            .border(1.0, accent.alpha(0.45))
                            .shadow(8.0, 24.0, Color::BLACK.alpha(0.12))
                            .child(
                                el().w_full().grow(1.0).center().child(
                                    el().size(24.0, 24.0).shape(rune::Shape::new().fill(
                                        vec![[5.0, 3.0], [21.0, 12.0], [5.0, 21.0]], accent,
                                    )),
                                ),
                            )
                            .child(el().w_full().h(4.0).radius(2.0).fill(accent.alpha(0.4)))
                            .child(el().w(40.0).h(4.0).radius(2.0).fill(accent.alpha(0.2))),
                    ),
            )
            .child(
                col().gap(8.0).w_full().items_center()
                    .child(
                        text("Your next great short starts here.")
                            .font(TextStyle::new(21.0, 650.0))
                            .color(theme.text)
                            .wrap()
                            .max_w(360.0)
                            .align(Align::Center),
                    )
                    .child(
                        text("Turn the best moments in your video into hook-first clips. Preview, fine-tune your direction, and export.")
                            .font(type_::BODY)
                            .color(theme.text_secondary)
                            .wrap()
                            .max_w(340.0)
                            .align(Align::Center),
                    ),
            )
            .child(
                row().key("workflow-guide")
                    .w_full()
                    .equal_columns(3)
                    .gap(10.0)
                    .pt(6.0)
                    .children([
                        ("01", "Choose a video"),
                        ("02", "Find strong hooks"),
                        ("03", "Preview & share"),
                    ].map(|(number, label)| {
                        col().gap(7.0).items_center()
                            .child(text(number).font(type_::MONO_CAPTION).color(accent))
                            .child(
                                text(label).font(type_::LABEL)
                                    .color(theme.text_secondary)
                                    .wrap()
                                    .align(Align::Center),
                            )
                    })),
            )
            .children(recent_projects(state, theme, accent));
    };

    col()
        .key("results")
        .w_full()
        .gap(12.0)
        .child(
            row()
                .key("results-summary")
                .w_full()
                .gap(10.0)
                .center_y()
                .child(
                    text(format!(
                        "{} clip{} · {} ready",
                        result.shorts.len(),
                        if result.shorts.len() == 1 { "" } else { "s" },
                        result
                            .shorts
                            .iter()
                            .filter(|short| short.error.is_none() && short.clip_url.is_some())
                            .count()
                    ))
                    .font(TextStyle::new(15.0, 600.0))
                    .color(theme.text)
                    .grow(1.0),
                )
                .child(
                    text(format!("Source {:.0}s", result.transcript.duration))
                        .font(type_::LABEL)
                        .color(theme.text_secondary),
                ),
        )
        .children(
            result
                .shorts
                .iter()
                .enumerate()
                .map(|(index, short)| result_card(state, index, short, theme, accent, danger)),
        )
}

fn clip_trim(state: &GuiState, index: usize, theme: Theme, accent: Color) -> El {
    let busy = state.running || state.exporting_clip.is_some();
    let exporting = state.exporting_clip == Some(index);
    let result = state
        .result
        .as_ref()
        .expect("trim editor requires a selected result");
    let bounds = crate::edit::parse_trim(
        &state.edit_start.text,
        &state.edit_end.text,
        result.transcript.duration,
    );
    let valid = bounds.is_ok();
    let feedback = match bounds {
        Ok((start, end)) => {
            let excerpt = result
                .transcript
                .segments
                .iter()
                .filter(|segment| segment.end > start && segment.start < end)
                .flat_map(|segment| segment.text.split_whitespace())
                .take(61)
                .collect::<Vec<_>>();
            let mut summary = format!("{:.2}s selected", end - start);
            if excerpt.is_empty() {
                summary.push_str(" · No transcript in this range");
            } else {
                summary.push_str(" · ");
                summary.push_str(&excerpt[..excerpt.len().min(60)].join(" "));
                if excerpt.len() > 60 {
                    summary.push('…');
                }
            }
            summary
        }
        Err(error) => error.to_string(),
    };
    col()
        .key(format!("result-{index}-trim"))
        .w_full()
        .gap(8.0)
        .child(section_label("Trim and export this clip"))
        .child(text("Enter seconds or a timestamp such as 1:24.50.").font(type_::LABEL).color(theme.text_secondary).wrap())
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(
                    field(
                        "Start time",
                        line_input(
                            "trim-start",
                            &state.edit_start,
                            "0.00",
                            "Clip start in seconds or clock time",
                            theme,
                            busy,
                            |state| &mut state.edit_start,
                        ),
                    )
                    .grow(1.0)
                    .min_w(0.0),
                )
                .child(
                    field(
                        "End time",
                        line_input(
                            "trim-end",
                            &state.edit_end,
                            "30.00",
                            "Clip end in seconds or clock time",
                            theme,
                            busy,
                            |state| &mut state.edit_end,
                        ),
                    )
                    .grow(1.0)
                    .min_w(0.0),
                ),
        )
        .child(text(feedback).font(type_::LABEL).color(theme.text_secondary).wrap())
        .child(field(
            "Hook text",
            line_input(
                "trim-hook",
                &state.edit_hook,
                "Spoken or on-screen hook",
                "Hook text",
                theme,
                busy,
                |state| &mut state.edit_hook,
            ),
        ))
        .child(field(
            "Background music style (optional)",
            line_input("music-style", &state.music_style, "e.g. mellow lo-fi piano",
                "Gemini background music style", theme, busy, |state| &mut state.music_style),
        ))
        .child(
            button(
                "reexport-selected",
                if exporting {
                    "Exporting..."
                } else if !state.music_style.text.trim().is_empty() {
                    "Generate music and export"
                } else {
                    "Export this clip"
                },
                theme,
                accent,
                true,
                !busy && valid,
                |state, _| state.reexport_selected_clip(),
            )
            .w_full(),
        )
        .child(
            text("Enter a style to generate one paid Gemini music track per export. Leave blank for original audio.")
                .font(type_::LABEL)
                .color(theme.text_secondary)
                .wrap(),
        )
}

fn recent_projects(state: &GuiState, theme: Theme, accent: Color) -> Option<El> {
    if state.recent_projects.is_empty() {
        return None;
    }
    Some(
        col()
            .key("recent-projects")
            .w_full()
            .gap(8.0)
            .pt(8.0)
            .child(section_label("Saved projects"))
            .children(
                state
                    .recent_projects
                    .iter()
                    .take(5)
                    .enumerate()
                    .map(|(index, project)| {
                        let name = std::path::Path::new(&project.source)
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| "Saved project".into());
                        button(
                            format!("recent-project-{index}"),
                            format!("{name} · {} ready", project.ready_clips),
                            theme,
                            accent,
                            false,
                            !state.running,
                            move |state, _| state.open_recent_project(index),
                        )
                        .w_full()
                    }),
            ),
    )
}

fn result_card(
    state: &GuiState,
    index: usize,
    short: &RenderedShort,
    theme: Theme,
    accent: Color,
    danger: Color,
) -> El {
    let highlight = &short.highlight;
    let selected = state.selected_short == Some(index);
    let title = col()
        .key(format!("result-{index}-heading"))
        .grow(1.0)
        .min_w(0.0)
        .gap(4.0)
        .child(
            text(highlight.title.clone())
                .key(format!("result-{index}-title"))
                .font(type_::TITLE)
                .color(theme.text)
                .wrap(),
        )
        .child(
            text(format!(
                "{} – {}  ·  {:.1}s  ·  Score {}/100",
                timestamp(highlight.start_time),
                timestamp(highlight.end_time),
                highlight.duration().max(0.0),
                highlight.score
            ))
            .font(type_::LABEL)
            .color(theme.text_secondary)
            .wrap(),
        );

    let actions = short.clip_url.as_ref().map(|path| {
        let remote = path.contains("://");
        row()
            .key(format!("result-{index}-actions"))
            .w_full()
            .gap(8.0)
            .child(button(
                format!("preview-result-{index}"),
                if state.selected_short == Some(index) {
                    "Hide preview"
                } else {
                    "Preview"
                },
                theme,
                accent,
                selected,
                short.error.is_none(),
                move |state, tx| {
                    state.toggle_short_preview(index);
                    tx.keep_on_screen(format!("preview-result-{index}"));
                },
            ))
            .child(button(
                format!("open-result-{index}"),
                if remote { "Open URL" } else { "Open" },
                theme,
                accent,
                false,
                true,
                {
                    let path = path.clone();
                    move |state, _| state.open_clip(&path)
                },
            ))
            .child(button(
                format!("copy-result-{index}"),
                if remote { "Copy URL" } else { "Copy path" },
                theme,
                accent,
                false,
                true,
                {
                    let path = path.clone();
                    move |state, tx| {
                        tx.copy(path.clone());
                        state.notify(if remote {
                            "Clip URL copied."
                        } else {
                            "Clip path copied."
                        });
                    }
                },
            ))
    });

    col()
        .key(format!("result-{index}"))
        .w_full()
        .padding(16.0)
        .gap(14.0)
        .radius(14.0)
        .fill(theme.bg)
        .border(
            1.0,
            if selected {
                accent.alpha(0.55)
            } else {
                theme.card_border
            },
        )
        .child(
            row()
                .w_full()
                .gap(10.0)
                .center_y()
                .child(
                    el().key(format!("result-{index}-number"))
                        .size(28.0, 28.0)
                        .radius(7.0)
                        .fill(accent.alpha(0.16))
                        .center()
                        .child(
                            text(format!("{:02}", index + 1))
                                .font(type_::MONO_CAPTION)
                                .color(accent),
                        ),
                )
                .child(title),
        )
        .children(actions)
        .children(
            (state.selected_short == Some(index))
                .then(|| short_preview(state, index, theme, accent)),
        )
        .children(short.error.as_ref().map(|error| {
            row()
                .key(format!("result-{index}-error"))
                .w_full()
                .px(10.0)
                .py(8.0)
                .radius(7.0)
                .fill(danger.alpha(0.12))
                .child(
                    text(format!("Clip failed: {error}"))
                        .font(type_::LABEL)
                        .color(theme.text)
                        .wrap(),
                )
        }))
        .child(
            col()
                .key(format!("result-{index}-details"))
                .w_full()
                .gap(5.0)
                .child(
                    text(format!("Hook: {}", highlight.hook_sentence))
                        .font(type_::BODY)
                        .color(theme.text)
                        .wrap()
                        .selectable(true),
                )
                .child(
                    text(highlight.virality_reason.clone())
                        .font(type_::LABEL)
                        .color(theme.text_secondary)
                        .wrap()
                        .selectable(true),
                ),
        )
        .children(
            (selected && short.error.is_none()).then(|| clip_trim(state, index, theme, accent)),
        )
        .children(short.clip_url.as_ref().map(|path| {
            text(
                Path::new(path)
                    .file_name()
                    .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned()),
            )
            .help(path.clone())
            .key(format!("result-{index}-path"))
            .font(type_::MONO_CAPTION)
            .color(theme.text_secondary)
            .truncate()
            .selectable(true)
        }))
}

fn activity(state: &GuiState, theme: Theme, accent: Color, danger: Color) -> El {
    if state.logs.is_empty() {
        return col()
            .key("activity-empty")
            .w_full()
            .py(56.0)
            .gap(8.0)
            .center()
            .child(
                text("No activity yet")
                    .font(TextStyle::new(15.0, 600.0))
                    .color(theme.text),
            )
            .child(
                text("Worker messages and stage updates will appear here while a video is processed.")
                    .font(type_::BODY)
                    .color(theme.text_secondary)
                    .wrap()
                    .max_w(390.0)
                    .align(Align::Center),
            );
    }

    col()
        .key("activity")
        .w_full()
        .gap(10.0)
        .children(state.logs.iter().enumerate().map(|(index, message)| {
            row()
                .key(format!("log-{index}"))
                .w_full()
                .gap(9.0)
                .child(
                    el().key(format!("log-{index}-dot"))
                        .size(6.0, 6.0)
                        .radius(3.0)
                        .mt(5.0)
                        .fill(if message.starts_with("Error:") {
                            danger
                        } else {
                            accent
                        }),
                )
                .child(
                    text(message.clone())
                        .font(type_::MONO_CAPTION)
                        .color(theme.text_secondary)
                        .wrap()
                        .wrap_anywhere()
                        .selectable(true)
                        .grow(1.0)
                        .min_w(0.0),
                )
        }))
}

fn section_label(label: &'static str) -> El {
    text(label).font(TextStyle::new(12.0, 600.0))
}

fn field(label: &'static str, input: El) -> El {
    col()
        .w_full()
        .gap(7.0)
        .child(section_label(label))
        .child(input)
}

fn line_input(
    key: &'static str,
    edit: &TextEdit,
    placeholder: &'static str,
    label: &'static str,
    theme: Theme,
    disabled: bool,
    access: impl for<'a> Fn(&'a mut GuiState) -> &'a mut TextEdit + 'static,
) -> El {
    let mut input = text_input(edit, type_::BODY)
        .key(key)
        .role("textbox")
        .label(label)
        .placeholder(placeholder, theme.text_secondary)
        .color(if disabled {
            theme.text_secondary
        } else {
            theme.text
        })
        .single_line()
        .w_full()
        .min_w(0.0);
    if disabled {
        input = input
            .disabled(true)
            .read_only(true)
            .on_input_paste(|state: &mut GuiState, _, _| state.running)
            .on_input_key(|state: &mut GuiState, _, _| state.running);
    } else {
        input = input
            .on_edit(access)
            .on_input_change(|state: &mut GuiState, _| state.clear_feedback());
    }
    row()
        .key(format!("{key}-field"))
        .w_full()
        .h(38.0)
        .px(10.0)
        .radius(8.0)
        .fill(theme.bg)
        .border(1.0, theme.card_border)
        .center_y()
        .child(input)
}

fn prompt_input(state: &GuiState, theme: Theme, disabled: bool) -> El {
    let mut input = text_input(&state.prompt, type_::BODY)
        .key("prompt-input")
        .role("textbox")
        .label("Creative direction")
        .placeholder(
            "e.g. Focus on practical tips, a fast pace, and a curious audience.",
            theme.text_secondary,
        )
        .color(if disabled {
            theme.text_secondary
        } else {
            theme.text
        })
        .w_full();
    if disabled {
        input = input
            .disabled(true)
            .read_only(true)
            .on_input_paste(|state: &mut GuiState, _, _| state.running)
            .on_input_key(|state: &mut GuiState, _, _| state.running);
    } else {
        input = input
            .on_edit(|state: &mut GuiState| &mut state.prompt)
            .on_input_change(|state: &mut GuiState, _| state.clear_feedback())
            .on_submit(|state: &mut GuiState, _| {
                state.prompt.insert("\n");
            });
    }
    col()
        .key("prompt-field")
        .w_full()
        .h(96.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .px(10.0)
        .py(8.0)
        .radius(8.0)
        .fill(theme.bg)
        .border(1.0, theme.card_border)
        .child(input)
}

fn button(
    key: impl Into<String>,
    label: impl Into<String>,
    theme: Theme,
    accent: Color,
    primary: bool,
    enabled: bool,
    on_press: impl Fn(&mut GuiState, &mut Tx) + 'static,
) -> El {
    let key = key.into();
    let label = label.into();
    let mut button = el()
        .key(key)
        .role("button")
        .label(label.clone())
        .disabled(!enabled)
        .h(36.0)
        .px(13.0)
        .radius(8.0)
        .motion(motion::SNAPPY)
        .center()
        .border(
            1.0,
            if primary {
                accent.alpha(if enabled { 1.0 } else { 0.35 })
            } else {
                theme.card_border
            },
        )
        .fill(if primary {
            accent.alpha(if enabled { 1.0 } else { 0.35 })
        } else if enabled {
            theme.control_track
        } else {
            theme.control_off
        })
        .child(text(label).font(type_::LABEL).color(if primary && enabled {
            Color::WHITE
        } else if enabled {
            theme.text
        } else {
            theme.text_secondary
        }));
    if enabled {
        button = button
            .focusable(true)
            .cursor(rune::Cursor::Pointer)
            .hover(if primary {
                accent.alpha(0.9)
            } else {
                theme.hover
            })
            .press(if primary {
                accent.alpha(0.8)
            } else {
                theme.press
            })
            .on_press_tx(on_press);
    } else {
        button = button.disabled(true);
    }
    button
}

fn segmented(
    key: &'static str,
    labels: &[&str],
    selected: usize,
    theme: Theme,
    accent: Color,
    disabled: bool,
    on_select: impl Fn(&mut GuiState, usize, &mut Tx) + 'static,
) -> El {
    let on_select = Rc::new(on_select);
    row()
        .key(format!("{key}-group"))
        .role("radiogroup")
        .focus_group()
        .label(key)
        .disabled(disabled)
        .w_full()
        .h(32.0)
        .padding(3.0)
        .gap(3.0)
        .equal_columns(labels.len())
        .radius(8.0)
        .fill(theme.control_track)
        .border(1.0, theme.card_border)
        .children(labels.iter().enumerate().map(|(index, label)| {
            let is_selected = index == selected;
            let mut option = el()
                .key(format!("{key}-{index}"))
                .role("radio")
                .label(*label)
                .checked(is_selected)
                .focusable(!disabled)
                .focus_selected(is_selected)
                .disabled(disabled)
                .grow(1.0)
                .min_w(0.0)
                .h_full()
                .radius(6.0)
                .center()
                .fill(if is_selected {
                    theme.indicator
                } else {
                    Color::TRANSPARENT
                })
                .border(
                    1.0,
                    if is_selected {
                        accent.alpha(if disabled { 0.2 } else { 0.55 })
                    } else {
                        Color::TRANSPARENT
                    },
                )
                .child(
                    text(*label)
                        .font(type_::LABEL)
                        .color(if is_selected {
                            theme.text
                        } else {
                            theme.text_secondary
                        })
                        .truncate(),
                );
            if !disabled {
                let on_select = Rc::clone(&on_select);
                option = option.on_press_tx(move |state, tx| on_select(state, index, tx));
            }
            option
        }))
}

fn tab_button(
    key: &'static str,
    label: &str,
    selected: bool,
    theme: Theme,
    on_press: impl Fn(&mut GuiState, &mut Tx) + 'static,
) -> El {
    el().key(key)
        .role("tab")
        .label(label)
        .checked(selected)
        .focusable(true)
        .focus_selected(selected)
        .grow(1.0)
        .h(34.0)
        .radius(7.0)
        .center()
        .fill(if selected {
            theme.control_off
        } else {
            Color::TRANSPARENT
        })
        .hover(theme.hover)
        .press(theme.press)
        .on_press_tx(on_press)
        .child(text(label).font(type_::LABEL).color(if selected {
            theme.text
        } else {
            theme.text_secondary
        }))
}

fn aspect_index(aspect: AspectChoice) -> usize {
    match aspect {
        AspectChoice::Portrait => 0,
        AspectChoice::Square => 1,
        AspectChoice::Landscape => 2,
    }
}

fn quality_index(format: u32) -> usize {
    [720, 1080, 1440, 2160]
        .iter()
        .position(|value| *value == format)
        .unwrap_or(1)
}

fn analysis_index(format: u32) -> usize {
    [360, 480, 720, 1080]
        .iter()
        .position(|value| *value == format)
        .unwrap_or(0)
}

fn timestamp(seconds: f64) -> String {
    let seconds = if seconds.is_finite() {
        seconds.max(0.0).round() as u64
    } else {
        0
    };
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}
