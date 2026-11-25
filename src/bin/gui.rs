#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
fn main() {
    shorts_generator::gui::run();
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!(
        "The Rune GUI is available on Windows. Use shorts-generator for the cross-platform CLI."
    );
}
