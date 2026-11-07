#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod control;
#[cfg(windows)]
mod fonts;
#[cfg(windows)]
mod ipc;
#[cfg(windows)]
mod layout;
#[cfg(windows)]
mod lock;
#[cfg(windows)]
mod session;

#[cfg(windows)]
use anyhow::Context;
#[cfg(windows)]
use clap::Parser;
#[cfg(windows)]
use std::path::PathBuf;
#[cfg(windows)]
use std::sync::Mutex;

#[cfg(windows)]
#[derive(Parser)]
#[command(name = "chibipop-settings", version)]
struct Args {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    dict: Option<PathBuf>,
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config_path = args
        .config
        .unwrap_or_else(|| chibipop::paths::beside_exe("chibipop.toml"));
    let dict_path = args
        .dict
        .unwrap_or_else(|| chibipop::paths::data_file(r"data\chibipop.sqlite"));
    tauri::Builder::default()
        .manage(Mutex::new(session::Session::new(config_path, dict_path)))
        .invoke_handler(tauri::generate_handler![
            ipc::load_layout,
            ipc::load_form,
            ipc::font_families,
            ipc::save_form,
            ipc::apply_form,
            ipc::anki_refresh,
            ipc::capture_chord,
            ipc::cancel_capture,
            ipc::new_bind,
            ipc::validate_form,
        ])
        .run(tauri::generate_context!())
        .context("running the settings window")
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("chibipop-settings: this binary only runs on Windows; Linux uses `chibipop settings`");
    std::process::ExitCode::FAILURE
}
