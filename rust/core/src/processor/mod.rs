//! Port of `internal/processor`. Grows unit by unit: slot runner, path helpers, provider
//! availability and the script-retry worker first; the processor proper follows `pkg/postie`.

pub mod availability;
pub mod postcheck_retry;
pub mod script_retry;
pub mod slots;
pub mod utils;
