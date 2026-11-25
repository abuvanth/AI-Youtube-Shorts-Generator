use std::{path::PathBuf, time::Duration};

use crate::models::GenerationResult;

#[derive(Debug, Clone)]
pub enum ProgressEvent {
    Log(String),
    SourceReady(PathBuf),
    Stage { label: String, fraction: f32 },
    Finished(GenerationResult),
    Failed(String),
}

impl ProgressEvent {
    pub fn stage(label: impl Into<String>, fraction: f32) -> Self {
        Self::Stage {
            label: label.into(),
            fraction: fraction.clamp(0.0, 1.0),
        }
    }
}

pub fn exponential_backoff(attempt: u32, base: Duration, maximum: Duration) -> Duration {
    base.saturating_mul(2_u32.saturating_pow(attempt))
        .min(maximum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_bounded() {
        assert_eq!(
            exponential_backoff(0, Duration::from_secs(1), Duration::from_secs(8)),
            Duration::from_secs(1)
        );
        assert_eq!(
            exponential_backoff(2, Duration::from_secs(1), Duration::from_secs(8)),
            Duration::from_secs(4)
        );
        assert_eq!(
            exponential_backoff(10, Duration::from_secs(1), Duration::from_secs(8)),
            Duration::from_secs(8)
        );
    }
}
