use crate::{
    archive::{self, Files, Member},
    package, process,
    source::Collector,
    Result,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

const RELEASE: &str = "1.98.1";
const PREFIX: &str = "THIRD_PARTY/RUST/";
const REQUIRED_NOTICES: [&str; 9] = [
    "COPYRIGHT-library.html",
    "LICENSE-APACHE",
    "LICENSE-MIT",
    "SOURCE.json",
    "rust-src/library/compiler-builtins/LICENSE.txt",
    "rust-src/library/compiler-builtins/libm/LICENSE.txt",
    "rust-src/library/Cargo.lock",
    "rust-src/src/llvm-project/libunwind/LICENSE.TXT",
    "rust-src/library/std/src/sys/sync/mutex/fuchsia.rs",
];
const REQUIRED_COMPONENTS: [&str; 5] = [
    "components.txt",
    "rust-std-manifest.txt",
    "rustc-manifest.txt",
    "rust-src-manifest.txt",
    "channel-manifest.toml",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub release: String,
    pub commit: String,
    pub host: String,
    pub compiler_sha256: String,
    pub components: Vec<Member>,
    pub runtime_libraries: Vec<Member>,
    pub common_notices: Vec<Member>,
}

pub struct Inputs {
    pub record: Record,
    pub common: Files,
    pub native: Files,
}

fn host(architecture: &str) -> Result<&'static str> {
    match architecture {
        "amd64" => Ok("x86_64-unknown-linux-gnu"),
        "arm64" => Ok("aarch64-unknown-linux-gnu"),
        _ => Err("unsupported Rust runtime architecture".into()),
    }
}

fn compiler_version(text: &str, architecture: &str) -> Result<(String, String)> {
    let field = |key: &str| -> Result<&str> {
        let mut values = text.lines().filter_map(|line| line.strip_prefix(key));
        let value = values.next().ok_or("missing compiler provenance field")?;
        if values.next().is_some() {
            return Err("duplicate compiler provenance field".into());
        }
        Ok(value)
    };
    let commit = field("commit-hash: ")?;
    let observed_host = field("host: ")?;
    if field("release: ")? != RELEASE
        || observed_host != host(architecture)?
        || !package::hex(commit, 40)
    {
        return Err("compiler release, commit or native host mismatch".into());
    }
    Ok((commit.into(), observed_host.into()))
}

fn member(path: String, bytes: &[u8]) -> Member {
    Member {
        path,
        size: bytes.len() as u64,
        sha256: archive::hash(bytes),
        executable: false,
    }
}

fn notice_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "license",
        "licence",
        "copying",
        "copyright",
        "notice",
        "unlicense",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

fn source_notices(
    root: &Path,
    relative: &str,
    collector: &mut Collector,
    entries: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > 20 {
        return Err("Rust source-notice depth exceeded".into());
    }
    for entry in fs::read_dir(root)? {
        *entries = entries
            .checked_add(1)
            .filter(|count| *count <= 65536)
            .ok_or("Rust source-notice traversal exceeded")?;
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "non-UTF-8 Rust source path")?;
        let path = format!("{relative}/{name}");
        let kind = entry.file_type()?;
        if kind.is_dir() {
            source_notices(&entry.path(), &path, collector, entries, depth + 1)?;
        } else if kind.is_file() && notice_name(&name) {
            collector.file(format!("{PREFIX}rust-src/{path}"), &entry.path())?;
        } else if !kind.is_file() {
            return Err("linked or special Rust source-notice input".into());
        }
    }
    Ok(())
}

pub fn collect(architecture: &str) -> Result<Inputs> {
    let version = process::text(Command::new("rustup").args([
        "run",
        RELEASE,
        "rustc",
        "--version",
        "--verbose",
    ]))?;
    let (commit, host) = compiler_version(&version, architecture)?;
    let sysroot = process::text(
        Command::new("rustup").args(["run", RELEASE, "rustc", "--print", "sysroot"]),
    )?;
    let sysroot = Path::new(sysroot.trim());
    if !sysroot.is_absolute() {
        return Err("Rust sysroot is not absolute".into());
    }
    let mut common = Collector::new(512, 16 * 1024 * 1024);
    common.file(
        format!("{PREFIX}COPYRIGHT-library.html"),
        &sysroot.join("share/doc/rust/COPYRIGHT-library.html"),
    )?;
    let static_files = sysroot.join("share/doc/rust/html/static.files");
    for (prefix, output) in [
        ("LICENSE-APACHE-", "LICENSE-APACHE"),
        ("LICENSE-MIT-", "LICENSE-MIT"),
    ] {
        let mut matching = Vec::new();
        for (index, entry) in fs::read_dir(&static_files)?.enumerate() {
            if index >= 4096 {
                return Err("Rust documentation directory limit exceeded".into());
            }
            let entry = entry?;
            let name = entry.file_name();
            if name
                .to_str()
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".txt"))
            {
                matching.push(entry.path());
            }
        }
        if matching.len() != 1 {
            return Err("Rust documentation licence text missing or ambiguous".into());
        }
        common.file(format!("{PREFIX}{output}"), &matching[0])?;
    }
    let rust_source = sysroot.join("lib/rustlib/src/rust");
    source_notices(
        &rust_source.join("library"),
        "library",
        &mut common,
        &mut 0,
        0,
    )?;
    for path in [
        "library/compiler-builtins/LICENSE.txt",
        "library/compiler-builtins/libm/LICENSE.txt",
        "library/Cargo.lock",
        "src/llvm-project/libunwind/LICENSE.TXT",
        "library/std/src/sys/sync/mutex/fuchsia.rs",
    ] {
        let target = format!("{PREFIX}rust-src/{path}");
        if !common.files.contains_key(&target) {
            common.file(target, &rust_source.join(path))?;
        }
    }
    let common_record = json!({"release":RELEASE,"commit":commit,"source":"https://github.com/rust-lang/rust","source_path":"library","licence_scope":"standard-library report plus conservatively retained rust-src licence texts, compiler-builtins/libm and libunwind; not a per-function link proof; no compiler or OS binary is redistributed"});
    common.insert(
        format!("{PREFIX}SOURCE.json"),
        serde_json::to_vec_pretty(&common_record)?,
    )?;

    let mut native = Collector::new(8, 16 * 1024 * 1024);
    for (source, target) in [
        ("components".to_owned(), "components.txt"),
        (format!("manifest-rust-std-{host}"), "rust-std-manifest.txt"),
        (format!("manifest-rustc-{host}"), "rustc-manifest.txt"),
        ("manifest-rust-src".to_owned(), "rust-src-manifest.txt"),
        (
            "multirust-channel-manifest.toml".to_owned(),
            "channel-manifest.toml",
        ),
    ] {
        native.file(
            format!("{PREFIX}native/{target}"),
            &sysroot.join("lib/rustlib").join(source),
        )?;
    }
    let component_text = std::str::from_utf8(
        &native
            .files
            .get(&format!("{PREFIX}native/components.txt"))
            .ok_or("missing component list")?
            .0,
    )?;
    if !component_text
        .lines()
        .any(|name| name == format!("rust-std-{host}"))
        || !component_text.lines().any(|name| name == "rust-src")
    {
        return Err("installed Rust component provenance lacks std or source".into());
    }
    let mut libraries = Vec::new();
    for (index, entry) in fs::read_dir(sysroot.join(format!("lib/rustlib/{host}/lib")))?.enumerate()
    {
        if index >= 4096 {
            return Err("Rust library directory limit exceeded".into());
        }
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "non-UTF-8 runtime library name")?;
        if name.ends_with(".rlib") {
            if libraries.len() >= 128 {
                return Err("Rust runtime library count exceeded".into());
            }
            let bytes = archive::read(&entry.path(), archive::MAX_FILE)?;
            libraries.push(member(format!("lib/rustlib/{host}/lib/{name}"), &bytes));
        }
    }
    libraries.sort_by(|left, right| left.path.cmp(&right.path));
    for name in ["libstd-", "libcore-", "liballoc-", "libcompiler_builtins-"] {
        if libraries
            .iter()
            .filter(|library| {
                library
                    .path
                    .rsplit('/')
                    .next()
                    .is_some_and(|file| file.starts_with(name))
            })
            .count()
            != 1
        {
            return Err("Rust standard/runtime library input missing or ambiguous".into());
        }
    }
    let record = Record {
        release: RELEASE.into(),
        commit,
        host,
        compiler_sha256: archive::hash(&archive::read(
            &sysroot.join("bin/rustc"),
            archive::MAX_FILE,
        )?),
        components: native
            .files
            .iter()
            .map(|(path, (bytes, _))| member(path.clone(), bytes))
            .collect(),
        runtime_libraries: libraries,
        common_notices: common
            .files
            .iter()
            .map(|(path, (bytes, _))| member(path.clone(), bytes))
            .collect(),
    };
    native.insert(
        format!("{PREFIX}native/BUILD.json"),
        serde_json::to_vec_pretty(&record)?,
    )?;
    Ok(Inputs {
        record,
        common: common.files,
        native: native.files,
    })
}

pub fn verify(
    record: &Record,
    architecture: &str,
    files: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    if record.release != RELEASE
        || record.host != host(architecture)?
        || !package::hex(&record.commit, 40)
        || !package::hex(&record.compiler_sha256, 64)
        || record.runtime_libraries.is_empty()
        || record.runtime_libraries.len() > 128
        || record.common_notices.is_empty()
        || record.common_notices.len() > 512
        || record.components.len() != 5
    {
        return Err("invalid Rust runtime provenance".into());
    }
    let mut paths = BTreeSet::new();
    for declared in record.common_notices.iter().chain(&record.components) {
        if !declared.path.starts_with(PREFIX)
            || declared.executable
            || !paths.insert(&declared.path)
        {
            return Err("invalid Rust notice member".into());
        }
        let bytes = files
            .get(&declared.path)
            .ok_or("required Rust notice missing from package")?;
        if bytes.len() as u64 != declared.size || archive::hash(bytes) != declared.sha256 {
            return Err("Rust notice digest mismatch".into());
        }
    }
    for name in REQUIRED_NOTICES {
        let path = format!("{PREFIX}{name}");
        if !record
            .common_notices
            .iter()
            .any(|member| member.path == path)
        {
            return Err("required Rust runtime licence or source provenance missing".into());
        }
    }
    for name in REQUIRED_COMPONENTS {
        let path = format!("{PREFIX}native/{name}");
        if !record.components.iter().any(|member| member.path == path) {
            return Err("required Rust component input missing".into());
        }
    }
    let origin: serde_json::Value = serde_json::from_slice(
        files
            .get(&format!("{PREFIX}SOURCE.json"))
            .ok_or("missing Rust source provenance")?,
    )?;
    if origin["release"] != record.release
        || origin["commit"] != record.commit
        || origin["source"] != "https://github.com/rust-lang/rust"
    {
        return Err("Rust source provenance and compiler record disagree".into());
    }
    let mut runtime_paths = BTreeSet::new();
    for library in &record.runtime_libraries {
        if !library
            .path
            .starts_with(&format!("lib/rustlib/{}/lib/lib", record.host))
            || !library.path.ends_with(".rlib")
            || !archive::relative(&library.path)
            || library.executable
            || library.size == 0
            || library.size > archive::MAX_FILE
            || !package::hex(&library.sha256, 64)
            || !runtime_paths.insert(&library.path)
        {
            return Err("invalid Rust runtime library input record".into());
        }
    }
    for name in ["libstd-", "libcore-", "liballoc-", "libcompiler_builtins-"] {
        if record
            .runtime_libraries
            .iter()
            .filter(|library| {
                library
                    .path
                    .rsplit('/')
                    .next()
                    .is_some_and(|file| file.starts_with(name))
            })
            .count()
            != 1
        {
            return Err("required Rust runtime library record missing or ambiguous".into());
        }
    }
    let actual: Record = serde_json::from_slice(
        files
            .get(&format!("{PREFIX}native/BUILD.json"))
            .ok_or("missing native Rust receipt")?,
    )?;
    if &actual != record {
        return Err("Rust build report and packaged receipt disagree".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compiler_provenance_refuses_wrong_release_host_or_missing_commit() {
        let valid = format!(
            "release: {RELEASE}\nhost: x86_64-unknown-linux-gnu\ncommit-hash: {}\n",
            "a".repeat(40)
        );
        assert!(compiler_version(&valid, "amd64").is_ok());
        assert!(compiler_version(&valid, "arm64").is_err());
        assert!(compiler_version(&valid.replace(RELEASE, "1.98.0"), "amd64").is_err());
        assert!(compiler_version(&valid.replace(&"a".repeat(40), "unknown"), "amd64").is_err());
    }

    #[test]
    fn missing_or_modified_packaged_runtime_notices_refuse_verification() {
        let commit = "a".repeat(40);
        let mut files = std::collections::BTreeMap::new();
        for name in REQUIRED_NOTICES {
            files.insert(
                format!("{PREFIX}{name}"),
                b"synthetic licence test input".to_vec(),
            );
        }
        files.insert(format!("{PREFIX}SOURCE.json"), serde_json::to_vec(&json!({"release":RELEASE,"commit":commit,"source":"https://github.com/rust-lang/rust"})).unwrap());
        let common_notices = files
            .iter()
            .map(|(path, bytes)| member(path.clone(), bytes))
            .collect();
        let mut components = Vec::new();
        for name in REQUIRED_COMPONENTS {
            let path = format!("{PREFIX}native/{name}");
            files.insert(path.clone(), b"synthetic component test input".to_vec());
            components.push(member(path, b"synthetic component test input"));
        }
        let runtime_libraries = ["std", "core", "alloc", "compiler_builtins"]
            .iter()
            .map(|name| {
                member(
                    format!("lib/rustlib/x86_64-unknown-linux-gnu/lib/lib{name}-fixture.rlib"),
                    b"x",
                )
            })
            .collect();
        let record = Record {
            release: RELEASE.into(),
            commit,
            host: "x86_64-unknown-linux-gnu".into(),
            compiler_sha256: "b".repeat(64),
            components,
            runtime_libraries,
            common_notices,
        };
        files.insert(
            format!("{PREFIX}native/BUILD.json"),
            serde_json::to_vec(&record).unwrap(),
        );
        assert!(verify(&record, "amd64", &files).is_ok());
        assert!(verify(&record, "arm64", &files).is_err());
        let path = format!("{PREFIX}COPYRIGHT-library.html");
        let saved = files.remove(&path).unwrap();
        assert!(verify(&record, "amd64", &files).is_err());
        files.insert(path, b"changed".to_vec());
        assert!(verify(&record, "amd64", &files).is_err());
        files.insert(format!("{PREFIX}COPYRIGHT-library.html"), saved);
        let mut changed = record.clone();
        changed.runtime_libraries.pop();
        assert!(verify(&changed, "amd64", &files).is_err());
    }

    #[test]
    fn retained_modified_extraction_files_carry_prominent_notices() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let historical = fs::read_to_string(root.join("provenance/UPSTREAM.sha256")).unwrap();
        for line in historical.lines() {
            let (digest, path) = line.split_once("  ").unwrap();
            let input = root.join(path);
            if !input.exists() {
                continue;
            }
            let bytes = archive::read(&input, archive::MAX_FILE).unwrap();
            if archive::hash(&bytes) != digest {
                let prefix = std::str::from_utf8(&bytes[..bytes.len().min(1024)]).unwrap();
                assert!(
                    prefix.contains("modified after extraction"),
                    "missing notice: {path}"
                );
            }
        }
        let wire = fs::read_to_string(root.join("src/managed/wire.rs")).unwrap();
        assert!(wire.starts_with("//! adapted and modified from Apache-2.0 Among Terminal"));
    }
}
