use crate::{
    archive::{self, Member},
    process, source, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

pub const IDS: [&str; 3] = ["clock", "system-status", "pane-pwd"];
pub const REPOSITORY: u64 = 1431055733;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Api {
    major: u32,
    minor: u32,
}

// This deliberately narrow producer profile is not an alternative host parser.
// The host's shared schema remains authoritative for arbitrary third-party data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    schema_version: u32,
    pub id: String,
    pub version: String,
    name: String,
    description: String,
    pub entrypoint: String,
    api: Api,
    pub required_features: Vec<String>,
    optional_features: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    pub kind: String,
    pub libc: String,
    pub minimum: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub os: String,
    pub architecture: String,
    pub runtime: Runtime,
    pub dependencies: Vec<Value>,
    pub format: String,
    pub size: u64,
    pub sha256: String,
    pub members: Vec<Member>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub name: String,
    pub manifest: Manifest,
    pub manifest_sha256: String,
    pub artifact: Artifact,
    pub needed_libraries: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub schema_version: u32,
    pub repository_id: u64,
    pub revision: String,
    pub architecture: String,
    pub source_archive: String,
    pub source_sha256: String,
    pub packages: Vec<Package>,
}

pub fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn revision(root: &Path) -> Result<String> {
    let revision = process::text(
        Command::new("git")
            .current_dir(root)
            .args(["rev-parse", "HEAD"]),
    )?
    .trim()
    .to_owned();
    if !hex(&revision, 40)
        || !process::text(Command::new("git").current_dir(root).args([
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
        ]))?
        .is_empty()
    {
        return Err("packaging requires a clean exact source commit".into());
    }
    Ok(revision)
}

pub fn architecture() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("amd64"),
        "aarch64" => Ok("arm64"),
        _ => Err("unsupported native build architecture".into()),
    }
}

fn manifest(root: &Path, id: &str) -> Result<(Manifest, Vec<u8>)> {
    let bytes = archive::read(
        &root.join(format!("plugins/{id}/among-plugin.json")),
        256 * 1024,
    )?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    let features: Vec<String> = if id == "pane-pwd" {
        vec![
            "status".into(),
            "context.terminals".into(),
            "context.cwd".into(),
        ]
    } else {
        vec!["status".into()]
    };
    if manifest.schema_version != 1
        || manifest.id != id
        || manifest.api != (Api { major: 1, minor: 2 })
        || manifest.entrypoint != format!("bin/{id}-managed")
        || manifest.required_features != features
        || !manifest.optional_features.is_empty()
        || manifest.name.trim().is_empty()
        || manifest.description.trim().is_empty()
        || manifest.name.len() > 128
        || manifest.description.len() > 2048
        || manifest.version.is_empty()
        || manifest.version.len() > 128
        || !manifest
            .version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return Err("manifest disagrees with the implemented managed production profile".into());
    }
    Ok((manifest, bytes))
}

fn version(value: &str) -> Result<[u16; 3]> {
    let mut result = [0; 3];
    let parts: Vec<_> = value.split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        return Err("invalid numeric ABI version".into());
    }
    for (index, part) in parts.iter().enumerate() {
        result[index] = part.parse()?;
    }
    Ok(result)
}

fn integer(bytes: &[u8], start: usize, length: usize) -> Result<u64> {
    let slice = bytes
        .get(start..start.checked_add(length).ok_or("ELF range overflow")?)
        .ok_or("truncated ELF")?;
    Ok(slice.iter().enumerate().fold(0, |result, (index, byte)| {
        result | (u64::from(*byte) << (index * 8))
    }))
}

pub fn elf(bytes: &[u8], architecture: &str) -> Result<()> {
    let machine = match architecture {
        "amd64" => 62,
        "arm64" => 183,
        _ => return Err("unknown ELF architecture".into()),
    };
    if bytes.get(..7) != Some(b"\x7fELF\x02\x01\x01")
        || !matches!(bytes.get(7), Some(0 | 3))
        || integer(bytes, 18, 2)? != machine
        || !matches!(integer(bytes, 16, 2)?, 2 | 3)
        || integer(bytes, 52, 2)? != 64
        || integer(bytes, 54, 2)? != 56
    {
        return Err("not a matching Linux ELF64 executable".into());
    }
    let offset = usize::try_from(integer(bytes, 32, 8)?)?;
    let count = usize::try_from(integer(bytes, 56, 2)?)?;
    if count == 0 || count > 128 {
        return Err("ELF program header limit".into());
    }
    let entry = integer(bytes, 24, 8)?;
    let mut executable_entry = false;
    let mut interpreter = None;
    for index in 0..count {
        let start = offset
            .checked_add(index * 56)
            .ok_or("ELF header overflow")?;
        let kind = integer(bytes, start, 4)?;
        let flags = integer(bytes, start + 4, 4)?;
        let file_offset = usize::try_from(integer(bytes, start + 8, 8)?)?;
        let address = integer(bytes, start + 16, 8)?;
        let size = usize::try_from(integer(bytes, start + 32, 8)?)?;
        let memory = integer(bytes, start + 40, 8)?;
        let content = bytes
            .get(
                file_offset
                    ..file_offset
                        .checked_add(size)
                        .ok_or("ELF segment overflow")?,
            )
            .ok_or("truncated ELF segment")?;
        if kind == 1
            && flags & 1 != 0
            && entry >= address
            && entry < address.checked_add(memory).ok_or("ELF address overflow")?
        {
            executable_entry = true;
        }
        if kind == 3 {
            if interpreter.is_some() || size > 256 {
                return Err("ambiguous ELF interpreter".into());
            }
            interpreter = Some(content);
        }
    }
    let loader: &[u8] = if architecture == "amd64" {
        b"/lib64/ld-linux-x86-64.so.2\0"
    } else {
        b"/lib/ld-linux-aarch64.so.1\0"
    };
    if !executable_entry || interpreter != Some(loader) {
        return Err("missing executable entry or unexpected glibc loader".into());
    }
    Ok(())
}

fn native(path: &Path, architecture: &str) -> Result<(Runtime, Vec<String>)> {
    elf(&archive::read(path, archive::MAX_FILE)?, architecture)?;
    let versions = process::text(
        Command::new("readelf")
            .args(["--version-info", "--wide"])
            .arg(path),
    )?;
    let mut minimum = None;
    for word in versions
        .split_whitespace()
        .filter_map(|word| word.strip_prefix("GLIBC_"))
    {
        let number = version(word)?;
        if minimum
            .as_ref()
            .is_none_or(|(current, _)| number > *current)
        {
            minimum = Some((number, word.to_owned()));
        }
    }
    let minimum = minimum.ok_or("no measured GLIBC symbol floor")?.1;
    let dynamic = process::text(
        Command::new("readelf")
            .args(["--dynamic", "--wide"])
            .arg(path),
    )?;
    let mut libraries = BTreeSet::new();
    for line in dynamic.lines() {
        if line.contains("(RPATH)") || line.contains("(RUNPATH)") {
            return Err("unmodeled native library search path".into());
        }
        if line.contains("(NEEDED)") {
            let name = line
                .split_once('[')
                .and_then(|(_, tail)| tail.split_once(']').map(|(name, _)| name))
                .ok_or("unrecognized DT_NEEDED")?;
            if !system_library(name, architecture) {
                return Err(format!("unmodeled shared library: {name}").into());
            }
            libraries.insert(name.to_owned());
        }
    }
    if !libraries.contains("libc.so.6") {
        return Err("missing glibc dependency".into());
    }
    let linked = process::text(Command::new("ldd").arg(path))?;
    if linked.contains("not found") {
        return Err("unresolved native library".into());
    }
    Ok((
        Runtime {
            kind: "native".into(),
            libc: "glibc".into(),
            minimum,
        },
        libraries.into_iter().collect(),
    ))
}

fn system_library(name: &str, architecture: &str) -> bool {
    [
        "libc.so.6",
        "libm.so.6",
        "libgcc_s.so.1",
        "libpthread.so.0",
        "libdl.so.2",
        "librt.so.1",
    ]
    .contains(&name)
        || matches!(
            (name, architecture),
            ("ld-linux-x86-64.so.2", "amd64") | ("ld-linux-aarch64.so.1", "arm64")
        )
}

fn delivery(root: &Path, vendor: &Path) -> Result<(source::Collector, archive::Files)> {
    let mut source = source::Collector::new(32768, 256 * 1024 * 1024);
    let tracked = process::capture(
        Command::new("git")
            .current_dir(root)
            .args(["ls-files", "-z"]),
        &[],
        4 * 1024 * 1024,
    )?;
    for path in tracked
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path)?;
        source.file(format!("source/{path}"), &root.join(path))?;
    }
    if !fs::symlink_metadata(vendor)?.is_dir() {
        return Err("vendor directory is not a regular directory".into());
    }
    source.walk(vendor, "vendor", 0)?;
    let mut legal = BTreeMap::new();
    for path in [
        "LICENSE",
        "NOTICE",
        "PROVENANCE.md",
        "provenance/among-NOTICE",
        "provenance/UPSTREAM.sha256",
    ] {
        legal.insert(
            path.into(),
            (archive::read(&root.join(path), archive::MAX_FILE)?, false),
        );
    }
    let metadata_root: Value = serde_json::from_slice(&process::capture(
        Command::new("cargo").current_dir(root).args([
            "metadata",
            "--locked",
            "--offline",
            "--format-version=1",
        ]),
        &[],
        8 * 1024 * 1024,
    )?)?;
    let metadata_tool: Value = serde_json::from_slice(&process::capture(
        Command::new("cargo").current_dir(root).args([
            "metadata",
            "--manifest-path",
            "tools/among-tools/Cargo.toml",
            "--locked",
            "--offline",
            "--format-version=1",
        ]),
        &[],
        8 * 1024 * 1024,
    )?)?;
    let mut dependencies = BTreeMap::new();
    for metadata in [metadata_root, metadata_tool] {
        for package in metadata["packages"]
            .as_array()
            .ok_or("missing Cargo package metadata")?
        {
            if package["source"].is_null() {
                continue;
            }
            let name = package["name"].as_str().ok_or("missing dependency name")?;
            let version = package["version"]
                .as_str()
                .ok_or("missing dependency version")?;
            let directory = format!("{name}-{version}");
            if !vendor.join(&directory).is_dir()
                || package["license"].as_str().is_none_or(str::is_empty)
            {
                return Err("dependency source/license metadata missing from vendor".into());
            }
            dependencies.insert(directory.clone(), json!({"name":name,"version":version,"license":package["license"],"repository":package["repository"]}));
            let prefix = format!("vendor/{directory}/");
            let mut notices = 0;
            for (path, content) in source
                .files
                .iter()
                .filter(|(path, _)| path.starts_with(&prefix))
            {
                let suffix = &path[prefix.len()..];
                let legal_file = suffix.split('/').any(|component| {
                    let name = component.to_ascii_lowercase();
                    [
                        "license",
                        "licence",
                        "copying",
                        "copyright",
                        "notice",
                        "unlicense",
                        "readme",
                    ]
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
                });
                if suffix == "Cargo.toml" || legal_file {
                    legal.insert(format!("THIRD_PARTY/{directory}/{suffix}"), content.clone());
                    if legal_file {
                        notices += 1;
                    }
                }
            }
            if notices == 0 {
                return Err(format!("no dependency licensing text: {directory}").into());
            }
        }
    }
    let index = serde_json::to_vec_pretty(&dependencies)?;
    legal.insert("THIRD_PARTY/DEPENDENCIES.json".into(), (index, false));
    source.insert("source/.cargo/config.toml".into(), b"[source.crates-io]\nreplace-with = 'vendored-sources'\n[source.vendored-sources]\ndirectory = '../vendor'\n".to_vec())?;
    Ok((source, legal))
}

pub fn build(root: &Path, binaries: &Path, vendor: &Path, output: &Path) -> Result<()> {
    let revision = revision(root)?;
    let architecture = architecture()?.to_owned();
    let (mut source, legal) = delivery(root, vendor)?;
    source.insert(
        "SOURCE.json".into(),
        serde_json::to_vec_pretty(&json!({"repository_id": REPOSITORY, "revision": revision}))?,
    )?;
    fs::create_dir(output)?;
    let source_archive = format!("among-plugins-{revision}-source.tar.gz");
    archive::pack(&output.join(&source_archive), &source.files)?;
    let source_sha256 = archive::hash(&archive::read(
        &output.join(&source_archive),
        archive::MAX_FILE,
    )?);
    let mut packages = Vec::new();
    for id in IDS {
        let (manifest, manifest_bytes) = manifest(root, id)?;
        let binary = binaries.join(format!("{id}-managed"));
        let (runtime, needed_libraries) = native(&binary, &architecture)?;
        let mut files = legal.clone();
        files.insert("among-plugin.json".into(), (manifest_bytes.clone(), false));
        files.insert(
            manifest.entrypoint.clone(),
            (archive::read(&binary, archive::MAX_FILE)?, true),
        );
        files.insert("SOURCE.json".into(), (serde_json::to_vec_pretty(&json!({"repository_id":REPOSITORY,"revision":revision,"source_archive":source_archive,"source_sha256":source_sha256}))?, false));
        let name = format!("{id}-{}-linux-{architecture}.tar.gz", manifest.version);
        let members = archive::pack(&output.join(&name), &files)?;
        let bytes = archive::read(&output.join(&name), archive::MAX_FILE)?;
        archive::unpack(&bytes, &members)?;
        packages.push(Package {
            name,
            manifest,
            manifest_sha256: archive::hash(&manifest_bytes),
            artifact: Artifact {
                os: "linux".into(),
                architecture: architecture.clone(),
                runtime,
                dependencies: Vec::new(),
                format: "tar.gz".into(),
                size: bytes.len() as u64,
                sha256: archive::hash(&bytes),
                members,
            },
            needed_libraries,
        });
    }
    let report = Build {
        schema_version: 1,
        repository_id: REPOSITORY,
        revision,
        architecture,
        source_archive,
        source_sha256,
        packages,
    };
    archive::json(&output.join("build.json"), &report)?;
    let mut sums = String::new();
    for package in &report.packages {
        sums.push_str(&format!("{}  {}\n", package.artifact.sha256, package.name));
    }
    sums.push_str(&format!(
        "{}  {}\n",
        report.source_sha256, report.source_archive
    ));
    archive::write_new(&output.join("SHA256SUMS"), sums.as_bytes())?;
    check(root, output)
}

pub fn load(root: &Path, directory: &Path) -> Result<Build> {
    let report: Build =
        serde_json::from_slice(&archive::read(&directory.join("build.json"), 1024 * 1024)?)?;
    if report.schema_version != 1
        || report.repository_id != REPOSITORY
        || report.revision != revision(root)?
        || !["amd64", "arm64"].contains(&report.architecture.as_str())
        || report.packages.len() != 3
    {
        return Err("build provenance or inventory mismatch".into());
    }
    if report.source_archive != format!("among-plugins-{}-source.tar.gz", report.revision)
        || archive::hash(&archive::read(
            &directory.join(&report.source_archive),
            archive::MAX_FILE,
        )?) != report.source_sha256
    {
        return Err("source delivery mismatch".into());
    }
    let mut ids = BTreeSet::new();
    for package in &report.packages {
        let id = package.manifest.id.as_str();
        if !IDS.contains(&id) || !ids.insert(id) {
            return Err("duplicate or unknown plugin in build".into());
        }
        let (manifest, bytes) = manifest(root, id)?;
        if manifest != package.manifest
            || archive::hash(&bytes) != package.manifest_sha256
            || package.name
                != format!(
                    "{id}-{}-linux-{}.tar.gz",
                    manifest.version, report.architecture
                )
            || package.artifact.architecture != report.architecture
            || package.artifact.os != "linux"
            || package.artifact.format != "tar.gz"
            || package.artifact.runtime.kind != "native"
            || package.artifact.runtime.libc != "glibc"
            || !package.artifact.dependencies.is_empty()
        {
            return Err("package profile mismatch".into());
        }
        version(&package.artifact.runtime.minimum)?;
        let packed = archive::read(&directory.join(&package.name), archive::MAX_FILE)?;
        if packed.len() as u64 != package.artifact.size
            || archive::hash(&packed) != package.artifact.sha256
        {
            return Err("package archive size/digest mismatch".into());
        }
        let files = archive::unpack(&packed, &package.artifact.members)?;
        if files.get("among-plugin.json") != Some(&bytes) {
            return Err("packaged manifest mismatch".into());
        }
        elf(
            files
                .get(&manifest.entrypoint)
                .ok_or("missing declared executable")?,
            &report.architecture,
        )?;
    }
    Ok(report)
}

pub fn check(root: &Path, directory: &Path) -> Result<()> {
    let report = load(root, directory)?;
    if report.architecture != architecture()? {
        return Err("package smoke test requires its native architecture".into());
    }
    let checks = directory.join(format!("checks-{}", std::process::id()));
    fs::create_dir(&checks)?;
    for package in &report.packages {
        let bytes = archive::read(&directory.join(&package.name), archive::MAX_FILE)?;
        let files = archive::unpack(&bytes, &package.artifact.members)?;
        let binary = checks.join(format!("{}-managed", package.manifest.id));
        archive::write_new(
            &binary,
            files
                .get(&package.manifest.entrypoint)
                .ok_or("missing executable")?,
        )?;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
        let (runtime, libraries) = native(&binary, &report.architecture)?;
        if runtime != package.artifact.runtime || libraries != package.needed_libraries {
            return Err("packaged ABI differs from build record".into());
        }
        let hello = admission(&package.manifest);
        let input = format!("{hello}\n{{\"type\":\"shutdown\"}}\n");
        let output = process::capture(&mut Command::new(&binary), input.as_bytes(), 256 * 1024)?;
        archive::write_new(
            &checks.join(format!("{}-admission.json", package.manifest.id)),
            &output,
        )?;
        let response: Value = serde_json::from_slice(&output)?;
        if response != hello {
            return Err(format!(
                "packaged {} admission mismatch: expected {hello}, observed {response}",
                package.manifest.id
            )
            .into());
        }
    }
    Ok(())
}

fn admission(manifest: &Manifest) -> Value {
    let identity = format!("github:{REPOSITORY}/{}", manifest.id);
    let features: BTreeSet<_> = manifest.required_features.iter().collect();
    json!({"type":"managed_hello","major":1,"minor":2,"identity":identity,"session":"package-check","generation":"package-check","features":features})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_and_wrong_architecture_elf_are_refused() {
        assert!(elf(b"not ELF", "amd64").is_err());
        assert!(elf(b"\x7fELF\x02\x01\x01", "arm64").is_err());
        assert!(elf(&[0; 128], "ppc64").is_err());
    }

    #[test]
    fn only_the_matching_glibc_loader_is_a_modeled_dependency() {
        assert!(system_library("ld-linux-x86-64.so.2", "amd64"));
        assert!(system_library("ld-linux-aarch64.so.1", "arm64"));
        assert!(!system_library("ld-linux-x86-64.so.2", "arm64"));
        assert!(!system_library("libpublisher.so.1", "amd64"));
    }

    #[test]
    fn pane_package_admission_matches_actual_sdk_canonical_feature_order() {
        use among_example_plugins::{managed, pwd};
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (manifest, _) = manifest(&root, "pane-pwd").unwrap();
        let hello = admission(&manifest);
        let input = format!("{hello}\n{{\"type\":\"shutdown\"}}\n");
        let profile = managed::Profile {
            identity: "github:1431055733/pane-pwd",
            required: &["status", "context.terminals", "context.cwd"],
            optional: &[],
        };
        let mut output = Vec::new();
        managed::serve(&profile, input.as_bytes(), &mut output, pwd::managed_status).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&output).unwrap(), hello);
        assert_eq!(
            hello["features"],
            json!(["context.cwd", "context.terminals", "status"])
        );
    }
}
