use among_example_plugins::install::{install, NAMES};
use std::path::PathBuf;
use std::process::ExitCode;

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut enable_new = false;
    let mut names = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--enable-new" => enable_new = true,
            "--help" => {
                println!("Usage: sh scripts/install-plugins.sh [--enable-new] [clock system-status pane-pwd]\nNo names selects all three. --enable-new adds missing configuration entries, all disabled by default. Enable wanted plugins in Preferences. Existing entries are preserved. Restart plugins separately.");
                return Ok(());
            }
            _ if NAMES.contains(&arg.as_str()) => names.push(arg),
            _ => return Err(format!("unknown option or plugin: {arg}").into()),
        }
    }
    if names.is_empty() {
        names = NAMES.iter().map(|name| name.to_string()).collect();
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&home).join(".local/share"));
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&home).join(".config"))
        .join("among/plugins.json");
    let executable = std::env::current_exe()?;
    let built = executable
        .parent()
        .and_then(|path| path.parent())
        .ok_or("cannot locate release binaries")?;
    let plugins = data.join("among/plugins");
    let selected: Vec<&str> = names.iter().map(String::as_str).collect();
    let backup = install(built, &plugins, &config, &selected, enable_new)?;
    println!("Installed: {}\nPlugin directory: {}\nBackup: {}\nExisting entries were preserved. Disable/re-enable running plugins; new entries load on the next host startup.", names.join(", "),plugins.display(),backup.display());
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Plugin installation failed: {error}");
            ExitCode::FAILURE
        }
    }
}
