#[cfg(test)]
use std::{cell::RefCell, collections::HashMap, rc::Rc};

use anyhow::{Context, Result, bail};

use crate::config::{AsrProvider, Config, WhisperModelSize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ApiKey {
    Gemini,
    ElevenLabs,
    OpenAi,
    MuApi,
}

impl ApiKey {
    pub(crate) const ALL: [Self; 4] = [Self::Gemini, Self::ElevenLabs, Self::OpenAi, Self::MuApi];

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Gemini => 0,
            Self::ElevenLabs => 1,
            Self::OpenAi => 2,
            Self::MuApi => 3,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Gemini => "Gemini API key",
            Self::ElevenLabs => "ElevenLabs API key",
            Self::OpenAi => "OpenAI API key",
            Self::MuApi => "MuAPI API key",
        }
    }

    pub(crate) const fn input_name(self) -> &'static str {
        match self {
            Self::Gemini => "gemini-key-input",
            Self::ElevenLabs => "elevenlabs-key-input",
            Self::OpenAi => "openai-key-input",
            Self::MuApi => "muapi-key-input",
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::Gemini => "Required when Gemini is selected for video analysis.",
            Self::ElevenLabs => "Required when Scribe is selected for transcription.",
            Self::OpenAi => "Optional with Gemini. Required when LLM_PROVIDER=openai is selected.",
            Self::MuApi => "Optional for local generation. Required for MuAPI cloud rendering.",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Gemini => "GEMINI_API_KEY",
            Self::ElevenLabs => "ELEVENLABS_API_KEY",
            Self::OpenAi => "OPENAI_API_KEY",
            Self::MuApi => "MUAPI_API_KEY",
        }
    }

    pub(crate) fn get(self, config: &Config) -> Option<&str> {
        let value = match self {
            Self::Gemini => config.gemini_api_key.as_deref(),
            Self::ElevenLabs => config.elevenlabs_api_key.as_deref(),
            Self::OpenAi => config.openai_api_key.as_deref(),
            Self::MuApi => config.muapi_api_key.as_deref(),
        };
        value.map(str::trim).filter(|value| !value.is_empty())
    }

    pub(crate) fn set(self, config: &mut Config, value: Option<String>) {
        let value = value
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        match self {
            Self::Gemini => config.gemini_api_key = value,
            Self::ElevenLabs => config.elevenlabs_api_key = value,
            Self::OpenAi => config.openai_api_key = value,
            Self::MuApi => config.muapi_api_key = value,
        }
    }

    pub(crate) fn validate(self, config: &Config) -> Result<()> {
        match self {
            Self::Gemini => {
                config.gemini_api_key()?;
            }
            Self::ElevenLabs => {
                config.elevenlabs_api_key()?;
            }
            Self::OpenAi => {
                config.openai_api_key()?;
            }
            Self::MuApi => {
                config.muapi_api_key()?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CredentialKey {
    Api(ApiKey),
    OnboardingComplete,
    AsrProvider,
    LocalWhisperModel,
}

impl CredentialKey {
    fn label(self) -> &'static str {
        match self {
            Self::Api(key) => key.label(),
            Self::OnboardingComplete => "onboarding status",
            Self::AsrProvider => "transcription provider",
            Self::LocalWhisperModel => "local Whisper model",
        }
    }

    fn target_name(self) -> String {
        match self {
            Self::Api(key) => format!("AI-YouTube-Shorts-Generator/{}", key.name()),
            Self::OnboardingComplete => "AI-YouTube-Shorts-Generator/onboarding-complete".into(),
            Self::AsrProvider => "AI-YouTube-Shorts-Generator/asr-provider".into(),
            Self::LocalWhisperModel => "AI-YouTube-Shorts-Generator/local-whisper-model".into(),
        }
    }
}

pub(crate) trait CredentialStore {
    fn read(&self, key: CredentialKey) -> Result<Option<String>>;
    fn write(&self, key: CredentialKey, value: &str) -> Result<()>;
}

#[derive(Default)]
pub(crate) struct StoredCredentials {
    values: [Option<String>; 4],
    pub(crate) onboarding_complete: bool,
    pub(crate) asr_provider: Option<AsrProvider>,
    pub(crate) local_whisper_model: Option<WhisperModelSize>,
}

impl StoredCredentials {
    pub(crate) fn load(store: &dyn CredentialStore) -> Result<Self> {
        let mut values: [Option<String>; 4] = std::array::from_fn(|_| None);
        for key in ApiKey::ALL {
            values[key.index()] = store
                .read(CredentialKey::Api(key))
                .with_context(|| format!("Could not read {}", key.label()))?;
        }
        let onboarding_complete = store
            .read(CredentialKey::OnboardingComplete)
            .context("Could not read onboarding status")?
            .is_some_and(|value| value == "complete");
        let asr_provider = store
            .read(CredentialKey::AsrProvider)
            .context("Could not read transcription provider")?
            .and_then(|value| AsrProvider::parse(&value).ok());
        let local_whisper_model = store
            .read(CredentialKey::LocalWhisperModel)
            .context("Could not read local Whisper model")?
            .and_then(|value| WhisperModelSize::parse(&value));
        Ok(Self {
            values,
            onboarding_complete,
            asr_provider,
            local_whisper_model,
        })
    }

    pub(crate) fn apply_to(&self, config: &mut Config) {
        for key in ApiKey::ALL {
            if let Some(value) = &self.values[key.index()] {
                key.set(config, Some(value.clone()));
            }
        }
        if let Some(provider) = self.asr_provider {
            config.asr_provider = provider;
        }
        if let Some(model) = self.local_whisper_model {
            config.local_whisper_model = model.name().into();
        }
    }
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct MemoryCredentialStore(Rc<RefCell<HashMap<CredentialKey, String>>>);

#[cfg(test)]
impl CredentialStore for MemoryCredentialStore {
    fn read(&self, key: CredentialKey) -> Result<Option<String>> {
        Ok(self.0.borrow().get(&key).cloned())
    }

    fn write(&self, key: CredentialKey, value: &str) -> Result<()> {
        self.0.borrow_mut().insert(key, value.to_owned());
        Ok(())
    }
}

#[cfg(target_os = "windows")]
pub(crate) struct WindowsCredentialStore;

#[cfg(target_os = "windows")]
impl WindowsCredentialStore {
    pub(crate) const fn new() -> Self {
        Self
    }
}

#[cfg(target_os = "windows")]
impl CredentialStore for WindowsCredentialStore {
    fn read(&self, key: CredentialKey) -> Result<Option<String>> {
        use std::{ffi::c_void, io};

        use windows_sys::Win32::{
            Foundation::ERROR_NOT_FOUND,
            Security::Credentials::{
                CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
            },
        };

        let target = wide(&key.target_name());
        let mut raw = std::ptr::null_mut::<CREDENTIALW>();
        let succeeded = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut raw) };
        if succeeded == 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
                Ok(None)
            } else {
                Err(error).with_context(|| {
                    format!(
                        "Could not access {} in Windows Credential Manager",
                        key.label()
                    )
                })
            };
        }
        if raw.is_null() {
            bail!("Windows Credential Manager returned an empty credential");
        }

        let result = unsafe {
            let credential = &*raw;
            if credential.CredentialBlobSize > CRED_MAX_CREDENTIAL_BLOB_SIZE {
                Err(anyhow::anyhow!("Stored credential is too large"))
            } else if credential.CredentialBlobSize == 0 {
                Ok(String::new())
            } else if credential.CredentialBlob.is_null() {
                Err(anyhow::anyhow!("Stored credential has no data"))
            } else {
                let bytes = std::slice::from_raw_parts_mut(
                    credential.CredentialBlob,
                    credential.CredentialBlobSize as usize,
                );
                let value = String::from_utf8(bytes.to_vec())
                    .context("Stored credential is not valid UTF-8");
                bytes.fill(0);
                value
            }
        };
        unsafe { CredFree(raw.cast::<c_void>()) };
        result.map(Some)
    }

    fn write(&self, key: CredentialKey, value: &str) -> Result<()> {
        use std::io;

        use windows_sys::Win32::Security::Credentials::{
            CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
            CREDENTIALW, CredWriteW,
        };

        if value.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
            bail!("{} is too long for Windows Credential Manager", key.label());
        }
        let mut target = wide(&key.target_name());
        let mut blob = value.as_bytes().to_vec();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: if blob.is_empty() {
                std::ptr::null_mut()
            } else {
                blob.as_mut_ptr()
            },
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        let succeeded = unsafe { CredWriteW(&credential, 0) };
        blob.fill(0);
        if succeeded == 0 {
            Err(io::Error::last_os_error()).with_context(|| {
                format!(
                    "Could not save {} in Windows Credential Manager",
                    key.label()
                )
            })
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "windows")]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trips_keys_and_onboarding_status() {
        let store = MemoryCredentialStore::default();
        store
            .write(CredentialKey::Api(ApiKey::Gemini), "gemini-fixture-key")
            .unwrap();
        store
            .write(CredentialKey::OnboardingComplete, "complete")
            .unwrap();
        store
            .write(CredentialKey::AsrProvider, AsrProvider::Whisper.name())
            .unwrap();
        store
            .write(
                CredentialKey::LocalWhisperModel,
                WhisperModelSize::Large.name(),
            )
            .unwrap();

        let stored = StoredCredentials::load(&store).unwrap();
        assert!(stored.onboarding_complete);
        assert_eq!(stored.asr_provider, Some(AsrProvider::Whisper));
        assert_eq!(stored.local_whisper_model, Some(WhisperModelSize::Large));
        let mut config = Config::default();
        stored.apply_to(&mut config);
        assert_eq!(ApiKey::Gemini.get(&config), Some("gemini-fixture-key"));
        assert_eq!(ApiKey::MuApi.get(&config), None);
        assert_eq!(config.asr_provider, AsrProvider::Whisper);
        assert_eq!(config.local_whisper_model, "large-v3");
    }

    #[test]
    fn saved_blank_values_override_environment_fallbacks() {
        let store = MemoryCredentialStore::default();
        store.write(CredentialKey::Api(ApiKey::Gemini), "").unwrap();
        let stored = StoredCredentials::load(&store).unwrap();
        let mut config = Config {
            gemini_api_key: Some("environment-fixture-key".into()),
            ..Default::default()
        };

        stored.apply_to(&mut config);

        assert_eq!(ApiKey::Gemini.get(&config), None);
    }
}
