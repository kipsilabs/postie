//! Port of `internal/backend`: the application service shared by the Tauri commands and the web API.
//! Grows by area: paths and config location first, then the App state, config, queue and processor.

pub mod config_change;
pub mod config_location;
pub mod paths;
