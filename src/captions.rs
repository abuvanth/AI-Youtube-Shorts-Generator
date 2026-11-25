//! Timed captions and optional text hooks rendered as ASS/SRT.
//!
//! Karaoke styling highlights the active phrase. Word-by-word `\k` timing is not
//! invented: the transcript stores phrase cues, not aligned words.

use std::{collections::HashSet, sync::OnceLock};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::models::{Highlight, Transcript};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptionPreset {
    Clean,
    Bold,
    Karaoke,
    Podcast,
    Gaming,
    Cinematic,
}

impl CaptionPreset {
    pub const ALL: [Self; 6] = [
        Self::Clean,
        Self::Bold,
        Self::Karaoke,
        Self::Podcast,
        Self::Gaming,
        Self::Cinematic,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Clean => "Clean",
            Self::Bold => "Bold",
            Self::Karaoke => "Karaoke",
            Self::Podcast => "Podcast",
            Self::Gaming => "Gaming",
            Self::Cinematic => "Cinema",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Bold => "bold",
            Self::Karaoke => "karaoke",
            Self::Podcast => "podcast",
            Self::Gaming => "gaming",
            Self::Cinematic => "cinematic",
        }
    }
}

impl std::str::FromStr for CaptionPreset {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "clean" | "minimal" => Ok(Self::Clean),
            "bold" | "creator" => Ok(Self::Bold),
            "karaoke" => Ok(Self::Karaoke),
            "podcast" => Ok(Self::Podcast),
            "gaming" | "game" => Ok(Self::Gaming),
            "cinematic" | "cinema" => Ok(Self::Cinematic),
            _ => Err(
                "caption preset must be clean, bold, karaoke, podcast, gaming, or cinematic".into(),
            ),
        }
    }
}

impl std::fmt::Display for CaptionPreset {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaptionCue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaptionPlan {
    pub ass: String,
    pub srt: String,
    pub cue_count: usize,
    pub burned_hook: bool,
}

struct AssLook {
    font_ratio: f64,
    primary: &'static str,
    secondary: &'static str,
    outline_colour: &'static str,
    back: &'static str,
    bold: i32,
    italic: i32,
    border_style: i32,
    outline: f64,
    shadow: f64,
    margin_v_ratio: f64,
}

impl CaptionPreset {
    fn look(self) -> AssLook {
        match self {
            Self::Clean => AssLook {
                font_ratio: 0.042,
                primary: "&H00FFFFFF",
                secondary: "&H000000FF",
                outline_colour: "&H00000000",
                back: "&H64000000",
                bold: 0,
                italic: 0,
                border_style: 1,
                outline: 2.0,
                shadow: 0.0,
                margin_v_ratio: 0.11,
            },
            Self::Bold => AssLook {
                font_ratio: 0.056,
                primary: "&H00FFFFFF",
                secondary: "&H000000FF",
                outline_colour: "&H00000000",
                back: "&H00000000",
                bold: -1,
                italic: 0,
                border_style: 1,
                outline: 4.5,
                shadow: 1.0,
                margin_v_ratio: 0.13,
            },
            Self::Karaoke => AssLook {
                font_ratio: 0.050,
                primary: "&H00FFFFFF",
                secondary: "&H0000D7FF",
                outline_colour: "&H00000000",
                back: "&H00000000",
                bold: -1,
                italic: 0,
                border_style: 1,
                outline: 3.5,
                shadow: 0.0,
                margin_v_ratio: 0.14,
            },
            Self::Podcast => AssLook {
                font_ratio: 0.040,
                primary: "&H00FFFFFF",
                secondary: "&H000000FF",
                outline_colour: "&H00000000",
                back: "&HAA101010",
                bold: 0,
                italic: 0,
                border_style: 3,
                outline: 0.0,
                shadow: 0.0,
                margin_v_ratio: 0.12,
            },
            Self::Gaming => AssLook {
                font_ratio: 0.052,
                primary: "&H0046FFB0",
                secondary: "&H000000FF",
                outline_colour: "&H00000000",
                back: "&H00000000",
                bold: -1,
                italic: 0,
                border_style: 1,
                outline: 4.0,
                shadow: 1.5,
                margin_v_ratio: 0.16,
            },
            Self::Cinematic => AssLook {
                font_ratio: 0.038,
                primary: "&H00F4F4F4",
                secondary: "&H000000FF",
                outline_colour: "&H00000000",
                back: "&H32000000",
                bold: 0,
                italic: -1,
                border_style: 1,
                outline: 1.6,
                shadow: 0.5,
                margin_v_ratio: 0.10,
            },
        }
    }
}

pub fn plan_captions(
    transcript: &Transcript,
    highlight: &Highlight,
    preset: CaptionPreset,
    width: u32,
    height: u32,
    burn_captions: bool,
    text_hook: bool,
) -> Option<CaptionPlan> {
    if width == 0 || height == 0 || highlight.duration() <= 0.0 {
        return None;
    }
    let cues = if burn_captions {
        cues_for_range(transcript, highlight.start_time, highlight.end_time)
    } else {
        Vec::new()
    };
    let hook = text_hook
        .then(|| highlight.hook_sentence.trim())
        .filter(|hook| !hook.is_empty())
        .filter(|hook| hook_is_grounded(hook, transcript, highlight.start_time, highlight.end_time))
        .map(str::to_owned);
    if cues.is_empty() && hook.is_none() {
        return None;
    }
    let burned_hook = hook.is_some();
    let ass = ass_document(
        &cues,
        hook.as_deref(),
        preset,
        width,
        height,
        highlight.duration(),
    );
    let srt = srt_document(&cues);
    Some(CaptionPlan {
        ass,
        srt,
        cue_count: cues.len(),
        burned_hook,
    })
}

pub fn cues_for_range(transcript: &Transcript, start: f64, end: f64) -> Vec<CaptionCue> {
    if !start.is_finite() || !end.is_finite() || end <= start {
        return Vec::new();
    }
    let mut cues = Vec::new();
    for segment in &transcript.segments {
        let text = segment
            .text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if text.is_empty() || !segment.start.is_finite() || !segment.end.is_finite() {
            continue;
        }
        let overlap_start = start.max(segment.start);
        let overlap_end = end.min(segment.end);
        if overlap_end - overlap_start <= 0.05 {
            continue;
        }
        let cue_start = round_millis((overlap_start - start).max(0.0));
        let cue_end = round_millis((overlap_end - start).min(end - start));
        if cue_end - cue_start <= 0.04 {
            continue;
        }
        cues.push(CaptionCue {
            start: cue_start,
            end: cue_end,
            text,
        });
    }
    cues
}

pub fn hook_is_grounded(hook: &str, transcript: &Transcript, start: f64, end: f64) -> bool {
    let hook_words = tokens(hook);
    if hook_words.is_empty() {
        return false;
    }
    let mut spoken = HashSet::new();
    for segment in &transcript.segments {
        let overlap = end.min(segment.end) - start.max(segment.start);
        if overlap.is_finite() && overlap > 0.05 {
            spoken.extend(tokens(&segment.text));
        }
    }
    let matches = hook_words
        .iter()
        .filter(|word| spoken.contains(*word))
        .count();
    matches as f64 / hook_words.len() as f64 >= 0.5
}

pub fn srt_document(cues: &[CaptionCue]) -> String {
    let mut output = String::new();
    for (index, cue) in cues.iter().enumerate() {
        output.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            index + 1,
            srt_time(cue.start),
            srt_time(cue.end),
            cue.text
        ));
    }
    output
}

fn ass_document(
    cues: &[CaptionCue],
    hook: Option<&str>,
    preset: CaptionPreset,
    width: u32,
    height: u32,
    clip_duration: f64,
) -> String {
    let look = preset.look();
    let margin_x = ((width as f64) * 0.08).round().max(24.0) as u32;
    let margin_v = ((height as f64) * look.margin_v_ratio).round().max(36.0) as u32;
    let font_size = ((height as f64) * look.font_ratio)
        .round()
        .clamp(18.0, 160.0) as u32;
    let max_chars = (width as usize / 38).clamp(16, 42);
    let mut events = String::new();
    if let Some(hook) = hook {
        let hook_end = round_millis((clip_duration * 0.34).clamp(1.2, 2.8).min(clip_duration));
        events.push_str(&format!(
            "Dialogue: 1,{},{},Hook,,0,0,0,,{}\n",
            ass_time(0.0),
            ass_time(hook_end),
            wrap_ass(hook, max_chars.saturating_sub(4).max(12))
        ));
    }
    for cue in cues {
        events.push_str(&format!(
            "Dialogue: 0,{},{},Caption,,0,0,0,,{}\n",
            ass_time(cue.start),
            ass_time(cue.end),
            wrap_ass(&cue.text, max_chars)
        ));
    }
    let hook_size = ((height as f64) * 0.046).round().clamp(18.0, 140.0) as u32;
    let hook_margin = ((height as f64) * 0.08).round().max(36.0) as u32;
    format!(
        "\u{feff}[Script Info]\n\
         ; preset: {preset}\n\
         ; Font is Nirmala UI so Tamil, Hindi, and Latin can share one Windows face.\n\
         ; Karaoke events are phrase cues. Word timestamps are not synthesized.\n\
         ScriptType: v4.00+\n\
         PlayResX: {width}\n\
         PlayResY: {height}\n\
         WrapStyle: 0\n\
         ScaledBorderAndShadow: yes\n\n\
         [V4+ Styles]\n\
         Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: Caption,Nirmala UI,{font_size},{primary},{secondary},{outline_colour},{back},{bold},{italic},0,0,100,100,0,0,{border_style},{outline_width:.1},{shadow:.1},2,{margin_x},{margin_x},{margin_v},1\n\
         Style: Hook,Nirmala UI,{hook_size},&H00FFFFFF,&H000000FF,&H00000000,&HCC101010,-1,0,0,0,100,100,0,0,3,0.0,0.0,8,{margin_x},{margin_x},{hook_margin},1\n\n\
         [Events]\n\
         Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
         {events}",
        preset = preset.name(),
        primary = look.primary,
        secondary = look.secondary,
        outline_colour = look.outline_colour,
        back = look.back,
        bold = look.bold,
        italic = look.italic,
        border_style = look.border_style,
        outline_width = look.outline,
        shadow = look.shadow,
    )
}

fn wrap_ass(text: &str, max_chars: usize) -> String {
    let words = text.split_whitespace().collect::<Vec<_>>();
    if words.is_empty() {
        return String::new();
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in words {
        if current.is_empty() {
            current.push_str(word);
            continue;
        }
        if current.chars().count() + 1 + word.chars().count() > max_chars {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        } else {
            current.push(' ');
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
        .into_iter()
        .map(|line| ass_escape(&line))
        .collect::<Vec<_>>()
        .join(r"\N")
}

fn ass_escape(text: &str) -> String {
    let mut escaped = String::new();
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str(r"\\"),
            '{' => escaped.push_str(r"\{"),
            '}' => escaped.push_str(r"\}"),
            character if character.is_control() => {}
            character => escaped.push(character),
        }
    }
    escaped
}

fn round_millis(value: f64) -> f64 {
    (value.max(0.0) * 1000.0).round() / 1000.0
}

fn ass_time(seconds: f64) -> String {
    let centiseconds = (seconds.max(0.0) * 100.0).round() as u64;
    let hours = centiseconds / 360_000;
    let minutes = (centiseconds / 6_000) % 60;
    let secs = (centiseconds / 100) % 60;
    let cents = centiseconds % 100;
    format!("{hours}:{minutes:02}:{secs:02}.{cents:02}")
}

fn srt_time(seconds: f64) -> String {
    let millis = (seconds.max(0.0) * 1000.0).round() as u64;
    let hours = millis / 3_600_000;
    let minutes = (millis / 60_000) % 60;
    let secs = (millis / 1_000) % 60;
    let ms = millis % 1_000;
    format!("{hours:02}:{minutes:02}:{secs:02},{ms:03}")
}

fn tokens(value: &str) -> Vec<String> {
    static WORD_REGEX: OnceLock<Regex> = OnceLock::new();
    WORD_REGEX
        .get_or_init(|| Regex::new(r"\w+").expect("token pattern is valid"))
        .find_iter(&value.to_lowercase())
        .map(|value| value.as_str().to_string())
        .filter(|token| token.chars().count() > 1)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Segment;

    fn sample() -> Transcript {
        Transcript {
            duration: 40.0,
            segments: vec![
                Segment {
                    start: 2.0,
                    end: 6.0,
                    text: "தமிழ் caption starts the thought".into(),
                },
                Segment {
                    start: 6.0,
                    end: 9.5,
                    text: "हिन्दी caption continues it".into(),
                },
                Segment {
                    start: 20.0,
                    end: 24.0,
                    text: "This part is outside the clip".into(),
                },
            ],
        }
    }

    fn clip() -> Highlight {
        Highlight {
            title: "Clip".into(),
            start_time: 3.0,
            end_time: 10.0,
            score: 80,
            hook_sentence: "தமிழ் caption starts".into(),
            virality_reason: "Grounded opening".into(),
            category: "hook".into(),
        }
    }

    #[test]
    fn cues_are_shifted_into_the_clip_and_keep_unicode() {
        let cues = cues_for_range(&sample(), 3.0, 10.0);
        assert_eq!(cues.len(), 2);
        assert!((cues[0].start - 0.0).abs() < 0.001);
        assert!((cues[0].end - 3.0).abs() < 0.001);
        assert!(cues[0].text.contains("தமிழ்"));
        assert!(cues[1].text.contains("हिन्दी"));
        assert!((cues[1].start - 3.0).abs() < 0.001);
        assert!((cues[1].end - 6.5).abs() < 0.001);
    }

    #[test]
    fn fabricated_hooks_are_rejected_and_grounded_hooks_pass() {
        let transcript = sample();
        assert!(hook_is_grounded(
            "தமிழ் caption starts",
            &transcript,
            3.0,
            10.0
        ));
        assert!(!hook_is_grounded(
            "I made a million dollars overnight",
            &transcript,
            3.0,
            10.0
        ));
        assert!(!hook_is_grounded(
            "தமிழ் caption starts",
            &transcript,
            20.0,
            30.0
        ));
    }

    #[test]
    fn ass_and_srt_escape_text_and_record_the_preset() {
        let plan = plan_captions(
            &sample(),
            &clip(),
            CaptionPreset::Bold,
            1080,
            1920,
            true,
            true,
        )
        .unwrap();
        assert!(plan.burned_hook);
        assert_eq!(plan.cue_count, 2);
        assert!(plan.ass.contains("; preset: bold"));
        assert!(plan.ass.contains("Nirmala UI"));
        assert!(plan.ass.contains("தமிழ்"));
        assert!(plan.ass.contains("हिन्दी"));
        assert!(plan.ass.contains("Style: Hook"));
        assert!(plan.srt.contains("தமிழ் caption starts the thought"));
        assert!(plan.srt.contains("00:00:00,000 --> 00:00:03,000"));

        let mut braced = clip();
        braced.hook_sentence = "starts {the} caption".into();
        let transcript = Transcript {
            duration: 40.0,
            segments: vec![Segment {
                start: 3.0,
                end: 8.0,
                text: "starts {the} caption now".into(),
            }],
        };
        let escaped = plan_captions(
            &transcript,
            &braced,
            CaptionPreset::Clean,
            1080,
            1920,
            false,
            true,
        )
        .unwrap();
        assert!(escaped.ass.contains(r"starts \{the\} caption"));
        assert!(escaped.srt.is_empty());
        let gaming = plan_captions(
            &sample(),
            &clip(),
            CaptionPreset::Gaming,
            1080,
            1920,
            true,
            false,
        )
        .unwrap();
        assert!(!gaming.ass.contains("; preset: bold"));
    }

    #[test]
    fn presets_parse_and_differ() {
        assert_eq!(
            "cinema".parse::<CaptionPreset>().unwrap(),
            CaptionPreset::Cinematic
        );
        let bold = plan_captions(
            &sample(),
            &clip(),
            CaptionPreset::Bold,
            720,
            1280,
            true,
            false,
        )
        .unwrap();
        let podcast = plan_captions(
            &sample(),
            &clip(),
            CaptionPreset::Podcast,
            720,
            1280,
            true,
            false,
        )
        .unwrap();
        assert!(bold.ass.contains("; preset: bold"));
        assert!(podcast.ass.contains("; preset: podcast"));
        assert!(podcast.ass.contains(",3,"));
        assert!(!bold.burned_hook);
    }
}
