//! Native Windows desktop interface for the Rust generation pipeline.

#![forbid(unsafe_code)]

mod state;
mod view;

#[cfg(test)]
mod tests;

use rune::{AppSpec, WindowHandle};

use state::GuiState;

const MIN_SIZE: (f32, f32) = (940.0, 640.0);
const TASKBAR: f64 = 48.0;
const FRAME_BORDER: f64 = 8.0;

pub fn run() {
    let mut spec = AppSpec::new(
        "AI YouTube Shorts Generator",
        (1180.0, 800.0),
        GuiState::new,
        view::view,
    );
    spec.min_size = Some(MIN_SIZE);
    spec.poll = Some(GuiState::poll);
    spec.on_key = Some(GuiState::on_key);
    spec.setup = Some(GuiState::setup);
    rune::App::new(spec).on_window(fit_window_to_display).run();
}

/// Runs before the first frame, so the window never opens with its footer off-screen on a
/// display shorter than the requested height. The position Windows reports at creation is a
/// placeholder, so the window is placed explicitly in the space above a bottom taskbar.
fn fit_window_to_display(window: &WindowHandle) {
    let Some(placement) = window.placement() else {
        return;
    };
    let Some(((display_x, display_y), (display_width, display_height))) = placement.display else {
        return;
    };
    let scale = placement.scale;
    let chrome_top = f64::from(placement.content_position.1 - placement.position.1).max(0.0);
    let (width, current) = (
        placement.content_size.0 as f64 / scale,
        placement.content_size.1 as f64 / scale,
    );
    let height = fitting_height(f64::from(display_height), chrome_top, scale)
        .max(MIN_SIZE.1)
        .min(current as f32);
    if f64::from(height) < current {
        window.resize_instant((width as f32, height));
    }

    let frame = FRAME_BORDER * scale;
    let outer_width = width * scale + 2.0 * frame;
    let outer_height = f64::from(height) * scale + chrome_top + frame;
    let free = display_height as f64 - TASKBAR * scale - outer_height;
    window.set_position((
        display_x + ((f64::from(display_width) - outer_width) / 2.0).max(0.0) as i32,
        display_y + (free / 2.0).max(0.0) as i32,
    ));
}

/// The logical content height that keeps the window's bottom edge above a taskbar at the
/// bottom of the display, from physical-pixel values.
fn fitting_height(display_height: f64, chrome_top: f64, scale: f64) -> f32 {
    let physical = display_height - (TASKBAR + FRAME_BORDER) * scale - chrome_top;
    (physical / scale).floor() as f32
}
