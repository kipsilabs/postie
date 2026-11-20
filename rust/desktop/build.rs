fn main() {
    // app commands are reachable from the loopback page only through the permissions generated here
    let commands = &[
        "select_folder",
        "select_folders",
        "select_files",
        "add_files",
        "upload_folders",
        "select_config_directory",
        "download_nzb",
        "get_config_path",
    ];
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(commands)),
    )
    .expect("failed to run tauri build");
}
