// Plugin versions are independent. A release is one snapshot of the whole
// repository whose catalogue lists exactly one version per plugin, so a plugin
// that did not change must keep its version and a plugin that did change must
// raise it. This module compares the plugins just built with the catalogue of
// the previous release and refuses both mistakes.

use crate::{archive, process, Result};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

const CATALOGUE_ASSET: &str = "among-plugins.json";
const MAX_CATALOGUE_BYTES: usize = 1024 * 1024;
const MANIFEST_MEMBER: &str = "among-plugin.json";

// Members that record where or with which toolchain a package was built, not
// what it contains. `SOURCE.json` names the source commit, so it differs on
// every commit; `THIRD_PARTY/RUST/native/` describes the runner's installed
// toolchain components, which drift with the runner image while the executable
// stays identical (measured on three commits of the plugin repository: only
// these members differed). The manifest is compared as parsed data without its
// version, because the version is exactly what the check is about.
fn build_record(path: &str) -> bool {
    path == "SOURCE.json" || path == MANIFEST_MEMBER || path.starts_with("THIRD_PARTY/RUST/native/")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Member {
    sha256: String,
    size: u64,
    executable: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Plugin {
    pub(crate) version: String,
    // The manifest without its `version`.
    manifest: Value,
    // Content members per architecture, build records removed.
    members: BTreeMap<String, BTreeMap<String, Member>>,
}

fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name]
        .as_str()
        .ok_or_else(|| format!("`{name}` is missing or not a string").into())
}

fn content_members(artifact: &Value) -> Result<BTreeMap<String, Member>> {
    let mut members = BTreeMap::new();
    for member in artifact["members"]
        .as_array()
        .ok_or("artifact has no member list")?
    {
        let path = text(member, "path")?;
        if build_record(path) {
            continue;
        }
        let entry = Member {
            sha256: text(member, "sha256")?.to_owned(),
            size: member["size"].as_u64().ok_or("member has no size")?,
            executable: member["executable"]
                .as_bool()
                .ok_or("member has no executable flag")?,
        };
        if members.insert(path.to_owned(), entry).is_some() {
            return Err(format!("member {path} is listed twice").into());
        }
    }
    Ok(members)
}

fn plugin_of(manifest: &Value, architecture: &str, artifact: &Value) -> Result<Plugin> {
    let mut manifest = manifest.clone();
    let version = manifest
        .as_object_mut()
        .and_then(|object| object.remove("version"))
        .and_then(|version| version.as_str().map(str::to_owned))
        .ok_or("manifest has no version")?;
    Ok(Plugin {
        version,
        manifest,
        members: BTreeMap::from([(architecture.to_owned(), content_members(artifact)?)]),
    })
}

// A release catalogue, schema 1 or 2: one version per plugin.
pub(crate) fn from_catalogue(catalogue: &Value) -> Result<BTreeMap<String, Plugin>> {
    match catalogue["schema_version"].as_u64() {
        Some(1 | 2) => {}
        other => return Err(format!("unsupported catalogue schema {other:?}").into()),
    }
    let mut plugins = BTreeMap::new();
    for entry in catalogue["plugins"]
        .as_array()
        .ok_or("catalogue has no plugin list")?
    {
        let id = text(entry, "id")?;
        let versions = entry["versions"]
            .as_array()
            .ok_or("plugin has no versions")?;
        let [version] = versions.as_slice() else {
            return Err(format!("{id}: a release catalogue lists exactly one version").into());
        };
        let mut plugin: Option<Plugin> = None;
        for artifact in version["artifacts"]
            .as_array()
            .ok_or("version has no artifacts")?
        {
            let architecture = text(artifact, "architecture")?;
            let one = plugin_of(&version["manifest"], architecture, artifact)?;
            match &mut plugin {
                None => plugin = Some(one),
                Some(known) => {
                    if known
                        .members
                        .insert(architecture.to_owned(), one.members[architecture].clone())
                        .is_some()
                    {
                        return Err(
                            format!("{id}: architecture {architecture} listed twice").into()
                        );
                    }
                }
            }
        }
        let plugin = plugin.ok_or_else(|| format!("{id}: version has no artifacts"))?;
        if plugins.insert(id.to_owned(), plugin).is_some() {
            return Err(format!("{id}: plugin listed twice").into());
        }
    }
    Ok(plugins)
}

// The `build.json` that `plugin-packages` writes for one architecture.
pub(crate) fn from_builds(builds: &[Value]) -> Result<BTreeMap<String, Plugin>> {
    let mut plugins: BTreeMap<String, Plugin> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for build in builds {
        let architecture = text(build, "architecture")?;
        if !seen.insert(architecture.to_owned()) {
            return Err(format!("architecture {architecture} given twice").into());
        }
        for package in build["packages"]
            .as_array()
            .ok_or("build record has no packages")?
        {
            let id = text(&package["manifest"], "id")?;
            let artifact = &package["artifact"];
            if text(artifact, "architecture")? != architecture {
                return Err(format!("{id}: package architecture differs from the build").into());
            }
            let one = plugin_of(&package["manifest"], architecture, artifact)?;
            match plugins.get_mut(id) {
                None => {
                    plugins.insert(id.to_owned(), one);
                }
                Some(known) => {
                    if known.version != one.version || known.manifest != one.manifest {
                        return Err(format!("{id}: manifests differ between architectures").into());
                    }
                    known
                        .members
                        .insert(architecture.to_owned(), one.members[architecture].clone());
                }
            }
        }
    }
    if plugins.is_empty() {
        return Err("the build records list no plugin".into());
    }
    Ok(plugins)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Identifier {
    Number(u64),
    Text(String),
}

// Semantic Versioning 2.0.0 precedence for `major.minor.patch[-prerelease]`.
// Build metadata is refused: a plugin version that differs only by metadata
// would compare equal and hide a missing raise.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Version {
    pub(crate) core: [u64; 3],
    pub(crate) pre: Vec<Identifier>,
}

fn number(part: &str) -> Option<u64> {
    let plain = !part.is_empty()
        && part.bytes().all(|byte| byte.is_ascii_digit())
        && (part == "0" || !part.starts_with('0'));
    plain.then(|| part.parse().ok()).flatten()
}

impl Version {
    pub(crate) fn parse(version: &str) -> Result<Self> {
        let fail = || format!("`{version}` is not a semantic version without build metadata");
        let (core, pre) = match version.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (version, None),
        };
        let parts: Vec<_> = core.split('.').map(number).collect();
        let [Some(major), Some(minor), Some(patch)] = parts[..] else {
            return Err(fail().into());
        };
        let mut identifiers = Vec::new();
        for part in pre
            .map(|pre| pre.split('.').collect::<Vec<_>>())
            .unwrap_or_default()
        {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                return Err(fail().into());
            }
            if part.bytes().all(|byte| byte.is_ascii_digit()) {
                identifiers.push(Identifier::Number(number(part).ok_or_else(fail)?));
            } else {
                identifiers.push(Identifier::Text(part.to_owned()));
            }
        }
        Ok(Self {
            core: [major, minor, patch],
            pre: identifiers,
        })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core
            .cmp(&other.core)
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (left, right) in self.pre.iter().zip(&other.pre) {
                        let order = match (left, right) {
                            (Identifier::Number(a), Identifier::Number(b)) => a.cmp(b),
                            (Identifier::Number(_), Identifier::Text(_)) => Ordering::Less,
                            (Identifier::Text(_), Identifier::Number(_)) => Ordering::Greater,
                            (Identifier::Text(a), Identifier::Text(b)) => a.cmp(b),
                        };
                        if order != Ordering::Equal {
                            return order;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// What differs between the previous and the current plugin, empty when equal.
// Only architectures that were just built are compared, so a job that packages
// one architecture does not call the other one changed.
pub(crate) fn differences(previous: &Plugin, current: &Plugin) -> Vec<String> {
    let mut found = Vec::new();
    if previous.manifest != current.manifest {
        found.push("manifest".to_owned());
    }
    for (architecture, now) in &current.members {
        let Some(before) = previous.members.get(architecture) else {
            found.push(format!("{architecture}: no previous package"));
            continue;
        };
        let paths: BTreeSet<_> = before.keys().chain(now.keys()).collect();
        for path in paths {
            if before.get(path) != now.get(path) {
                found.push(format!("{architecture}: {path}"));
            }
        }
    }
    found
}

#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub lines: Vec<String>,
    pub failures: usize,
}

// With `pending`, changed content at an unchanged version passes: that is the
// state of every ordinary pull request, because the version is raised once, by
// the release pull request. Everything else is refused in both modes.
fn compare(
    previous: &BTreeMap<String, Plugin>,
    current: &BTreeMap<String, Plugin>,
    pending: bool,
) -> Report {
    let mut report = Report {
        lines: Vec::new(),
        failures: 0,
    };
    let mut verdict = |ok: bool, id: &str, message: String| {
        if !ok {
            report.failures += 1;
        }
        report.lines.push(format!(
            "{} {id}: {message}",
            if ok { "ok  " } else { "FAIL" }
        ));
    };
    for (id, now) in current {
        let Some(before) = previous.get(id) else {
            verdict(true, id, format!("new plugin at {}", now.version));
            continue;
        };
        let (old, new) = match (
            Version::parse(&before.version),
            Version::parse(&now.version),
        ) {
            (Ok(old), Ok(new)) => (old, new),
            (Err(error), _) | (_, Err(error)) => {
                verdict(false, id, error.to_string());
                continue;
            }
        };
        let changes = differences(before, now);
        let shown = changes
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let more = changes.len().saturating_sub(5);
        let list = if more > 0 {
            format!("{shown} and {more} more")
        } else {
            shown
        };
        let (ok, message) = match (new.cmp(&old), changes.is_empty()) {
            (Ordering::Less, _) => (
                false,
                format!("version fell from {} to {}", before.version, now.version),
            ),
            (Ordering::Equal, true) => (true, format!("unchanged at {}", now.version)),
            (Ordering::Equal, false) if pending => (
                true,
                format!(
                    "packaged content changed ({list}); the release raises the version from {}",
                    now.version
                ),
            ),
            (Ordering::Equal, false) => (
                false,
                format!(
                    "packaged content changed ({list}) but the version is still {}",
                    now.version
                ),
            ),
            (Ordering::Greater, true) => (
                false,
                format!(
                    "version rose from {} to {} but the packaged content is unchanged",
                    before.version, now.version
                ),
            ),
            (Ordering::Greater, false) => (
                true,
                format!(
                    "changed ({list}); version {} -> {}",
                    before.version, now.version
                ),
            ),
        };
        verdict(ok, id, message);
    }
    for id in previous.keys().filter(|id| !current.contains_key(*id)) {
        verdict(
            true,
            id,
            "not in the current build; the next catalogue drops it".to_owned(),
        );
    }
    report
}

pub fn check(catalogue: &Value, builds: &[Value], pending: bool) -> Result<Report> {
    Ok(compare(
        &from_catalogue(catalogue)?,
        &from_builds(builds)?,
        pending,
    ))
}

pub(crate) fn read_json(path: &Path) -> Result<Value> {
    let bytes = std::fs::read(path)?;
    if bytes.len() > 32 * MAX_CATALOGUE_BYTES {
        return Err(format!("{} is unreasonably large", path.display()).into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn run(previous: &Path, builds: &[&Path], pending: bool) -> Result<()> {
    let catalogue = read_json(previous)?;
    let builds = builds
        .iter()
        .map(|path| read_json(path))
        .collect::<Result<Vec<_>>>()?;
    let report = check(&catalogue, &builds, pending)?;
    for line in &report.lines {
        println!("{line}");
    }
    if report.failures > 0 {
        return Err(format!(
            "{} plugin(s) have a version that does not match their content",
            report.failures
        )
        .into());
    }
    Ok(())
}

// The newest published release, a prerelease included, whose assets contain a
// catalogue. A prerelease counts because its bytes are already out: the host
// refuses the same version with different bytes. Drafts are not published and
// are skipped. Among the candidates of one page the newest is the one with the
// latest `published_at` (an ISO 8601 UTC time, so text order is time order),
// ties broken by the larger release id; the order GitHub lists them in is not
// relied on.
fn newest_with_catalogue(releases: &Value) -> Result<Option<String>> {
    let mut best: Option<(String, u64, String)> = None;
    for release in releases.as_array().ok_or("release list is not an array")? {
        if release["draft"]
            .as_bool()
            .ok_or("release has no draft flag")?
        {
            continue;
        }
        let has = release["assets"]
            .as_array()
            .ok_or("release has no asset list")?
            .iter()
            .any(|asset| asset["name"] == CATALOGUE_ASSET);
        if !has {
            continue;
        }
        let key = (
            text(release, "published_at")?.to_owned(),
            release["id"].as_u64().ok_or("release has no id")?,
            text(release, "tag_name")?.to_owned(),
        );
        if best
            .as_ref()
            .is_none_or(|known| (&key.0, key.1) > (&known.0, known.1))
        {
            best = Some(key);
        }
    }
    Ok(best.map(|(_, _, tag)| tag))
}

// The most pages of 100 releases that are read looking for a catalogue. A
// repository with more releases than this and none with a catalogue in them is
// reported instead of being treated as having no previous release.
const MAX_RELEASE_PAGES: u32 = 20;

// Reads release pages until one holds a published release with a catalogue.
fn newest_across_pages(mut page: impl FnMut(u32) -> Result<Value>) -> Result<Option<String>> {
    for number in 1..=MAX_RELEASE_PAGES {
        let releases = page(number)?;
        if releases
            .as_array()
            .ok_or("release list is not an array")?
            .is_empty()
        {
            return Ok(None);
        }
        if let Some(tag) = newest_with_catalogue(&releases)? {
            return Ok(Some(tag));
        }
    }
    Err(format!("no release with a catalogue in the newest {MAX_RELEASE_PAGES} pages").into())
}

pub(crate) fn valid_repository(repository: &str) -> Result<()> {
    if repository.split('/').count() != 2
        || repository.split('/').any(str::is_empty)
        || repository.starts_with('-')
        || !repository
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./".contains(&byte))
    {
        return Err("repository must look like owner/name".into());
    }
    Ok(())
}

// The tag of the previous release and its catalogue bytes, or no tag and an
// empty schema 2 catalogue when there is no release with a catalogue. The tag
// and the catalogue come from the same release by construction.
pub(crate) fn fetch_previous(repository: &str) -> Result<(Option<String>, Vec<u8>)> {
    valid_repository(repository)?;
    let newest = newest_across_pages(|number| {
        let releases = process::capture(
            Command::new("gh").args([
                "api",
                "--hostname",
                "github.com",
                &format!("repos/{repository}/releases?per_page=100&page={number}"),
            ]),
            &[],
            8 * MAX_CATALOGUE_BYTES,
        )?;
        Ok(serde_json::from_slice(&releases)?)
    })?;
    let bytes = match &newest {
        Some(tag) => {
            let bytes = process::capture(
                Command::new("gh").args([
                    "release",
                    "download",
                    tag,
                    "-R",
                    repository,
                    "-p",
                    CATALOGUE_ASSET,
                    "--output",
                    "-",
                ]),
                &[],
                MAX_CATALOGUE_BYTES,
            )?;
            from_catalogue(&serde_json::from_slice(&bytes)?)?;
            bytes
        }
        None => {
            let mut empty =
                serde_json::to_vec_pretty(&json!({"schema_version": 2, "plugins": []}))?;
            empty.push(b'\n');
            empty
        }
    };
    Ok((newest, bytes))
}

// Writes the catalogue of the previous release to `output`; when there is none
// the output is an empty schema 2 catalogue, so every plugin counts as new.
pub fn previous_catalogue(repository: &str, output: &Path) -> Result<()> {
    let (newest, bytes) = fetch_previous(repository)?;
    match &newest {
        Some(tag) => eprintln!("previous release: {tag}"),
        None => eprintln!("no published release has a catalogue: every plugin counts as new"),
    }
    archive::write_new(output, &bytes)?;
    // The tag of the previous release on stdout (an empty line when there is
    // none), for the release planner.
    println!("{}", newest.unwrap_or_default());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(path: &str, sha: &str, executable: bool) -> Value {
        json!({"path": path, "sha256": sha, "size": 10, "executable": executable})
    }

    fn manifest(id: &str, version: &str) -> Value {
        json!({"schema_version": 1, "id": id, "version": version, "name": id,
               "entrypoint": format!("bin/{id}"), "api": {"major": 1, "minor": 2}})
    }

    // One package of `id` as both a catalogue artifact and a build record.
    fn members(binary: &str, source: &str, toolchain: &str) -> Vec<Value> {
        vec![
            member("LICENSE", "license", false),
            member("SOURCE.json", source, false),
            member("THIRD_PARTY/RUST/native/BUILD.json", toolchain, false),
            member("THIRD_PARTY/DEPENDENCIES.json", "deps", false),
            member("among-plugin.json", "manifest-file", false),
            member("bin/clock", binary, true),
        ]
    }

    fn catalogue(version: &str, binary: &str, source: &str, toolchain: &str) -> Value {
        json!({"schema_version": 2, "plugins": [{"id": "clock", "versions": [{
            "manifest": manifest("clock", version),
            "artifacts": [{"architecture": "amd64", "members": members(binary, source, toolchain)}]}]}]})
    }

    fn build(version: &str, binary: &str, source: &str, toolchain: &str) -> Value {
        json!({"architecture": "amd64", "packages": [{
            "manifest": manifest("clock", version),
            "artifact": {"architecture": "amd64", "members": members(binary, source, toolchain)}}]})
    }

    fn verdict(previous: &Value, built: &Value) -> Report {
        check(previous, std::slice::from_ref(built), false).unwrap()
    }

    #[test]
    fn pending_mode_accepts_only_changed_content_at_the_same_version() {
        let previous = catalogue("0.2.0", "bin-a", "c", "r");
        let changed = build("0.2.0", "bin-b", "c", "r");
        let report = check(&previous, std::slice::from_ref(&changed), true).unwrap();
        assert_eq!(report.failures, 0, "{:?}", report.lines);
        // A raise without content and a falling version stay refused.
        let raised = build("0.2.1", "bin-a", "c", "r");
        assert_eq!(check(&previous, &[raised], true).unwrap().failures, 1);
        let lower = build("0.1.9", "bin-b", "c", "r");
        assert_eq!(check(&previous, &[lower], true).unwrap().failures, 1);
        // Strict mode still refuses the first case.
        assert_eq!(check(&previous, &[changed], false).unwrap().failures, 1);
    }

    #[test]
    fn same_content_at_the_same_version_passes_despite_new_build_records() {
        let report = verdict(
            &catalogue("0.2.0", "bin-a", "commit-1", "runner-1"),
            &build("0.2.0", "bin-a", "commit-2", "runner-2"),
        );
        assert_eq!(report.failures, 0, "{:?}", report.lines);
    }

    #[test]
    fn a_changed_executable_without_a_version_raise_fails() {
        let report = verdict(
            &catalogue("0.2.0", "bin-a", "c", "r"),
            &build("0.2.0", "bin-b", "c", "r"),
        );
        assert_eq!(report.failures, 1);
        assert!(
            report.lines[0].contains("amd64: bin/clock"),
            "{:?}",
            report.lines
        );
        assert!(report.lines[0].contains("version is still 0.2.0"));
    }

    #[test]
    fn a_changed_attribution_file_counts_as_a_content_change() {
        let mut built = build("0.2.0", "bin-a", "c", "r");
        built["packages"][0]["artifact"]["members"][0]["sha256"] = json!("another-license");
        let report = verdict(&catalogue("0.2.0", "bin-a", "c", "r"), &built);
        assert_eq!(report.failures, 1);
        assert!(report.lines[0].contains("amd64: LICENSE"));
    }

    #[test]
    fn a_version_raise_without_a_content_change_fails() {
        let report = verdict(
            &catalogue("0.2.0", "bin-a", "c", "r"),
            &build("0.2.1", "bin-a", "c2", "r2"),
        );
        assert_eq!(report.failures, 1);
        assert!(report.lines[0].contains("packaged content is unchanged"));
    }

    #[test]
    fn a_version_raise_with_a_content_change_passes() {
        let report = verdict(
            &catalogue("0.2.0", "bin-a", "c", "r"),
            &build("0.2.1", "bin-b", "c", "r"),
        );
        assert_eq!(report.failures, 0, "{:?}", report.lines);
    }

    #[test]
    fn a_version_that_falls_fails_even_with_changed_content() {
        let report = verdict(
            &catalogue("0.2.1", "bin-a", "c", "r"),
            &build("0.2.0", "bin-b", "c", "r"),
        );
        assert_eq!(report.failures, 1);
        assert!(report.lines[0].contains("fell from 0.2.1 to 0.2.0"));
    }

    #[test]
    fn a_manifest_change_other_than_the_version_is_a_content_change() {
        let mut built = build("0.2.0", "bin-a", "c", "r");
        built["packages"][0]["manifest"]["api"]["minor"] = json!(3);
        let report = verdict(&catalogue("0.2.0", "bin-a", "c", "r"), &built);
        assert_eq!(report.failures, 1);
        assert!(report.lines[0].contains("manifest"));
    }

    #[test]
    fn a_manifest_that_differs_only_by_version_is_not_a_content_change() {
        let report = verdict(
            &catalogue("0.2.0", "bin-a", "c", "r"),
            &build("0.2.1", "bin-a", "c", "r"),
        );
        assert_eq!(report.failures, 1, "the raise has no content behind it");
    }

    #[test]
    fn a_new_plugin_and_a_dropped_plugin_pass_with_a_note() {
        let empty = json!({"schema_version": 2, "plugins": []});
        let report = verdict(&empty, &build("0.1.0", "bin-a", "c", "r"));
        assert_eq!(report.failures, 0);
        assert!(report.lines[0].contains("new plugin at 0.1.0"));

        let mut other = build("0.1.0", "bin-a", "c", "r");
        other["packages"][0]["manifest"]["id"] = json!("pwd");
        let report = verdict(&catalogue("0.2.0", "bin-a", "c", "r"), &other);
        assert_eq!(report.failures, 0);
        assert!(report
            .lines
            .iter()
            .any(|line| line.contains("clock: not in the current build")));
    }

    #[test]
    fn an_architecture_without_a_previous_package_is_a_change() {
        let mut built = build("0.2.0", "bin-a", "c", "r");
        built["architecture"] = json!("arm64");
        built["packages"][0]["artifact"]["architecture"] = json!("arm64");
        let report = verdict(&catalogue("0.2.0", "bin-a", "c", "r"), &built);
        assert_eq!(report.failures, 1);
        assert!(report.lines[0].contains("arm64: no previous package"));
    }

    #[test]
    fn only_the_architectures_just_built_are_compared() {
        let mut previous = catalogue("0.2.0", "bin-a", "c", "r");
        let arm = json!({"architecture": "arm64", "members": members("arm-bin", "c", "r")});
        previous["plugins"][0]["versions"][0]["artifacts"]
            .as_array_mut()
            .unwrap()
            .push(arm);
        let report = verdict(&previous, &build("0.2.0", "bin-a", "c", "r"));
        assert_eq!(report.failures, 0, "{:?}", report.lines);
    }

    #[test]
    fn semantic_version_precedence() {
        let order = [
            "0.1.0-rc.1",
            "0.1.0-rc.2",
            "0.1.0-rc.10",
            "0.1.0-rc.10.1",
            "0.1.0",
            "0.1.1",
            "0.2.0-alpha",
            "0.2.0",
            "0.10.0",
            "1.0.0",
        ];
        for pair in order.windows(2) {
            let (low, high) = (
                Version::parse(pair[0]).unwrap(),
                Version::parse(pair[1]).unwrap(),
            );
            assert!(low < high, "{} < {}", pair[0], pair[1]);
            assert!(high > low, "{} > {}", pair[1], pair[0]);
        }
        assert!(Version::parse("1.0.0-1").unwrap() < Version::parse("1.0.0-alpha").unwrap());
    }

    #[test]
    fn malformed_versions_are_refused() {
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.2.3-",
            "1.2.3-01",
            "1.2.3+build",
            "v1.2.3",
            "1.2.x",
            "1.2.3-a..b",
        ] {
            assert!(Version::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_malformed_current_version_fails_instead_of_passing() {
        let report = verdict(
            &catalogue("0.2.0", "bin-a", "c", "r"),
            &build("0.2", "bin-a", "c", "r"),
        );
        assert_eq!(report.failures, 1);
    }

    #[test]
    fn catalogues_with_several_versions_or_an_unknown_schema_are_refused() {
        let mut two = catalogue("0.2.0", "bin-a", "c", "r");
        let first = two["plugins"][0]["versions"][0].clone();
        two["plugins"][0]["versions"]
            .as_array_mut()
            .unwrap()
            .push(first);
        assert!(from_catalogue(&two).is_err());
        let mut schema = catalogue("0.2.0", "bin-a", "c", "r");
        schema["schema_version"] = json!(3);
        assert!(from_catalogue(&schema).is_err());
    }

    #[test]
    fn schema_one_catalogues_are_read() {
        let mut old = catalogue("0.1.0-rc.1", "bin-a", "c", "r");
        old["schema_version"] = json!(1);
        let report = verdict(&old, &build("0.2.0", "bin-b", "c", "r"));
        assert_eq!(report.failures, 0, "{:?}", report.lines);
    }

    #[test]
    fn build_records_for_the_same_architecture_twice_are_refused() {
        let one = build("0.2.0", "bin-a", "c", "r");
        assert!(check(
            &catalogue("0.2.0", "bin-a", "c", "r"),
            &[one.clone(), one],
            false
        )
        .is_err());
    }

    #[test]
    fn manifests_that_differ_between_architectures_are_refused() {
        let amd = build("0.2.0", "bin-a", "c", "r");
        let mut arm = build("0.2.1", "bin-a", "c", "r");
        arm["architecture"] = json!("arm64");
        arm["packages"][0]["artifact"]["architecture"] = json!("arm64");
        assert!(from_builds(&[amd, arm]).is_err());
    }

    #[test]
    fn the_previous_release_is_the_newest_published_one_with_a_catalogue() {
        let release = |tag: &str, id: u64, at: &str, draft: bool, asset: &str| {
            json!({"tag_name": tag, "id": id, "published_at": at, "draft": draft,
                   "assets": [{"name": asset}, {"name": "clock-linux-amd64.tar.gz"}]})
        };
        // The list is deliberately not in time order: the newest by
        // `published_at` wins wherever it sits, drafts and releases without a
        // catalogue never do.
        let list = json!([
            release("v0.1.0", 1, "2026-01-01T00:00:00Z", false, CATALOGUE_ASSET),
            release("v0.3.0", 9, "2026-09-01T00:00:00Z", true, CATALOGUE_ASSET),
            release("v0.2.1", 8, "2026-08-01T00:00:00Z", false, "other.json"),
            release(
                "v0.2.0-rc.1",
                5,
                "2026-05-01T00:00:00Z",
                false,
                CATALOGUE_ASSET
            ),
            release(
                "v0.2.0-rc.2",
                6,
                "2026-06-01T00:00:00Z",
                false,
                CATALOGUE_ASSET
            ),
        ]);
        assert_eq!(
            newest_with_catalogue(&list).unwrap().as_deref(),
            Some("v0.2.0-rc.2")
        );
        // The same instant is broken by the larger release id.
        let tie = json!([
            release("a", 4, "2026-05-01T00:00:00Z", false, CATALOGUE_ASSET),
            release("b", 7, "2026-05-01T00:00:00Z", false, CATALOGUE_ASSET),
            release("c", 3, "2026-05-01T00:00:00Z", false, CATALOGUE_ASSET),
        ]);
        assert_eq!(newest_with_catalogue(&tie).unwrap().as_deref(), Some("b"));
        assert_eq!(newest_with_catalogue(&json!([])).unwrap(), None);
        assert_eq!(
            newest_with_catalogue(&json!([release(
                "v1",
                1,
                "2026-01-01T00:00:00Z",
                true,
                CATALOGUE_ASSET
            )]))
            .unwrap(),
            None
        );
    }

    #[test]
    fn pages_are_read_until_a_release_with_a_catalogue_is_found() {
        let plain = |tag: &str| {
            json!({"tag_name": tag, "id": 1, "published_at": "2026-01-01T00:00:00Z",
                   "draft": false, "assets": [{"name": "other.json"}]})
        };
        let with = json!({"tag_name": "v0.2.0", "id": 2, "published_at": "2026-02-01T00:00:00Z",
                          "draft": false, "assets": [{"name": CATALOGUE_ASSET}]});
        let mut asked = Vec::new();
        let found = newest_across_pages(|number| {
            asked.push(number);
            Ok(match number {
                1 => json!([plain("a"), plain("b")]),
                2 => json!([plain("c"), with.clone()]),
                _ => panic!("read past the page that has a catalogue"),
            })
        })
        .unwrap();
        assert_eq!(found.as_deref(), Some("v0.2.0"));
        assert_eq!(asked, [1, 2]);
        // An empty page is the end of the list.
        assert_eq!(
            newest_across_pages(|n| Ok(if n == 1 {
                json!([plain("a")])
            } else {
                json!([])
            }))
            .unwrap(),
            None
        );
        // Endless pages without a catalogue are an error, not "no release".
        assert!(newest_across_pages(|_| Ok(json!([plain("a")]))).is_err());
        assert!(newest_across_pages(|_| Err("network".into())).is_err());
    }

    #[test]
    fn repository_names_are_validated_before_any_process_starts() {
        for bad in [
            "--version",
            "--version/x",
            "-x/y",
            "a/b/c",
            "a",
            "/b",
            "a/",
            "a b/c",
            "a/b;rm",
        ] {
            assert!(valid_repository(bad).is_err(), "{bad}");
        }
        assert!(valid_repository("Among-Terminal/among-plugins").is_ok());
        // The fetching entry points reject the same names without a network call;
        // a missing `gh` would otherwise show up as a different error.
        let error = fetch_previous("-x/y").unwrap_err().to_string();
        assert_eq!(error, "repository must look like owner/name");
        assert!(previous_catalogue("-x/y", Path::new("/nonexistent/out")).is_err());
    }
}
