//! Explicit example upgrades, independent of the host and its running sessions.

use serde_json::{json, Value};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const NAMES: [&str; 3] = ["clock", "system-status", "pane-pwd"];

fn error(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn regular_or_missing(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => Ok(()),
        Ok(_) => Err(error(
            "refusing to replace a symlink or non-regular destination",
        )),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| error("destination has no parent"))?;
    let temp = parent.join(format!(".among-install-{}", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| {
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    // Do not remove a temporary file we did not create.
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub fn install(
    built: &Path,
    plugins: &Path,
    config: &Path,
    names: &[&str],
    enable_new: bool,
) -> io::Result<PathBuf> {
    if names.is_empty() || names.iter().any(|name| !NAMES.contains(name)) {
        return Err(error("select known example plugin names"));
    }
    regular_or_missing(config)?;
    let old_config = match fs::read(config) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(err),
    };
    let mut settings: Value = match old_config.as_ref() {
        Some(bytes) if enable_new => {
            serde_json::from_slice(bytes).map_err(|e| error(&e.to_string()))?
        }
        _ => json!({"plugins":[]}),
    };
    if enable_new {
        let object = settings
            .as_object_mut()
            .ok_or_else(|| error("configuration must be an object"))?;
        let list = object
            .entry("plugins")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| error("plugins must be an array"))?;
        for name in names {
            if !list
                .iter()
                .any(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
            {
                list.push(json!({"name":name,"enabled":false}));
            }
        }
    }
    let mut binaries = Vec::new();
    for name in names {
        regular_or_missing(&plugins.join(name))?;
        let mut magic = [0u8; 4];
        fs::File::open(built.join(name))?.read_exact(&mut magic)?;
        if magic != *b"\x7fELF" {
            return Err(error("built plugin is not an ELF executable"));
        }
        binaries.push((*name, fs::read(built.join(name))?));
    }
    fs::create_dir_all(plugins)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| error(&e.to_string()))?
        .as_nanos();
    let backup = plugins
        .parent()
        .ok_or_else(|| error("plugin directory has no parent"))?
        .join("plugin-backups")
        .join(format!("{stamp}-{}", std::process::id()));
    fs::create_dir_all(&backup)?;
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o700))?;
    if let Some(bytes) = old_config {
        atomic_write(&backup.join("plugins.json"), &bytes, 0o600)?;
    }
    for (name, _) in &binaries {
        if plugins.join(name).exists() {
            fs::copy(plugins.join(name), backup.join(name))?;
        }
    }
    for (name, bytes) in binaries {
        atomic_write(&plugins.join(name), &bytes, 0o755)?;
    }
    if enable_new {
        fs::create_dir_all(
            config
                .parent()
                .ok_or_else(|| error("config has no parent"))?,
        )?;
        let bytes = serde_json::to_vec_pretty(&settings).map_err(|e| error(&e.to_string()))?;
        atomic_write(config, &bytes, 0o600)?;
    }
    Ok(backup)
}
