//! Keyboard-driven window hints for Hyprland Lua sessions.
mod app;
mod geometry;
mod hints;
mod hyprctl;
mod layout;
mod model;
mod overlay;
mod render;
mod session;

/// Display hints and focus the selected window.
///
/// # Errors
/// Returns an error when the desktop session, rendering, or control operation fails.
pub fn run() -> anyhow::Result<()> {
    app::run()
}
