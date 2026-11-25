use std::fs;

use anyhow::Result;
use clap::Parser;
use shorts_generator::{
    config::Config,
    models::{GenerationResult, Mode},
    pipeline::{GenerateOptions, generate_with},
    progress::ProgressEvent,
};

#[derive(Debug, Parser)]
#[command(
    name = "shorts-generator",
    version,
    about = "Generate YouTube Shorts with AI"
)]
struct Cli {
    #[command(flatten)]
    options: GenerateOptions,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("FAILED: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let output_json = cli.options.output_json.clone();
    let config = Config::from_env()?;
    let result = generate_with(cli.options, &config, |event| match event {
        ProgressEvent::Log(message) => eprintln!("{message}"),
        ProgressEvent::Stage { label, fraction } => {
            eprintln!("[{:>3.0}%] {label}", fraction * 100.0)
        }
        ProgressEvent::SourceReady(_) | ProgressEvent::Finished(_) | ProgressEvent::Failed(_) => {}
    })?;
    let json = serde_json::to_string_pretty(&result)?;
    if let Some(path) = output_json {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, format!("{json}\n"))?;
        println!(
            "{}\nFull JSON written to {}",
            format_result(&result),
            path.display()
        );
    } else {
        println!("{}", format_result(&result));
    }
    Ok(())
}

fn format_result(result: &GenerationResult) -> String {
    let mode = match result.mode {
        Mode::Local => "local",
        Mode::Api => "api",
        Mode::Auto => "auto",
    };
    let mut output = format!(
        "\n{}\nMode:          {}\nSource video:  {}\nHighlights:    {} candidates → kept top {}\n{}\n",
        "=".repeat(72),
        mode,
        result.source_video_url,
        result.highlights.len(),
        result.shorts.len(),
        "=".repeat(72),
    );
    for (index, short) in result.shorts.iter().enumerate() {
        output.push_str(&format!(
            "\n#{}  score={}  {:.1}s → {:.1}s\n     title:  {}\n     hook:   {}\n",
            index + 1,
            short.highlight.score,
            short.highlight.start_time,
            short.highlight.end_time,
            short.highlight.title,
            short.highlight.hook_sentence,
        ));
        if let Some(url) = &short.clip_url {
            output.push_str(&format!("     clip:   {url}\n"));
        } else {
            output.push_str(&format!(
                "     clip:   FAILED ({})\n",
                short.error.as_deref().unwrap_or("unknown error")
            ));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use shorts_generator::models::{Highlight, RenderedShort, Transcript};

    #[test]
    fn cli_prints_the_python_compatible_summary() {
        let highlight = Highlight {
            title: "A strong hook".into(),
            start_time: 5.0,
            end_time: 35.0,
            score: 91,
            hook_sentence: "This changes everything.".into(),
            virality_reason: "Clear payoff.".into(),
            category: String::new(),
        };
        let result = GenerationResult {
            mode: Mode::Api,
            source_video_url: "https://cdn.example/source.mp4".into(),
            transcript: Transcript::default(),
            highlights: vec![highlight.clone()],
            shorts: vec![RenderedShort {
                highlight,
                clip_url: Some("https://cdn.example/clip.mp4".into()),
                error: None,
            }],
        };
        let summary = format_result(&result);
        assert!(summary.contains("Mode:          api"));
        assert!(summary.contains("1 candidates → kept top 1"));
        assert!(summary.contains("title:  A strong hook"));
        assert!(summary.contains("clip:   https://cdn.example/clip.mp4"));
    }
}
