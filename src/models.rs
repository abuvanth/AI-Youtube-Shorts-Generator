use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Local,
    Api,
    Auto,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub duration: f64,
    pub segments: Vec<Segment>,
}

impl Transcript {
    pub fn text(&self) -> String {
        self.segments
            .iter()
            .map(|segment| segment.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn from_text(text: &str, duration: f64) -> Self {
        Self {
            duration,
            segments: if text.trim().is_empty() {
                vec![Segment {
                    start: 0.0,
                    end: duration,
                    text: "Visual-only content; inspect the video to identify spoken or on-screen details."
                        .into(),
                }]
            } else {
                text.lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| Segment {
                        start: 0.0,
                        end: duration,
                        text: line.trim().to_string(),
                    })
                    .collect()
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Highlight {
    pub title: String,
    pub start_time: f64,
    pub end_time: f64,
    pub score: i64,
    pub hook_sentence: String,
    pub virality_reason: String,
    /// Model label such as hook, emotional, opinion, revelation, conflict, quotable, story, or practical.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub category: String,
}

impl Highlight {
    pub fn duration(&self) -> f64 {
        self.end_time - self.start_time
    }

    pub fn start_ms(&self) -> i64 {
        (self.start_time * 1000.0).round() as i64
    }

    pub fn end_ms(&self) -> i64 {
        (self.end_time * 1000.0).round() as i64
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderedShort {
    #[serde(flatten)]
    pub highlight: Highlight,
    pub clip_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationResult {
    pub mode: Mode,
    pub source_video_url: String,
    pub transcript: Transcript,
    pub highlights: Vec<Highlight>,
    pub shorts: Vec<RenderedShort>,
}
