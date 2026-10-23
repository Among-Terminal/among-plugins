use among_example_plugins::install::{install, NAMES};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "among-install-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::create_dir(path.join("built")).unwrap();
        for name in NAMES {
            std::fs::write(path.join("built").join(name), b"\x7fELFtest-fixture").unwrap();
        }
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn script_replacement_backs_up_and_preserves_user_configuration() {
    use std::os::unix::fs::PermissionsExt;
    let temp = Temp::new();
    let plugins = temp.0.join("data/among/plugins");
    let config = temp.0.join("config/among/plugins.json");
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(plugins.join("clock"), b"#!/bin/sh\nexit 99\n").unwrap();
    let original = json!({"extra":"keep", "plugins":[{"name":"clock","enabled":false,"custom":7},{"name":"custom-plugin","enabled":true}]});
    let bytes = serde_json::to_vec(&original).unwrap();
    std::fs::write(&config, &bytes).unwrap();
    let backup = install(&temp.0.join("built"), &plugins, &config, &NAMES, true).unwrap();
    assert_eq!(std::fs::read(backup.join("plugins.json")).unwrap(), bytes);
    assert_eq!(
        std::fs::read(backup.join("clock")).unwrap(),
        b"#!/bin/sh\nexit 99\n"
    );
    let settings: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(settings["extra"], "keep");
    assert_eq!(settings["plugins"][0], original["plugins"][0]);
    assert_eq!(settings["plugins"][1], original["plugins"][1]);
    assert_eq!(settings["plugins"].as_array().unwrap().len(), 4);
    let entries = settings["plugins"].as_array().unwrap();
    for name in ["clock", "system-status", "pane-pwd"] {
        assert_eq!(
            entries.iter().find(|entry| entry["name"] == name).unwrap()["enabled"],
            false
        );
    }
    for name in NAMES {
        assert_eq!(
            std::fs::read(plugins.join(name)).unwrap(),
            b"\x7fELFtest-fixture"
        );
        assert_eq!(
            std::fs::metadata(plugins.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
}

#[test]
fn no_enable_leaves_configuration_bytes_untouched() {
    let temp = Temp::new();
    let config = temp.0.join("plugins.json");
    let bytes = b"{\n \"plugins\":[] , \"keep\": true}\n";
    std::fs::write(&config, bytes).unwrap();
    install(
        &temp.0.join("built"),
        &temp.0.join("data/plugins"),
        &config,
        &["clock"],
        false,
    )
    .unwrap();
    assert_eq!(std::fs::read(&config).unwrap(), bytes);
}

#[test]
fn invalid_config_missing_binary_and_symlink_fail_before_replacement() {
    let temp = Temp::new();
    let config = temp.0.join("plugins.json");
    let plugins = temp.0.join("data/plugins");
    std::fs::write(&config, b"broken").unwrap();
    assert!(install(&temp.0.join("built"), &plugins, &config, &NAMES, true).is_err());
    assert!(!plugins.exists());
    std::fs::write(&config, b"{\"plugins\":[]}").unwrap();
    std::fs::remove_file(temp.0.join("built/clock")).unwrap();
    assert!(install(&temp.0.join("built"), &plugins, &config, &NAMES, true).is_err());
    assert!(!plugins.exists());
    std::fs::create_dir_all(&plugins).unwrap();
    std::os::unix::fs::symlink(&config, plugins.join("system-status")).unwrap();
    assert!(install(
        &temp.0.join("built"),
        &plugins,
        &config,
        &["system-status"],
        true
    )
    .is_err());
    assert_eq!(std::fs::read(&config).unwrap(), b"{\"plugins\":[]}");
}
