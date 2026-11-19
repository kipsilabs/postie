//! Port of `pkg/fileinfo`.

/// A file to process, with its size and the path used for subject generation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileInfo {
    pub path: String,
    pub size: u64,
    /// Path relative to the root folder for subject generation (e.g. `MyFolder/subfolder/file.mp4`).
    pub relative_path: String,
}
