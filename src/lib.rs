pub mod captions;
pub mod clip;
pub mod config;
#[cfg(all(feature = "gui", target_os = "windows"))]
pub(crate) mod credentials;
pub mod download;
pub mod edit;
#[cfg(all(feature = "gui", target_os = "windows"))]
pub mod gui;
pub mod highlights;
pub mod llm;
pub mod models;
pub mod muapi;
#[cfg(all(feature = "gui", target_os = "windows"))]
mod music;
pub mod pipeline;
pub mod progress;
pub mod projects;
pub mod transcribe;

pub use config::Config;
pub use pipeline::{GenerateOptions, generate, generate_with};
pub use progress::ProgressEvent;
