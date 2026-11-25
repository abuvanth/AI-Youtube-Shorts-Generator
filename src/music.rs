use std::{io::Write, time::Duration};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

fn audio_bytes(response: &Value) -> Result<Vec<u8>> {
    let encoded = response["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|step| step["type"] == "model_output")
        .flat_map(|step| step["content"].as_array().into_iter().flatten())
        .find(|part| part["type"] == "audio")
        .and_then(|part| part["data"].as_str())
        .filter(|data| !data.is_empty())
        .context("Gemini returned no music. Try a different music description.")?;
    STANDARD
        .decode(encoded)
        .context("Gemini returned invalid audio data")
}

pub(crate) fn generate(key: &str, style: &str) -> Result<tempfile::NamedTempFile> {
    let response = reqwest::blocking::Client::builder().timeout(Duration::from_secs(180)).build()?
        .post("https://generativelanguage.googleapis.com/v1beta/interactions")
        .header("x-goog-api-key", key)
        .json(&json!({"model":"lyria-3-clip-preview","input":format!("Create a loopable instrumental background track for a spoken video. Style: {style}. Keep it understated, with no vocals or lyrics.")}))
        .send().context("Could not reach the Gemini music API")?;
    if !response.status().is_success() {
        bail!(
            "Gemini music request failed (HTTP {}). Check the Gemini key, paid-tier access and quota.",
            response.status()
        );
    }
    let bytes = audio_bytes(&response.json()?)?;
    let mut file = tempfile::Builder::new().suffix(".mp3").tempfile()?;
    file.write_all(&bytes)?;
    file.flush()?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_model_audio_and_rejects_empty_responses() {
        let response = json!({"steps":[{"type":"model_output","content":[{"type":"text"},{"type":"audio","data":"AQID"}]}]});
        assert_eq!(audio_bytes(&response).unwrap(), vec![1, 2, 3]);
        assert!(audio_bytes(&json!({})).is_err());
    }
}
