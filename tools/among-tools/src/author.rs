//! Packages, catalogue and release checks for a plugin author's repository.
//!
//! The official commands in `package.rs` and `catalogue.rs` keep their own
//! profile and schema 1 catalogue. This module builds the same package format
//! from whatever plugins a repository declares, writes the schema 2 catalogue
//! that is attached to the release itself, and checks a GitHub release against
//! the exact expected inventory before and after publication. Nothing here
//! uses an official identifier.

use crate::{
    archive::{self, Member},
    package::{self, Delivery, Runtime},
    strict, toolchain, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const AUTHOR: Delivery<'static> = Delivery {
    legal_required: &["LICENSE"],
    legal_optional: &["NOTICE"],
    manifests: &[None],
};
const MAX_PLUGINS: usize = 32;
const MAX_MANIFEST: u64 = 256 * 1024;
const MAX_ICON: u64 = 1024 * 1024;
const MAX_CATALOGUE: usize = 1024 * 1024;
const MAX_ASSETS: usize = 64;
const CATALOGUE: &str = "among-plugins.json";
const SUMS: &str = "SHA256SUMS";
pub const BINDING_PREFIX: &str = "among-release-binding: ";

/// One plugin as its repository declares it under `plugins/<id>/`.
struct Declared {
    id: String,
    version: String,
    entrypoint: String,
    manifest: Vec<u8>,
    icon: Option<(String, Vec<u8>)>,
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
    pub id: String,
    pub name: String,
    pub manifest_sha256: String,
    pub artifact: Artifact,
    pub needed_libraries: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub schema: String,
    pub schema_version: u32,
    pub repository_id: u64,
    pub revision: String,
    pub architecture: String,
    pub source_archive: String,
    pub source_sha256: String,
    pub packages: Vec<Package>,
    pub toolchain: toolchain::Record,
}

const BUILD_SCHEMA: &str = "among-author-build";

fn plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Checks only what packaging itself relies on. The host's manifest parser
/// stays the only full validator, so this is deliberately not a second one.
fn manifest_fields(bytes: &[u8], id: &str) -> Result<(String, String, Option<String>)> {
    let manifest = strict::parse(bytes)?;
    let text = |name: &str| {
        manifest[name]
            .as_str()
            .map(str::to_owned)
            .ok_or(format!("{id}: manifest {name} missing"))
    };
    let version = text("version")?;
    let entrypoint = text("entrypoint")?;
    if manifest["schema_version"] != 1
        || manifest["id"] != id
        || manifest["api"]["major"] != 1
        || version.is_empty()
        || version.len() > 128
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        || !archive::relative(&entrypoint)
        || entrypoint == "among-plugin.json"
    {
        return Err(format!(
            "{id}: manifest needs schema_version 1, its directory id, api.major 1, a plain version and a relative entrypoint"
        )
        .into());
    }
    let icon = match manifest.get("icon") {
        None => None,
        Some(Value::String(icon)) if icon.starts_with("theme:") => None,
        Some(Value::String(icon)) if archive::relative(icon) && icon != &entrypoint => {
            Some(icon.clone())
        }
        Some(_) => return Err(format!("{id}: icon is neither theme:<name> nor a file").into()),
    };
    Ok((version, entrypoint, icon))
}

fn files_below(directory: &Path, prefix: &str, found: &mut Vec<String>) -> Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "non-UTF-8 plugin path")?;
        let path = format!("{prefix}{name}");
        let kind = fs::symlink_metadata(entry.path())?.file_type();
        if kind.is_dir() {
            files_below(&entry.path(), &format!("{path}/"), found)?;
        } else if kind.is_file() {
            found.push(path);
        } else {
            return Err(format!("not a regular file: {path}").into());
        }
    }
    Ok(())
}

/// Plugins of `root`, one per directory of `plugins/`. Each directory holds
/// its manifest and, when the manifest names a file icon, that icon; any other
/// file refuses the build, so nothing is packed or left out silently.
fn declared(root: &Path) -> Result<Vec<Declared>> {
    let directory = root.join("plugins");
    if !fs::symlink_metadata(&directory)?.is_dir() {
        return Err("plugins/ is not a directory".into());
    }
    let mut ids = fs::read_dir(&directory)?
        .map(|entry| {
            let entry = entry?;
            if !fs::symlink_metadata(entry.path())?.is_dir() {
                return Err(
                    format!("plugins/ holds a non-directory: {:?}", entry.file_name()).into(),
                );
            }
            entry
                .file_name()
                .into_string()
                .map_err(|_| "non-UTF-8 plugin directory".into())
        })
        .collect::<Result<Vec<_>>>()?;
    ids.sort();
    if ids.is_empty() || ids.len() > MAX_PLUGINS {
        return Err(format!("plugins/ must hold 1 to {MAX_PLUGINS} plugin directories").into());
    }
    let mut plugins = Vec::new();
    let mut binaries = BTreeMap::new();
    for id in ids {
        if !plugin_id(&id) {
            return Err(format!("plugin directory is not a plugin id: {id}").into());
        }
        let base = directory.join(&id);
        let manifest = archive::read(&base.join("among-plugin.json"), MAX_MANIFEST)?;
        let (version, entrypoint, icon_path) = manifest_fields(&manifest, &id)?;
        let mut expected = BTreeSet::from(["among-plugin.json".to_owned()]);
        let icon = match icon_path {
            Some(path) => {
                expected.insert(path.clone());
                let bytes = archive::read(&base.join(&path), MAX_ICON).map_err(|error| {
                    format!("plugins/{id}: icon {path} is not a readable file up to 1 MiB: {error}")
                })?;
                Some((path, bytes))
            }
            None => None,
        };
        let mut present = Vec::new();
        files_below(&base, "", &mut present)?;
        if let Some(extra) = present.iter().find(|path| !expected.contains(*path)) {
            return Err(format!("unsupported file in plugins/{id}: {extra}").into());
        }
        let binary = entrypoint
            .rsplit('/')
            .next()
            .ok_or("empty entrypoint")?
            .to_owned();
        if let Some(other) = binaries.insert(binary.clone(), id.clone()) {
            return Err(
                format!("plugins {other} and {id} both name the executable {binary}").into(),
            );
        }
        plugins.push(Declared {
            id,
            version,
            entrypoint,
            manifest,
            icon,
        });
    }
    Ok(plugins)
}

pub fn source_archive_name(revision: &str) -> String {
    format!("source-{revision}.tar.gz")
}

fn package_name(id: &str, version: &str, architecture: &str) -> String {
    format!("{id}-{version}-linux-{architecture}.tar.gz")
}

fn source_record(repository_id: u64, revision: &str, archive: &str, sha256: &str) -> Value {
    json!({"repository_id":repository_id,"revision":revision,"source_archive":archive,"source_sha256":sha256})
}

/// `author-packages`: native packages of every declared plugin plus the
/// source and licence delivery, for this machine's architecture.
pub fn packages(
    root: &Path,
    binaries: &Path,
    vendor: &Path,
    repository_id: u64,
    output: &Path,
) -> Result<()> {
    if repository_id == 0 {
        return Err("repository ID must be nonzero".into());
    }
    let revision = package::revision(root)?;
    let architecture = package::architecture()?.to_owned();
    let plugins = declared(root)?;
    let (mut source, legal) = package::delivery(root, vendor, &AUTHOR)?;
    let rust = toolchain::collect(&architecture)?;
    for (path, (bytes, _)) in &rust.common {
        source.insert(format!("toolchain/{path}"), bytes.clone())?;
    }
    source.insert(
        "SOURCE.json".into(),
        serde_json::to_vec_pretty(&json!({"repository_id": repository_id, "revision": revision}))?,
    )?;
    fs::create_dir(output)?;
    let source_archive = source_archive_name(&revision);
    archive::pack(&output.join(&source_archive), &source.files)?;
    let source_sha256 = archive::hash(&archive::read(
        &output.join(&source_archive),
        archive::MAX_FILE,
    )?);
    let record = source_record(repository_id, &revision, &source_archive, &source_sha256);
    let mut packages = Vec::new();
    for plugin in plugins {
        let binary = binaries.join(plugin.entrypoint.rsplit('/').next().unwrap_or_default());
        let (runtime, needed_libraries) = package::native(&binary, &architecture)?;
        let mut files = legal.clone();
        files.extend(rust.common.clone());
        files.extend(rust.native.clone());
        let mut add = |path: &str, bytes: Vec<u8>, executable: bool| -> Result<()> {
            if files.insert(path.to_owned(), (bytes, executable)).is_some() {
                return Err(format!("{}: package path {path} is used twice", plugin.id).into());
            }
            Ok(())
        };
        add("among-plugin.json", plugin.manifest.clone(), false)?;
        add(
            &plugin.entrypoint,
            archive::read(&binary, archive::MAX_FILE)?,
            true,
        )?;
        if let Some((path, bytes)) = &plugin.icon {
            add(path, bytes.clone(), false)?;
        }
        add("SOURCE.json", serde_json::to_vec_pretty(&record)?, false)?;
        let name = package_name(&plugin.id, &plugin.version, &architecture);
        let members = archive::pack(&output.join(&name), &files)?;
        let bytes = archive::read(&output.join(&name), archive::MAX_FILE)?;
        archive::unpack(&bytes, &members)?;
        packages.push(Package {
            id: plugin.id,
            name,
            manifest_sha256: archive::hash(&plugin.manifest),
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
    archive::json(
        &output.join("build.json"),
        &Build {
            schema: BUILD_SCHEMA.into(),
            schema_version: 1,
            repository_id,
            revision,
            architecture,
            source_archive,
            source_sha256,
            packages,
            toolchain: rust.record,
        },
    )
}

/// A build directory produced by `author-packages`, read as untrusted data:
/// every value is checked against the expected repository, revision and
/// architecture and against the archives themselves.
struct Checked {
    build: Build,
    directory: PathBuf,
    manifests: BTreeMap<String, Vec<u8>>,
}

/// Checks a build's Rust toolchain notices; production uses
/// `toolchain::verify`, tests a stand-in for a synthetic toolchain record.
type ToolchainCheck = fn(&toolchain::Record, &str, &BTreeMap<String, Vec<u8>>) -> Result<()>;

fn load(
    directory: &Path,
    repository_id: u64,
    revision: &str,
    architecture: &str,
    verify_toolchain: ToolchainCheck,
) -> Result<Checked> {
    let build: Build = serde_json::from_value(strict::parse(&archive::read(
        &directory.join("build.json"),
        4 * 1024 * 1024,
    )?)?)?;
    if build.schema != BUILD_SCHEMA
        || build.schema_version != 1
        || build.repository_id != repository_id
        || build.revision != revision
        || build.architecture != architecture
        || build.source_archive != source_archive_name(revision)
        || !package::hex(&build.source_sha256, 64)
    {
        return Err(format!(
            "{architecture} build is not this repository, revision and architecture"
        )
        .into());
    }
    if archive::hash(&archive::read(
        &directory.join(&build.source_archive),
        archive::MAX_FILE,
    )?) != build.source_sha256
    {
        return Err(format!("{architecture} source archive differs from its record").into());
    }
    let record = source_record(
        repository_id,
        revision,
        &build.source_archive,
        &build.source_sha256,
    );
    let mut manifests = BTreeMap::new();
    for package in &build.packages {
        let artifact = &package.artifact;
        if !plugin_id(&package.id)
            || artifact.os != "linux"
            || artifact.architecture != architecture
            || artifact.format != "tar.gz"
            || artifact.runtime.kind != "native"
            || artifact.runtime.libc != "glibc"
            || !artifact.dependencies.is_empty()
            || manifests.contains_key(&package.id)
        {
            return Err(format!("{architecture} package {} profile mismatch", package.id).into());
        }
        package::version(&artifact.runtime.minimum)?;
        // The name is checked before it is used as a path; its version part
        // is compared with the manifest below.
        if package.name.contains('/')
            || !archive::relative(&package.name)
            || !package.name.starts_with(&format!("{}-", package.id))
            || !package
                .name
                .ends_with(&format!("-linux-{architecture}.tar.gz"))
        {
            return Err(format!(
                "{architecture} package name {} is not a package of {}",
                package.name, package.id
            )
            .into());
        }
        let bytes = archive::read(&directory.join(&package.name), archive::MAX_FILE)?;
        if bytes.len() as u64 != artifact.size || archive::hash(&bytes) != artifact.sha256 {
            return Err(format!("{} differs from its record", package.name).into());
        }
        let files = archive::unpack(&bytes, &artifact.members)?;
        verify_toolchain(&build.toolchain, architecture, &files)?;
        let manifest = files
            .get("among-plugin.json")
            .ok_or_else(|| format!("{} has no manifest", package.name))?;
        let (version, entrypoint, _) = manifest_fields(manifest, &package.id)?;
        if archive::hash(manifest) != package.manifest_sha256
            || package.name != package_name(&package.id, &version, architecture)
            || files
                .get("SOURCE.json")
                .map(|bytes| strict::parse(bytes))
                .transpose()?
                != Some(record.clone())
        {
            return Err(
                format!("{} manifest, name or source record mismatch", package.name).into(),
            );
        }
        package::elf(
            files
                .get(&entrypoint)
                .ok_or_else(|| format!("{} lacks its entrypoint", package.name))?,
            architecture,
        )?;
        manifests.insert(package.id.clone(), manifest.clone());
    }
    if manifests.is_empty() {
        return Err(format!("{architecture} build holds no plugin").into());
    }
    Ok(Checked {
        build,
        directory: directory.to_owned(),
        manifests,
    })
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub repository_id: u64,
    pub tag: String,
    pub revision: String,
    /// SHA-256 of the `SHA256SUMS` bytes, which name every other asset.
    pub inventory_sha256: String,
    pub assets: Vec<Asset>,
}

/// `author-catalogue`: the schema 2 catalogue and the complete release asset
/// set, written to `output/assets`, plus `output/inventory.json`, the exact
/// snapshot the release is checked against.
pub fn catalogue(
    amd64: &Path,
    arm64: &Path,
    repository_id: u64,
    tag: &str,
    revision: &str,
    output: &Path,
) -> Result<()> {
    catalogue_with(
        amd64,
        arm64,
        repository_id,
        tag,
        revision,
        output,
        toolchain::verify,
    )
}

fn catalogue_with(
    amd64: &Path,
    arm64: &Path,
    repository_id: u64,
    tag: &str,
    revision: &str,
    output: &Path,
    verify_toolchain: ToolchainCheck,
) -> Result<()> {
    if repository_id == 0 || !package::hex(revision, 40) || !crate::catalogue::tag_name(tag) {
        return Err("expected repository ID, revision and tag are malformed".into());
    }
    let builds = [
        load(amd64, repository_id, revision, "amd64", verify_toolchain)?,
        load(arm64, repository_id, revision, "arm64", verify_toolchain)?,
    ];
    if builds[0].build.source_sha256 != builds[1].build.source_sha256
        || builds[0].manifests != builds[1].manifests
    {
        return Err("architectures differ in source delivery, plugin set or manifest bytes".into());
    }
    let assets = output.join("assets");
    fs::create_dir(output)?;
    fs::create_dir(&assets)?;
    let mut sums = BTreeMap::new();
    let mut copy = |directory: &Path, name: &str| -> Result<()> {
        let bytes = archive::read(&directory.join(name), archive::MAX_FILE)?;
        archive::write_new(&assets.join(name), &bytes)?;
        if sums
            .insert(name.to_owned(), (bytes.len() as u64, archive::hash(&bytes)))
            .is_some()
        {
            return Err(format!("release asset {name} appears twice").into());
        }
        Ok(())
    };
    copy(&builds[0].directory, &builds[0].build.source_archive)?;
    let mut plugins = Vec::new();
    for (id, manifest) in &builds[0].manifests {
        let mut artifacts = Vec::new();
        for build in &builds {
            let package = build
                .build
                .packages
                .iter()
                .find(|package| &package.id == id)
                .ok_or_else(|| format!("{id} missing on one architecture"))?;
            copy(&build.directory, &package.name)?;
            let mut artifact = serde_json::to_value(&package.artifact)?;
            artifact
                .as_object_mut()
                .ok_or("artifact is not an object")?
                .insert("asset".into(), package.name.clone().into());
            artifacts.push(artifact);
        }
        plugins.push(json!({"id": id, "versions": [{
            "revision": revision,
            "manifest_sha256": archive::hash(manifest),
            "manifest": strict::parse(manifest)?,
            "artifacts": artifacts,
        }]}));
    }
    let mut catalogue =
        serde_json::to_vec_pretty(&json!({"schema_version": 2, "plugins": plugins}))?;
    catalogue.push(b'\n');
    if catalogue.len() > MAX_CATALOGUE {
        return Err(format!(
            "catalogue is {} bytes, above the host's 1 MiB limit",
            catalogue.len()
        )
        .into());
    }
    archive::write_new(&assets.join(CATALOGUE), &catalogue)?;
    sums.insert(
        CATALOGUE.into(),
        (catalogue.len() as u64, archive::hash(&catalogue)),
    );
    if sums.len() + 1 > MAX_ASSETS {
        return Err(format!(
            "{} release assets exceed the limit of {MAX_ASSETS}",
            sums.len() + 1
        )
        .into());
    }
    let listing = sums
        .iter()
        .map(|(name, (_, sha256))| format!("{sha256}  {name}\n"))
        .collect::<String>();
    archive::write_new(&assets.join(SUMS), listing.as_bytes())?;
    let mut inventory = sums
        .into_iter()
        .map(|(name, (size, sha256))| Asset { name, size, sha256 })
        .collect::<Vec<_>>();
    inventory.push(Asset {
        name: SUMS.into(),
        size: listing.len() as u64,
        sha256: archive::hash(listing.as_bytes()),
    });
    inventory.sort_by(|a, b| a.name.cmp(&b.name));
    archive::json(
        &output.join("inventory.json"),
        &Inventory {
            repository_id,
            tag: tag.to_owned(),
            revision: revision.to_owned(),
            inventory_sha256: archive::hash(listing.as_bytes()),
            assets: inventory,
        },
    )
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub repository_id: u64,
    pub workflow_ref: String,
    pub run_id: u64,
    pub tag: String,
    pub commit: String,
    pub inventory_sha256: String,
}

/// `release-binding`: the one line a draft created by the release workflow
/// carries in its body, tying it to the repository, the run, the tag, the
/// source pin and the exact inventory.
pub fn binding(inventory: &Path, workflow_ref: &str, run_id: u64, output: &Path) -> Result<()> {
    let inventory = read_inventory(inventory)?;
    if workflow_ref.is_empty()
        || workflow_ref.len() > 512
        || !workflow_ref.is_ascii()
        || workflow_ref.contains(char::is_whitespace)
        || run_id == 0
    {
        return Err("workflow ref or run ID is malformed".into());
    }
    let binding = Binding {
        repository_id: inventory.repository_id,
        workflow_ref: workflow_ref.into(),
        run_id,
        tag: inventory.tag,
        commit: inventory.revision,
        inventory_sha256: inventory.inventory_sha256,
    };
    let line = format!("{BINDING_PREFIX}{}\n", serde_json::to_string(&binding)?);
    archive::write_new(output, line.as_bytes())
}

fn read_inventory(path: &Path) -> Result<Inventory> {
    Ok(serde_json::from_value(strict::parse(&archive::read(
        path,
        1024 * 1024,
    )?)?)?)
}

/// Rebind preparation metadata to the approved tag without rebuilding assets.
/// The approval digest covers SHA256SUMS, whose canonical bytes cover every
/// other asset. No output is created until the entire directory agrees.
pub fn prepared_inventory(
    prepared: &Path,
    repository_id: u64,
    revision: &str,
    tag: &str,
    approved_inventory_sha256: &str,
    output: &Path,
) -> Result<()> {
    if repository_id == 0
        || !package::hex(revision, 40)
        || !crate::catalogue::tag_name(tag)
        || !package::hex(approved_inventory_sha256, 64)
        || !fs::symlink_metadata(prepared)?.is_dir()
    {
        return Err("prepared release requires exact repository/source/tag/approval inputs".into());
    }
    let mut inventory = read_inventory(&prepared.join("inventory.json"))?;
    if inventory.repository_id != repository_id
        || inventory.revision != revision
        || inventory.inventory_sha256 != approved_inventory_sha256
        || inventory.assets.is_empty()
        || inventory.assets.len() > MAX_ASSETS
    {
        return Err("prepared inventory differs from the approved source or digest".into());
    }
    let assets = prepared.join("assets");
    if !fs::symlink_metadata(&assets)?.is_dir() {
        return Err("prepared assets must be a real directory".into());
    }
    let mut expected = BTreeMap::new();
    for asset in &inventory.assets {
        if asset.name.contains('/')
            || !archive::relative(&asset.name)
            || asset.size == 0
            || asset.size > archive::MAX_FILE
            || !package::hex(&asset.sha256, 64)
            || expected.insert(asset.name.as_str(), asset).is_some()
        {
            return Err("prepared inventory has invalid or duplicate assets".into());
        }
    }
    let source_name = source_archive_name(revision);
    for required in [CATALOGUE, SUMS, source_name.as_str()] {
        if !expected.contains_key(required) {
            return Err(format!("prepared inventory lacks {required}").into());
        }
    }
    let mut seen = BTreeSet::new();
    for entry in fs::read_dir(&assets)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "non-UTF-8 prepared asset")?;
        let declaration = expected
            .get(name.as_str())
            .ok_or("unrecorded prepared asset")?;
        let bytes = archive::read(&entry.path(), archive::MAX_FILE)?;
        if !seen.insert(name)
            || bytes.len() as u64 != declaration.size
            || archive::hash(&bytes) != declaration.sha256
        {
            return Err("prepared asset bytes differ from the approved inventory".into());
        }
    }
    if seen.len() != expected.len() {
        return Err("prepared release is missing an approved asset".into());
    }
    let sums = archive::read(&assets.join(SUMS), 64 * 1024)?;
    let canonical = expected
        .iter()
        .filter(|(name, _)| **name != SUMS)
        .map(|(name, asset)| format!("{}  {name}\n", asset.sha256))
        .collect::<String>();
    if sums != canonical.as_bytes() || archive::hash(&sums) != approved_inventory_sha256 {
        return Err("prepared SHA256SUMS differs from the exact owner approval".into());
    }
    let catalogue = strict::parse(&archive::read(
        &assets.join(CATALOGUE),
        MAX_CATALOGUE as u64,
    )?)?;
    let plugins = catalogue["plugins"]
        .as_array()
        .filter(|entries| !entries.is_empty() && entries.len() <= MAX_PLUGINS)
        .ok_or("prepared catalogue lacks bounded plugins")?;
    if catalogue["schema_version"] != 2 {
        return Err("prepared catalogue must use schema2".into());
    }
    let source = source_record(
        repository_id,
        revision,
        &source_name,
        &expected[source_name.as_str()].sha256,
    );
    let mut referenced = BTreeSet::from([CATALOGUE.to_owned(), SUMS.to_owned(), source_name]);
    let mut ids = BTreeSet::new();
    for plugin in plugins {
        let id = plugin["id"]
            .as_str()
            .filter(|id| plugin_id(id))
            .ok_or("prepared plugin lacks an id")?;
        let versions = plugin["versions"]
            .as_array()
            .filter(|versions| versions.len() == 1)
            .ok_or("prepared plugin requires exactly one version")?;
        let version = &versions[0];
        if !ids.insert(id) || version["revision"] != revision {
            return Err(
                "prepared catalogue does not bind unique plugins to the approved source".into(),
            );
        }
        let artifacts = version["artifacts"]
            .as_array()
            .filter(|artifacts| artifacts.len() == 2)
            .ok_or("prepared plugin requires both native architectures")?;
        let mut architectures = BTreeSet::new();
        for artifact in artifacts {
            let name = artifact["asset"]
                .as_str()
                .ok_or("prepared package lacks an asset name")?;
            let asset = expected
                .get(name)
                .ok_or("catalogue package is absent from inventory")?;
            let architecture = artifact["architecture"]
                .as_str()
                .ok_or("prepared package lacks architecture")?;
            if !matches!(architecture, "amd64" | "arm64")
                || !architectures.insert(architecture)
                || !referenced.insert(name.to_owned())
                || artifact["size"] != asset.size
                || artifact["sha256"] != asset.sha256
            {
                return Err("prepared catalogue package differs from the asset inventory".into());
            }
            let members: Vec<Member> = serde_json::from_value(artifact["members"].clone())?;
            let files = archive::unpack(
                &archive::read(&assets.join(name), archive::MAX_FILE)?,
                &members,
            )?;
            let manifest = files
                .get("among-plugin.json")
                .ok_or("prepared package lacks manifest")?;
            let (plugin_version, _, _) = manifest_fields(manifest, id)?;
            if name != package_name(id, &plugin_version, architecture)
                || version["manifest_sha256"] != archive::hash(manifest)
                || version["manifest"] != strict::parse(manifest)?
                || files
                    .get("SOURCE.json")
                    .map(|bytes| strict::parse(bytes))
                    .transpose()?
                    != Some(source.clone())
            {
                return Err("prepared package manifest or source binding differs".into());
            }
        }
    }
    if referenced.len() != expected.len() {
        return Err("prepared inventory contains assets outside its catalogue".into());
    }
    inventory.tag = tag.into();
    archive::json(output, &inventory)
}

fn body_binding(release: &Value) -> Result<Binding> {
    let body = release["body"].as_str().unwrap_or_default();
    let mut lines = body
        .lines()
        .filter_map(|line| line.strip_prefix(BINDING_PREFIX));
    let line = lines.next().ok_or("release carries no binding")?;
    if lines.next().is_some() {
        return Err("release carries more than one binding".into());
    }
    Ok(serde_json::from_value(strict::parse(line.as_bytes())?)?)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// An existing draft found for the tag: resumable only when it is ours.
    Resume,
    /// The draft after uploads, before publication.
    Draft,
    /// The release after publication.
    Published,
}

/// `verify-release`: compares a release, as the REST API returns it, with
/// the expected inventory and binding. For `Resume` it returns the names of
/// assets still to upload; otherwise an empty list.
pub fn verify_release(
    stage: Stage,
    release: &Path,
    inventory: &Path,
    binding: &Path,
    latest: Option<&Path>,
) -> Result<Vec<String>> {
    let release = strict::parse(&archive::read(release, 4 * 1024 * 1024)?)?;
    let inventory = read_inventory(inventory)?;
    let expected: Binding = serde_json::from_value(strict::parse(
        archive::read(binding, 64 * 1024)?
            .strip_prefix(BINDING_PREFIX.as_bytes())
            .ok_or("binding file lacks its prefix")?,
    )?)?;
    if expected.repository_id != inventory.repository_id
        || expected.tag != inventory.tag
        || expected.commit != inventory.revision
        || expected.inventory_sha256 != inventory.inventory_sha256
    {
        return Err("binding and inventory describe different releases".into());
    }
    let id = release["id"]
        .as_u64()
        .filter(|id| *id != 0)
        .ok_or("release has no ID")?;
    let draft = stage != Stage::Published;
    if release["tag_name"] != inventory.tag.as_str()
        || release["draft"] != draft
        || release["prerelease"] != false
    {
        return Err(format!(
            "release {id} is not the {} release of {}",
            if draft { "draft" } else { "published" },
            inventory.tag
        )
        .into());
    }
    if body_binding(&release)? != expected {
        return Err(format!(
            "release {id} is bound to another repository, run, tag, commit or inventory"
        )
        .into());
    }
    let assets = release["assets"]
        .as_array()
        .ok_or("release lists no assets")?;
    if assets.len() > MAX_ASSETS {
        return Err(format!("release {id} has more than {MAX_ASSETS} assets").into());
    }
    let wanted = inventory
        .assets
        .iter()
        .map(|asset| (asset.name.as_str(), asset))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for asset in assets {
        let name = asset["name"].as_str().ok_or("asset has no name")?;
        let expected = wanted
            .get(name)
            .ok_or_else(|| format!("release {id} has unexpected asset {name}"))?;
        let digest = asset["digest"].as_str().unwrap_or_default();
        if !seen.insert(name)
            || asset["size"].as_u64() != Some(expected.size)
            || asset["state"] != "uploaded"
            || digest.strip_prefix("sha256:") != Some(expected.sha256.as_str())
        {
            return Err(format!("release {id} asset {name} differs from the inventory").into());
        }
    }
    let missing = wanted
        .keys()
        .filter(|name| !seen.contains(*name))
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    if stage == Stage::Resume {
        return Ok(missing);
    }
    if !missing.is_empty() {
        return Err(format!("release {id} lacks assets: {}", missing.join(", ")).into());
    }
    if stage == Stage::Published {
        if release["immutable"] != true {
            return Err(format!(
                "release {id} ({}) for {} at {} was published but is not immutable; enable immutable releases and publish a new version",
                release["html_url"].as_str().unwrap_or("unknown URL"),
                inventory.tag,
                inventory.revision
            )
            .into());
        }
        let latest = strict::parse(&archive::read(
            latest.ok_or("published check needs the latest release")?,
            4 * 1024 * 1024,
        )?)?;
        if latest["id"] != id {
            return Err(format!(
                "latest release is {}, not the published release {id}",
                latest["id"]
            )
            .into());
        }
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disposable directory removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("among-author-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    /// The host's own example manifest, with icon, settings and delivery
    /// channels, so the opaque handling is checked against fields the
    /// official profile does not know.
    fn host_example(id: &str, entrypoint: &str, icon: &str) -> Vec<u8> {
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1, "id": id, "version": "1.0.0", "name": "Sample status",
            "description": "Static manifest example", "entrypoint": entrypoint, "icon": icon,
            "api": {"major": 1, "minor": 2}, "required_features": ["status", "settings"],
            "optional_features": [],
            "settings": {"version": 1, "apply": "live", "fields": [
                {"key": "interval", "label": "Refresh seconds", "kind": "integer", "min": 1, "max": 60, "default": 5}
            ]},
            "delivery_channels": []
        }))
        .unwrap()
    }

    #[test]
    fn declared_plugins_keep_their_manifest_bytes_and_file_icon() {
        let root = Scratch::new("declared");
        let manifest = host_example("sample-status", "bin/sample-status", "icons/sample.png");
        write(
            &root.0.join("plugins/sample-status/among-plugin.json"),
            &manifest,
        );
        write(
            &root.0.join("plugins/sample-status/icons/sample.png"),
            b"png",
        );
        write(
            &root.0.join("plugins/theme-only/among-plugin.json"),
            &host_example(
                "theme-only",
                "bin/theme-only",
                "theme:utilities-terminal-symbolic",
            ),
        );
        let plugins = declared(&root.0).unwrap();
        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[0].id, "sample-status");
        assert_eq!(plugins[0].manifest, manifest);
        assert_eq!(plugins[0].entrypoint, "bin/sample-status");
        assert_eq!(
            plugins[0].icon,
            Some(("icons/sample.png".into(), b"png".to_vec()))
        );
        assert_eq!(plugins[1].icon, None);
    }

    /// Each layout would pack something other than what the manifest
    /// declares, or two plugins from one executable. The refusal must name
    /// the reason.
    #[test]
    fn plugin_layouts_that_pack_unexpected_files_are_refused() {
        type Layout = fn(&Path);
        let cases: [(&str, Layout, &str); 11] = [
            (
                "extra file",
                |root| {
                    write(
                        &root.join("plugins/a/among-plugin.json"),
                        &host_example("a", "bin/a", "theme:x"),
                    );
                    write(&root.join("plugins/a/README.md"), b"text");
                },
                "unsupported file in plugins/a: README.md",
            ),
            (
                "shared executable name",
                |root| {
                    write(
                        &root.join("plugins/a/among-plugin.json"),
                        &host_example("a", "bin/tool", "theme:x"),
                    );
                    write(
                        &root.join("plugins/b/among-plugin.json"),
                        &host_example("b", "libexec/tool", "theme:x"),
                    );
                },
                "both name the executable tool",
            ),
            (
                "directory is not an id",
                |root| {
                    write(
                        &root.join("plugins/Bad/among-plugin.json"),
                        &host_example("Bad", "bin/x", "theme:x"),
                    );
                },
                "plugin directory is not a plugin id: Bad",
            ),
            (
                "manifest names another id",
                |root| {
                    write(
                        &root.join("plugins/a/among-plugin.json"),
                        &host_example("b", "bin/a", "theme:x"),
                    );
                },
                "manifest needs schema_version 1",
            ),
            (
                "absolute entrypoint",
                |root| {
                    write(
                        &root.join("plugins/a/among-plugin.json"),
                        &host_example("a", "/usr/bin/a", "theme:x"),
                    );
                },
                "manifest needs schema_version 1",
            ),
            (
                "missing icon file",
                |root| {
                    write(
                        &root.join("plugins/a/among-plugin.json"),
                        &host_example("a", "bin/a", "icon.png"),
                    );
                },
                "plugins/a: icon icon.png is not a readable file",
            ),
            (
                "duplicate manifest key",
                |root| {
                    write(
                        &root.join("plugins/a/among-plugin.json"),
                        br#"{"schema_version":1,"id":"a","id":"a"}"#,
                    );
                },
                "duplicate JSON key: id",
            ),
            (
                "schema version 2",
                |root| {
                    let manifest =
                        String::from_utf8(host_example("a", "bin/a", "theme:x")).unwrap();
                    write(
                        &root.join("plugins/a/among-plugin.json"),
                        manifest
                            .replace("\"schema_version\": 1", "\"schema_version\": 2")
                            .as_bytes(),
                    );
                },
                "manifest needs schema_version 1",
            ),
            (
                "api major 2",
                |root| {
                    let manifest =
                        String::from_utf8(host_example("a", "bin/a", "theme:x")).unwrap();
                    write(
                        &root.join("plugins/a/among-plugin.json"),
                        manifest.replace("\"major\": 1", "\"major\": 2").as_bytes(),
                    );
                },
                "manifest needs schema_version 1",
            ),
            (
                "version with a space",
                |root| {
                    let manifest =
                        String::from_utf8(host_example("a", "bin/a", "theme:x")).unwrap();
                    write(
                        &root.join("plugins/a/among-plugin.json"),
                        manifest.replace("\"1.0.0\"", "\"1.0 beta\"").as_bytes(),
                    );
                },
                "manifest needs schema_version 1",
            ),
            (
                "33 plugins",
                |root| {
                    for index in 0..33 {
                        let id = format!("p{index}");
                        write(
                            &root.join(format!("plugins/{id}/among-plugin.json")),
                            &host_example(&id, &format!("bin/{id}"), "theme:x"),
                        );
                    }
                },
                "must hold 1 to 32 plugin directories",
            ),
        ];
        for (name, layout, reason) in cases {
            let root = Scratch::new("layout");
            layout(&root.0);
            let error = declared(&root.0).err().map(|error| error.to_string());
            assert!(
                error.as_deref().is_some_and(|error| error.contains(reason)),
                "{name}: {error:?}"
            );
        }
    }

    fn inventory_fixture(directory: &Path) -> (PathBuf, PathBuf) {
        let inventory = Inventory {
            repository_id: 7,
            tag: "v1.0.0".into(),
            revision: "b".repeat(40),
            inventory_sha256: "c".repeat(64),
            assets: vec![
                Asset {
                    name: "a-1.0.0-linux-amd64.tar.gz".into(),
                    size: 10,
                    sha256: "d".repeat(64),
                },
                Asset {
                    name: SUMS.into(),
                    size: 5,
                    sha256: "e".repeat(64),
                },
            ],
        };
        let path = directory.join("inventory.json");
        archive::json(&path, &inventory).unwrap();
        let line = directory.join("binding");
        binding(
            &path,
            "owner/repo/.github/workflows/release.yml@refs/tags/v1.0.0",
            99,
            &line,
        )
        .unwrap();
        (path, line)
    }

    fn release(
        directory: &Path,
        body: &str,
        draft: bool,
        assets: Value,
        immutable: bool,
    ) -> PathBuf {
        let path = directory.join(format!(
            "release-{}.json",
            fs::read_dir(directory).unwrap().count()
        ));
        fs::write(
            &path,
            serde_json::to_vec(&json!({"id": 41, "tag_name": "v1.0.0", "draft": draft,
                "prerelease": false, "immutable": immutable, "html_url": "https://example.invalid/r",
                "body": body, "assets": assets}))
            .unwrap(),
        )
        .unwrap();
        path
    }

    fn tagged(directory: &Path, body: &str, tag: &str, prerelease: bool, id: u64) -> PathBuf {
        let path = directory.join(format!(
            "tagged-{}.json",
            fs::read_dir(directory).unwrap().count()
        ));
        fs::write(
            &path,
            serde_json::to_vec(&json!({"id": id, "tag_name": tag, "draft": true,
                "prerelease": prerelease, "body": body, "assets": []}))
            .unwrap(),
        )
        .unwrap();
        path
    }

    fn asset(name: &str, size: u64, sha: &str) -> Value {
        json!({"name": name, "size": size, "state": "uploaded", "digest": format!("sha256:{sha}")})
    }

    #[test]
    fn only_a_draft_bound_to_this_run_and_inventory_is_resumed() {
        let scratch = Scratch::new("resume");
        let (inventory, binding) = inventory_fixture(&scratch.0);
        let line = fs::read_to_string(&binding).unwrap();
        let package = asset("a-1.0.0-linux-amd64.tar.gz", 10, &"d".repeat(64));
        let ours = release(&scratch.0, &line, true, json!([package.clone()]), false);
        assert_eq!(
            verify_release(Stage::Resume, &ours, &inventory, &binding, None).unwrap(),
            vec![SUMS.to_owned()]
        );
        let other_run = line.replace("\"run_id\":99", "\"run_id\":98");
        let cases = [
            (
                "draft without binding",
                release(&scratch.0, "", true, json!([]), false),
                "carries no binding",
            ),
            (
                "draft of another run",
                release(&scratch.0, &other_run, true, json!([]), false),
                "bound to another",
            ),
            (
                "published release",
                release(&scratch.0, &line, false, json!([]), true),
                "is not the draft release",
            ),
            (
                "unexpected asset",
                release(
                    &scratch.0,
                    &line,
                    true,
                    json!([asset("x.tar.gz", 1, &"d".repeat(64))]),
                    false,
                ),
                "unexpected asset x.tar.gz",
            ),
            (
                "asset with other bytes",
                release(
                    &scratch.0,
                    &line,
                    true,
                    json!([asset("a-1.0.0-linux-amd64.tar.gz", 10, &"f".repeat(64))]),
                    false,
                ),
                "differs from the inventory",
            ),
            (
                "two bindings",
                release(&scratch.0, &format!("{line}{line}"), true, json!([]), false),
                "more than one binding",
            ),
            (
                "asset with another size",
                release(
                    &scratch.0,
                    &line,
                    true,
                    json!([asset("a-1.0.0-linux-amd64.tar.gz", 11, &"d".repeat(64))]),
                    false,
                ),
                "differs from the inventory",
            ),
            (
                "asset still uploading",
                release(
                    &scratch.0,
                    &line,
                    true,
                    json!([{"name": "a-1.0.0-linux-amd64.tar.gz", "size": 10, "state": "starter", "digest": format!("sha256:{}", "d".repeat(64))}]),
                    false,
                ),
                "differs from the inventory",
            ),
            (
                "asset without digest",
                release(
                    &scratch.0,
                    &line,
                    true,
                    json!([{"name": "a-1.0.0-linux-amd64.tar.gz", "size": 10, "state": "uploaded", "digest": null}]),
                    false,
                ),
                "differs from the inventory",
            ),
            (
                "repeated asset name",
                release(
                    &scratch.0,
                    &line,
                    true,
                    json!([package.clone(), package.clone()]),
                    false,
                ),
                "differs from the inventory",
            ),
            (
                "too many assets",
                release(
                    &scratch.0,
                    &line,
                    true,
                    Value::Array(vec![package.clone(); 65]),
                    false,
                ),
                "more than 64 assets",
            ),
            (
                "release of another tag",
                tagged(&scratch.0, &line, "v2.0.0", false, 41),
                "is not the draft release of v1.0.0",
            ),
            (
                "prerelease draft",
                tagged(&scratch.0, &line, "v1.0.0", true, 41),
                "is not the draft release of v1.0.0",
            ),
            (
                "release without ID",
                tagged(&scratch.0, &line, "v1.0.0", false, 0),
                "release has no ID",
            ),
        ];
        for (name, path, reason) in cases {
            let error = verify_release(Stage::Resume, &path, &inventory, &binding, None)
                .err()
                .map(|error| error.to_string());
            assert!(
                error.as_deref().is_some_and(|error| error.contains(reason)),
                "{name}: {error:?}"
            );
        }
    }

    #[test]
    fn publication_needs_every_asset_immutability_and_latest() {
        let scratch = Scratch::new("published");
        let (inventory, binding) = inventory_fixture(&scratch.0);
        let line = fs::read_to_string(&binding).unwrap();
        let all = json!([
            asset("a-1.0.0-linux-amd64.tar.gz", 10, &"d".repeat(64)),
            asset(SUMS, 5, &"e".repeat(64))
        ]);
        let draft = release(&scratch.0, &line, true, all.clone(), false);
        assert!(
            verify_release(Stage::Draft, &draft, &inventory, &binding, None)
                .unwrap()
                .is_empty()
        );
        let partial = release(
            &scratch.0,
            &line,
            true,
            json!([asset(SUMS, 5, &"e".repeat(64))]),
            false,
        );
        assert!(
            verify_release(Stage::Draft, &partial, &inventory, &binding, None)
                .unwrap_err()
                .to_string()
                .contains("lacks assets: a-1.0.0-linux-amd64.tar.gz")
        );
        let latest = scratch.0.join("latest.json");
        fs::write(&latest, br#"{"id":41}"#).unwrap();
        let published = release(&scratch.0, &line, false, all.clone(), true);
        assert!(verify_release(
            Stage::Published,
            &published,
            &inventory,
            &binding,
            Some(&latest)
        )
        .is_ok());
        let mutable = release(&scratch.0, &line, false, all, false);
        let error = verify_release(
            Stage::Published,
            &mutable,
            &inventory,
            &binding,
            Some(&latest),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("is not immutable") && error.contains("https://example.invalid/r"),
            "{error}"
        );
        fs::write(&latest, br#"{"id":40}"#).unwrap();
        assert!(verify_release(
            Stage::Published,
            &published,
            &inventory,
            &binding,
            Some(&latest)
        )
        .unwrap_err()
        .to_string()
        .contains("latest release is 40"));
    }
}

/// Synthetic builds for `load` and `catalogue`: real archives, a minimal ELF
/// executable that `package::elf` accepts and a stand-in toolchain record,
/// since real Rust notices exist only on a native CI runner.
#[cfg(test)]
mod build_tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("among-author-build-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    /// One loadable segment holding the entry point and the glibc loader as
    /// interpreter: the smallest file `package::elf` accepts.
    fn elf(architecture: &str) -> Vec<u8> {
        let (machine, loader): (u16, &[u8]) = match architecture {
            "amd64" => (62, b"/lib64/ld-linux-x86-64.so.2\0"),
            _ => (183, b"/lib/ld-linux-aarch64.so.1\0"),
        };
        let mut bytes = vec![0u8; 64 + 2 * 56];
        // ELF64, little endian, current version.
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        let mut put = |offset: usize, value: u64, length: usize| {
            bytes[offset..offset + length].copy_from_slice(&value.to_le_bytes()[..length]);
        };
        put(16, 3, 2);
        put(18, u64::from(machine), 2);
        put(20, 1, 4);
        put(24, 0x1000, 8);
        put(32, 64, 8);
        put(52, 64, 2);
        put(54, 56, 2);
        put(56, 2, 2);
        put(64, 1, 4);
        put(68, 5, 4);
        put(80, 0x1000, 8);
        put(96, 64, 8);
        put(104, 0x100, 8);
        put(120, 3, 4);
        put(124, 4, 4);
        put(128, 176, 8);
        put(152, loader.len() as u64, 8);
        bytes.extend_from_slice(loader);
        bytes
    }

    fn manifest(id: &str, description: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({"schema_version":1,"id":id,"version":"1.0.0",
            "name":id,"description":description,"entrypoint":format!("bin/{id}"),
            "api":{"major":1,"minor":2},"required_features":["status"],"optional_features":[]}))
        .unwrap()
    }

    fn toolchain_record() -> toolchain::Record {
        serde_json::from_value(json!({"release":"1.98.1","commit":REVISION,"host":"synthetic",
            "compiler_sha256":"a".repeat(64),"components":[],"runtime_libraries":[],"common_notices":[]}))
        .unwrap()
    }

    fn accept(_: &toolchain::Record, _: &str, _: &BTreeMap<String, Vec<u8>>) -> Result<()> {
        Ok(())
    }

    /// Writes a build directory for `architecture` with the given plugins
    /// (id, manifest bytes) and the same source archive bytes on every call.
    fn build(root: &Path, architecture: &str, plugins: &[(&str, Vec<u8>)]) -> PathBuf {
        build_from(root, architecture, plugins, b"text")
    }

    fn build_from(
        root: &Path,
        architecture: &str,
        plugins: &[(&str, Vec<u8>)],
        source_text: &[u8],
    ) -> PathBuf {
        let directory = root.join(architecture);
        fs::create_dir(&directory).unwrap();
        let source_archive = source_archive_name(REVISION);
        let source =
            archive::Files::from([("source/README".to_owned(), (source_text.to_vec(), false))]);
        archive::pack(&directory.join(&source_archive), &source).unwrap();
        let source_sha256 = archive::hash(&fs::read(directory.join(&source_archive)).unwrap());
        let record = source_record(7, REVISION, &source_archive, &source_sha256);
        let mut packages = Vec::new();
        for (id, manifest) in plugins {
            let files = archive::Files::from([
                ("among-plugin.json".to_owned(), (manifest.clone(), false)),
                (format!("bin/{id}"), (elf(architecture), true)),
                (
                    "SOURCE.json".to_owned(),
                    (serde_json::to_vec_pretty(&record).unwrap(), false),
                ),
            ]);
            let name = package_name(id, "1.0.0", architecture);
            let members = archive::pack(&directory.join(&name), &files).unwrap();
            let bytes = fs::read(directory.join(&name)).unwrap();
            packages.push(Package {
                id: (*id).to_owned(),
                name,
                manifest_sha256: archive::hash(manifest),
                artifact: Artifact {
                    os: "linux".into(),
                    architecture: architecture.into(),
                    runtime: Runtime {
                        kind: "native".into(),
                        libc: "glibc".into(),
                        minimum: "2.34".into(),
                    },
                    dependencies: Vec::new(),
                    format: "tar.gz".into(),
                    size: bytes.len() as u64,
                    sha256: archive::hash(&bytes),
                    members,
                },
                needed_libraries: vec!["libc.so.6".into()],
            });
        }
        archive::json(
            &directory.join("build.json"),
            &Build {
                schema: BUILD_SCHEMA.into(),
                schema_version: 1,
                repository_id: 7,
                revision: REVISION.into(),
                architecture: architecture.into(),
                source_archive,
                source_sha256,
                packages,
                toolchain: toolchain_record(),
            },
        )
        .unwrap();
        directory
    }

    fn edit_build(directory: &Path, edit: impl Fn(&mut Value)) {
        let path = directory.join("build.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        edit(&mut value);
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    fn plugins() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("alpha", manifest("alpha", "first")),
            ("beta", manifest("beta", "second")),
        ]
    }

    #[test]
    fn both_architectures_make_a_schema_2_catalogue() {
        let root = Scratch::new("ok");
        let amd64 = build(&root.0, "amd64", &plugins());
        let arm64 = build(&root.0, "arm64", &plugins());
        let output = root.0.join("release");
        catalogue_with(&amd64, &arm64, 7, "v1.0.0", REVISION, &output, accept).unwrap();
        let catalogue: Value =
            serde_json::from_slice(&fs::read(output.join("assets").join(CATALOGUE)).unwrap())
                .unwrap();
        assert_eq!(catalogue["schema_version"], 2);
        assert_eq!(catalogue["plugins"].as_array().unwrap().len(), 2);
        assert_eq!(
            catalogue["plugins"][0]["versions"][0]["artifacts"][1]["asset"],
            "alpha-1.0.0-linux-arm64.tar.gz"
        );
        let inventory = read_inventory(&output.join("inventory.json")).unwrap();
        assert_eq!(inventory.assets.len(), 7);
        let sums = fs::read_to_string(output.join("assets").join(SUMS)).unwrap();
        assert_eq!(sums.lines().count(), 6);
        assert!(!sums.contains(SUMS));
        assert_eq!(inventory.inventory_sha256, archive::hash(sums.as_bytes()));
    }

    fn prepared(root: &Path) -> PathBuf {
        let amd64 = build(root, "amd64", &plugins());
        let arm64 = build(root, "arm64", &plugins());
        let output = root.join("prepared");
        catalogue_with(&amd64, &arm64, 7, "check", REVISION, &output, accept).unwrap();
        output
    }

    fn edit_inventory(prepared: &Path, edit: impl Fn(&mut Value)) {
        let path = prepared.join("inventory.json");
        let mut value = strict::parse(&fs::read(&path).unwrap()).unwrap();
        edit(&mut value);
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    #[test]
    fn prepared_inventory_retags_metadata_without_changing_approved_bytes() {
        let root = Scratch::new("prepared-ok");
        let prepared = prepared(&root.0);
        let inventory_before = fs::read(prepared.join("inventory.json")).unwrap();
        let inventory = read_inventory(&prepared.join("inventory.json")).unwrap();
        let before = inventory
            .assets
            .iter()
            .map(|asset| {
                (
                    asset.name.clone(),
                    fs::read(prepared.join("assets").join(&asset.name)).unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let output = root.0.join("publication.json");
        prepared_inventory(
            &prepared,
            7,
            REVISION,
            "v1.0.0",
            &inventory.inventory_sha256,
            &output,
        )
        .unwrap();
        let after = read_inventory(&output).unwrap();
        assert_eq!(after.tag, "v1.0.0");
        assert_eq!(after.repository_id, 7);
        assert_eq!(after.revision, REVISION);
        assert_eq!(after.inventory_sha256, inventory.inventory_sha256);
        assert_eq!(
            serde_json::to_value(&after.assets).unwrap(),
            serde_json::to_value(&inventory.assets).unwrap()
        );
        assert_eq!(
            fs::read(prepared.join("inventory.json")).unwrap(),
            inventory_before
        );
        for (name, bytes) in before {
            assert_eq!(fs::read(prepared.join("assets").join(name)).unwrap(), bytes);
        }
        assert!(prepared_inventory(
            &prepared,
            7,
            REVISION,
            "v1.0.0",
            &inventory.inventory_sha256,
            &output
        )
        .is_err());
        assert_eq!(read_inventory(&output).unwrap().tag, "v1.0.0");
    }

    #[test]
    fn prepared_inventory_refuses_damage_before_creating_output() {
        type Damage = fn(&Path);
        let cases: &[(&str, Damage)] = &[
            ("changed asset", |root| {
                fs::write(
                    root.join("assets/alpha-1.0.0-linux-amd64.tar.gz"),
                    b"changed",
                )
                .unwrap();
            }),
            ("missing asset", |root| {
                fs::remove_file(root.join("assets/alpha-1.0.0-linux-amd64.tar.gz")).unwrap();
            }),
            ("extra asset", |root| {
                fs::write(root.join("assets/unreviewed"), b"extra").unwrap();
            }),
            ("asset directory", |root| {
                fs::create_dir(root.join("assets/unreviewed")).unwrap();
            }),
            ("asset symlink", |root| {
                let path = root.join("assets/alpha-1.0.0-linux-amd64.tar.gz");
                let target = root.join("outside");
                fs::rename(&path, &target).unwrap();
                std::os::unix::fs::symlink(target, path).unwrap();
            }),
            ("assets symlink", |root| {
                fs::rename(root.join("assets"), root.join("outside")).unwrap();
                std::os::unix::fs::symlink("outside", root.join("assets")).unwrap();
            }),
            ("inventory symlink", |root| {
                fs::rename(root.join("inventory.json"), root.join("outside.json")).unwrap();
                std::os::unix::fs::symlink("outside.json", root.join("inventory.json")).unwrap();
            }),
            ("duplicate asset", |root| {
                edit_inventory(root, |value| {
                    let first = value["assets"][0].clone();
                    value["assets"].as_array_mut().unwrap().push(first);
                })
            }),
            ("path traversal", |root| {
                edit_inventory(root, |value| {
                    value["assets"][0]["name"] = "../outside".into()
                })
            }),
            ("empty inventory", |root| {
                edit_inventory(root, |value| value["assets"] = json!([]))
            }),
            ("65 assets", |root| {
                edit_inventory(root, |value| {
                    value["assets"] = Value::Array(vec![value["assets"][0].clone(); 65])
                })
            }),
            ("wrong size", |root| {
                edit_inventory(root, |value| value["assets"][0]["size"] = 1.into())
            }),
            ("zero size", |root| {
                edit_inventory(root, |value| value["assets"][0]["size"] = 0.into())
            }),
            ("oversize", |root| {
                edit_inventory(root, |value| {
                    value["assets"][0]["size"] = (archive::MAX_FILE + 1).into()
                })
            }),
            ("malformed digest", |root| {
                edit_inventory(root, |value| {
                    value["assets"][0]["sha256"] = "A".repeat(64).into()
                })
            }),
            ("wrong repository", |root| {
                edit_inventory(root, |value| value["repository_id"] = 8.into())
            }),
            ("wrong revision", |root| {
                edit_inventory(root, |value| value["revision"] = "a".repeat(40).into())
            }),
            ("changed sums", |root| {
                let path = root.join("assets").join(SUMS);
                let text = fs::read_to_string(&path).unwrap();
                fs::write(path, text.lines().rev().collect::<Vec<_>>().join("\n")).unwrap();
            }),
            ("duplicate inventory key", |root| {
                let path = root.join("inventory.json");
                let text = fs::read_to_string(&path).unwrap();
                fs::write(path, text.replacen('{', "{\"repository_id\":7,", 1)).unwrap();
            }),
        ];
        for (name, damage) in cases {
            let root = Scratch::new("prepared-damage");
            let prepared = prepared(&root.0);
            let digest = read_inventory(&prepared.join("inventory.json"))
                .unwrap()
                .inventory_sha256;
            damage(&prepared);
            let output = root.0.join("publication.json");
            assert!(
                prepared_inventory(&prepared, 7, REVISION, "v1.0.0", &digest, &output).is_err(),
                "{name}"
            );
            assert!(!output.exists(), "{name} created output");
        }
    }

    #[test]
    fn prepared_inventory_requires_exact_approval_and_source_even_with_valid_assets() {
        let root = Scratch::new("prepared-inputs");
        let prepared = prepared(&root.0);
        let digest = read_inventory(&prepared.join("inventory.json"))
            .unwrap()
            .inventory_sha256;
        let output = root.0.join("publication.json");
        for (repository, revision, tag, approval) in [
            (0, REVISION, "v1.0.0", digest.as_str()),
            (8, REVISION, "v1.0.0", digest.as_str()),
            (7, "short", "v1.0.0", digest.as_str()),
            (7, REVISION, "v 1", digest.as_str()),
            (7, REVISION, "v1.0.0", "bad"),
            (7, REVISION, "v1.0.0", &"a".repeat(64)),
        ] {
            assert!(
                prepared_inventory(&prepared, repository, revision, tag, approval, &output)
                    .is_err()
            );
            assert!(!output.exists());
        }
        // Rewriting unapproved metadata cannot reassign the approved packages.
        edit_inventory(&prepared, |value| value["repository_id"] = 8.into());
        let error =
            prepared_inventory(&prepared, 8, REVISION, "v1.0.0", &digest, &output).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("manifest or source binding differs"),
            "{error}"
        );
        assert!(!output.exists());
    }

    #[test]
    fn approved_checksums_do_not_replace_catalogue_binding_checks() {
        type Damage = fn(&mut Value);
        let cases: &[(&str, Damage)] = &[
            ("schema1", |value| value["schema_version"] = 1.into()),
            ("empty plugins", |value| value["plugins"] = json!([])),
            ("wrong revision", |value| {
                value["plugins"][0]["versions"][0]["revision"] = "a".repeat(40).into()
            }),
            ("two versions", |value| {
                let version = value["plugins"][0]["versions"][0].clone();
                value["plugins"][0]["versions"]
                    .as_array_mut()
                    .unwrap()
                    .push(version);
            }),
            ("unlisted asset", |value| {
                value["plugins"][0]["versions"][0]["artifacts"][0]["asset"] = "absent.tar.gz".into()
            }),
            ("wrong size", |value| {
                value["plugins"][0]["versions"][0]["artifacts"][0]["size"] = 1.into()
            }),
            ("wrong digest", |value| {
                value["plugins"][0]["versions"][0]["artifacts"][0]["sha256"] = "a".repeat(64).into()
            }),
            ("wrong manifest", |value| {
                value["plugins"][0]["versions"][0]["manifest"]["description"] = "changed".into()
            }),
            ("missing architecture", |value| {
                value["plugins"][0]["versions"][0]["artifacts"]
                    .as_array_mut()
                    .unwrap()
                    .pop();
            }),
            ("unreferenced packages", |value| {
                value["plugins"].as_array_mut().unwrap().pop();
            }),
        ];
        for (name, damage) in cases {
            let root = Scratch::new("prepared-catalogue");
            let prepared = prepared(&root.0);
            let path = prepared.join("assets").join(CATALOGUE);
            let mut value = strict::parse(&fs::read(&path).unwrap()).unwrap();
            damage(&mut value);
            fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
            let mut inventory = read_inventory(&prepared.join("inventory.json")).unwrap();
            for asset in &mut inventory.assets {
                let bytes = fs::read(prepared.join("assets").join(&asset.name)).unwrap();
                asset.size = bytes.len() as u64;
                asset.sha256 = archive::hash(&bytes);
            }
            let sums = inventory
                .assets
                .iter()
                .filter(|asset| asset.name != SUMS)
                .map(|asset| format!("{}  {}\n", asset.sha256, asset.name))
                .collect::<String>();
            fs::write(prepared.join("assets").join(SUMS), &sums).unwrap();
            let digest = archive::hash(sums.as_bytes());
            let asset = inventory
                .assets
                .iter_mut()
                .find(|asset| asset.name == SUMS)
                .unwrap();
            asset.size = sums.len() as u64;
            asset.sha256 = digest.clone();
            inventory.inventory_sha256 = digest.clone();
            fs::write(
                prepared.join("inventory.json"),
                serde_json::to_vec(&inventory).unwrap(),
            )
            .unwrap();
            let output = root.0.join("publication.json");
            assert!(
                prepared_inventory(&prepared, 7, REVISION, "v1.0.0", &digest, &output).is_err(),
                "{name}"
            );
            assert!(!output.exists(), "{name} created output");
        }
    }

    #[test]
    fn production_load_runs_the_real_toolchain_check() {
        let root = Scratch::new("toolchain");
        let amd64 = build(&root.0, "amd64", &plugins());
        let arm64 = build(&root.0, "arm64", &plugins());
        let error = catalogue(&amd64, &arm64, 7, "v1.0.0", REVISION, &root.0.join("out"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid Rust runtime provenance"), "{error}");
    }

    /// Each damage is a build that is not what the expected repository,
    /// revision and plugin set produced. The refusal must name the reason.
    #[test]
    fn damaged_builds_are_refused_before_any_output() {
        type Damage = fn(&Path, &Path);
        let cases: [(&str, Damage, &str); 9] = [
            (
                "source archive bytes",
                |_, arm64| {
                    let path = arm64.join(source_archive_name(REVISION));
                    let mut bytes = fs::read(&path).unwrap();
                    bytes.push(0);
                    fs::write(path, bytes).unwrap();
                },
                "arm64 source archive differs from its record",
            ),
            (
                "duplicate plugin",
                |_, arm64| {
                    edit_build(arm64, |build| {
                        let first = build["packages"][0].clone();
                        build["packages"].as_array_mut().unwrap().push(first);
                    })
                },
                "arm64 package alpha profile mismatch",
            ),
            (
                "name with another version",
                |_, arm64| {
                    fs::rename(
                        arm64.join("alpha-1.0.0-linux-arm64.tar.gz"),
                        arm64.join("alpha-1.0.1-linux-arm64.tar.gz"),
                    )
                    .unwrap();
                    edit_build(arm64, |build| {
                        build["packages"][0]["name"] = "alpha-1.0.1-linux-arm64.tar.gz".into()
                    });
                },
                "manifest, name or source record mismatch",
            ),
            (
                "name outside the build",
                |_, arm64| {
                    edit_build(arm64, |build| {
                        build["packages"][0]["name"] = "alpha-../../x-linux-arm64.tar.gz".into()
                    })
                },
                "is not a package of alpha",
            ),
            (
                "package of another architecture",
                |amd64, arm64| {
                    fs::copy(
                        amd64.join("alpha-1.0.0-linux-amd64.tar.gz"),
                        arm64.join("alpha-1.0.0-linux-arm64.tar.gz"),
                    )
                    .unwrap();
                    let bytes = fs::read(amd64.join("alpha-1.0.0-linux-amd64.tar.gz")).unwrap();
                    let members: Value = serde_json::from_slice::<Value>(
                        &fs::read(amd64.join("build.json")).unwrap(),
                    )
                    .unwrap()["packages"][0]["artifact"]["members"]
                        .clone();
                    edit_build(arm64, |build| {
                        build["packages"][0]["artifact"]["size"] = (bytes.len() as u64).into();
                        build["packages"][0]["artifact"]["sha256"] = archive::hash(&bytes).into();
                        build["packages"][0]["artifact"]["members"] = members.clone();
                    });
                },
                "not a matching Linux ELF64 executable",
            ),
            (
                "source archive replaced without repacking its packages",
                |_, arm64| {
                    let name = source_archive_name(REVISION);
                    let source = archive::Files::from([(
                        "source/README".to_owned(),
                        (b"other".to_vec(), false),
                    )]);
                    fs::remove_file(arm64.join(&name)).unwrap();
                    archive::pack(&arm64.join(&name), &source).unwrap();
                    let sha = archive::hash(&fs::read(arm64.join(&name)).unwrap());
                    edit_build(arm64, |build| build["source_sha256"] = sha.clone().into());
                },
                "manifest, name or source record mismatch",
            ),
            (
                "repository of the build",
                |_, arm64| edit_build(arm64, |build| build["repository_id"] = 8.into()),
                "arm64 build is not this repository",
            ),
            (
                "unknown build field",
                |_, arm64| edit_build(arm64, |build| build["extra"] = true.into()),
                "unknown field",
            ),
            (
                "duplicate key in build record",
                |_, arm64| {
                    let path = arm64.join("build.json");
                    let text = fs::read_to_string(&path).unwrap();
                    fs::write(&path, text.replacen("{", "{\"schema_version\":1,", 1)).unwrap();
                },
                "duplicate JSON key: schema_version",
            ),
        ];
        for (name, damage, reason) in cases {
            let root = Scratch::new("damage");
            let amd64 = build(&root.0, "amd64", &plugins());
            let arm64 = build(&root.0, "arm64", &plugins());
            damage(&amd64, &arm64);
            let output = root.0.join("release");
            let error = catalogue_with(&amd64, &arm64, 7, "v1.0.0", REVISION, &output, accept)
                .err()
                .map(|error| error.to_string());
            assert!(
                error.as_deref().is_some_and(|error| error.contains(reason)),
                "{name}: {error:?}"
            );
            assert!(
                !output.join("assets").join(CATALOGUE).exists(),
                "{name} wrote a catalogue"
            );
        }
    }

    #[test]
    fn architectures_must_ship_the_same_manifests() {
        let root = Scratch::new("manifests");
        let amd64 = build(&root.0, "amd64", &plugins());
        let arm64 = build(
            &root.0,
            "arm64",
            &[
                ("alpha", manifest("alpha", "changed")),
                ("beta", manifest("beta", "second")),
            ],
        );
        let error = catalogue_with(
            &amd64,
            &arm64,
            7,
            "v1.0.0",
            REVISION,
            &root.0.join("out"),
            accept,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("differ in source delivery, plugin set or manifest bytes"),
            "{error}"
        );
        let root = Scratch::new("sources");
        let amd64 = build(&root.0, "amd64", &plugins());
        let arm64 = build_from(&root.0, "arm64", &plugins(), b"other");
        assert!(catalogue_with(
            &amd64,
            &arm64,
            7,
            "v1.0.0",
            REVISION,
            &root.0.join("out"),
            accept
        )
        .unwrap_err()
        .to_string()
        .contains("differ in source delivery, plugin set or manifest bytes"));
        let root = Scratch::new("plugin-set");
        let amd64 = build(&root.0, "amd64", &plugins());
        let arm64 = build(&root.0, "arm64", &plugins()[..1]);
        assert!(catalogue_with(
            &amd64,
            &arm64,
            7,
            "v1.0.0",
            REVISION,
            &root.0.join("out"),
            accept
        )
        .unwrap_err()
        .to_string()
        .contains("differ in source delivery, plugin set or manifest bytes"));
    }

    #[test]
    fn release_limits_and_arguments_are_enforced() {
        let large = "x".repeat(200 * 1024);
        let many = (0..6)
            .map(|index| format!("big-{index}"))
            .collect::<Vec<_>>();
        let big = many
            .iter()
            .map(|id| (id.as_str(), manifest(id, &large)))
            .collect::<Vec<_>>();
        let root = Scratch::new("bytes");
        let amd64 = build(&root.0, "amd64", &big);
        let arm64 = build(&root.0, "arm64", &big);
        assert!(catalogue_with(
            &amd64,
            &arm64,
            7,
            "v1.0.0",
            REVISION,
            &root.0.join("out"),
            accept
        )
        .unwrap_err()
        .to_string()
        .contains("above the host's 1 MiB limit"));
        let ids = (0..31)
            .map(|index| format!("p-{index}"))
            .collect::<Vec<_>>();
        let set = ids
            .iter()
            .map(|id| (id.as_str(), manifest(id, "d")))
            .collect::<Vec<_>>();
        let root = Scratch::new("assets");
        let amd64 = build(&root.0, "amd64", &set);
        let arm64 = build(&root.0, "arm64", &set);
        assert!(catalogue_with(
            &amd64,
            &arm64,
            7,
            "v1.0.0",
            REVISION,
            &root.0.join("out"),
            accept
        )
        .unwrap_err()
        .to_string()
        .contains("65 release assets exceed the limit of 64"));
        for (repository, tag, revision) in [
            (0, "v1.0.0", REVISION),
            (7, "v 1", REVISION),
            (7, "v1.0.0", "short"),
        ] {
            assert!(catalogue_with(
                &amd64,
                &arm64,
                repository,
                tag,
                revision,
                &root.0.join("bad"),
                accept
            )
            .unwrap_err()
            .to_string()
            .contains("expected repository ID, revision and tag are malformed"));
        }
    }
}

#[cfg(test)]
mod binding_tests {
    use super::*;

    #[test]
    fn a_binding_must_describe_the_same_release_as_the_inventory() {
        let directory =
            std::env::temp_dir().join(format!("among-author-binding-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let inventory = directory.join("inventory.json");
        archive::json(
            &inventory,
            &Inventory {
                repository_id: 7,
                tag: "v1.0.0".into(),
                revision: "b".repeat(40),
                inventory_sha256: "c".repeat(64),
                assets: Vec::new(),
            },
        )
        .unwrap();
        for (workflow, run) in [
            ("has space", 1),
            ("", 1),
            ("owner/repo/w.yml@refs/tags/v1", 0),
        ] {
            assert!(binding(
                &inventory,
                workflow,
                run,
                &directory.join(format!("bad-{run}-{}", workflow.len()))
            )
            .unwrap_err()
            .to_string()
            .contains("workflow ref or run ID is malformed"));
        }
        let other = directory.join("other");
        let line = format!(
            "{BINDING_PREFIX}{}\n",
            serde_json::to_string(&Binding {
                repository_id: 7,
                workflow_ref: "w".into(),
                run_id: 1,
                tag: "v1.0.0".into(),
                commit: "b".repeat(40),
                inventory_sha256: "f".repeat(64),
            })
            .unwrap()
        );
        fs::write(&other, &line).unwrap();
        let release = directory.join("release.json");
        fs::write(
            &release,
            serde_json::to_vec(&json!({"id": 1, "tag_name": "v1.0.0", "draft": true,
            "prerelease": false, "body": line, "assets": []}))
            .unwrap(),
        )
        .unwrap();
        let error = verify_release(Stage::Resume, &release, &inventory, &other, None)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("binding and inventory describe different releases"),
            "{error}"
        );
        fs::remove_dir_all(&directory).unwrap();
    }
}

#[cfg(test)]
mod host_example_tests {
    use super::*;

    /// Among's `examples/managed-manifest.json` at the time of writing,
    /// including a number default, an enum and a secret field: the opaque
    /// handling must keep every byte the host's parser will read.
    const HOST_EXAMPLE: &str = r#"{
  "schema_version": 1,
  "id": "sample-status",
  "version": "1.0.0",
  "name": "Sample status",
  "description": "Static manifest and settings vocabulary example, not an installed plugin.",
  "entrypoint": "bin/sample-status",
  "icon": "theme:utilities-terminal-symbolic",
  "api": { "major": 1, "minor": 2 },
  "required_features": ["status", "settings"],
  "optional_features": [],
  "settings": {
    "version": 1,
    "apply": "live",
    "fields": [
      { "key": "enabled-meter", "label": "Show meter", "kind": "boolean", "default": true },
      { "key": "interval", "label": "Refresh seconds", "kind": "integer", "min": 1, "max": 60, "default": 5 },
      { "key": "threshold", "label": "Warning threshold", "kind": "number", "min": 0, "max": 1, "default": 0.8 },
      { "key": "label", "label": "Label", "kind": "string", "max_length": 64, "default": "Status" },
      { "key": "style", "label": "Style", "kind": "enum", "choices": [
        { "value": "plain", "label": "Plain" },
        { "value": "compact", "label": "Compact" }
      ], "default": "plain" },
      { "key": "credential", "label": "Credential (unavailable)", "kind": "secret", "required": false }
    ]
  },
  "delivery_channels": []
}
"#;

    #[test]
    fn the_host_example_manifest_is_packed_byte_for_byte() {
        let root = std::env::temp_dir().join(format!("among-author-host-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("plugins/sample-status")).unwrap();
        fs::write(
            root.join("plugins/sample-status/among-plugin.json"),
            HOST_EXAMPLE,
        )
        .unwrap();
        let plugins = declared(&root).unwrap();
        assert_eq!(plugins[0].manifest, HOST_EXAMPLE.as_bytes());
        assert_eq!(plugins[0].entrypoint, "bin/sample-status");
        assert_eq!(plugins[0].icon, None);
        let parsed = strict::parse(&plugins[0].manifest).unwrap();
        assert_eq!(parsed["settings"]["fields"][2]["default"], 0.8);
        assert_eq!(parsed["settings"]["fields"].as_array().unwrap().len(), 6);
        fs::remove_dir_all(&root).unwrap();
    }
}
