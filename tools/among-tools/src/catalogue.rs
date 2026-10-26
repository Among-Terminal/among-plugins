use crate::{
    archive,
    package::{self, Build},
    process, Result,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const PUBLISHER: u64 = 339567100;
const MAX_CATALOGUE_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Deserialize)]
struct ReleaseState {
    id: u64,
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

fn confirmed_state(first: &Value, second: &Value) -> Result<ReleaseState> {
    let first: ReleaseState = serde_json::from_value(first.clone())?;
    let second: ReleaseState = serde_json::from_value(second.clone())?;
    if first != second {
        return Err("release identity/tag/publication state changed during inspection".into());
    }
    Ok(second)
}

fn encode_catalogue(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_CATALOGUE_BYTES {
        return Err("catalogue exceeds the host 1-MiB byte limit".into());
    }
    Ok(bytes)
}

fn api(path: &str, maximum: usize, deadline: Instant) -> Result<Value> {
    if Instant::now() >= deadline {
        return Err("release inspection deadline".into());
    }
    let bytes = process::capture(
        Command::new("gh").args([
            "api",
            "--hostname",
            "github.com",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "Cache-Control: no-cache",
            path,
        ]),
        &[],
        maximum,
    )?;
    if Instant::now() >= deadline {
        return Err("release inspection deadline".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn tag_name(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 256
        && tag.split('/').count() <= 16
        && tag.split('/').all(|component| {
            !component.is_empty()
                && !component.starts_with('.')
                && !component.ends_with('.')
                && !component.ends_with(".lock")
                && !component.contains("..")
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
}

fn binding(tag: &str, deadline: Instant) -> Result<Value> {
    if !tag_name(tag) {
        return Err("unsupported exact release tag".into());
    }
    let reference = format!("refs/tags/{tag}");
    let value = api(
        &format!("repositories/{}/git/ref/tags/{tag}", package::REPOSITORY),
        64 * 1024,
        deadline,
    )?;
    if value["ref"] != reference {
        return Err("missing or ambiguous exact tag reference".into());
    }
    let mut kind = value["object"]["type"]
        .as_str()
        .ok_or("tag object lacks type")?
        .to_owned();
    let mut sha = value["object"]["sha"]
        .as_str()
        .ok_or("tag object lacks SHA")?
        .to_owned();
    let mut chain = Vec::new();
    let mut seen = BTreeSet::new();
    for _ in 0..=8 {
        if !package::hex(&sha, 40) || !seen.insert(sha.clone()) {
            return Err("invalid or cyclic tag object".into());
        }
        chain.push(json!({"type":kind,"sha":sha}));
        match kind.as_str() {
            "commit" => {
                let commit = api(
                    &format!("repositories/{}/git/commits/{sha}", package::REPOSITORY),
                    64 * 1024,
                    deadline,
                )?;
                if commit["sha"] != sha {
                    return Err("resolved commit mismatch".into());
                }
                return Ok(json!({"tag":tag,"commit":sha,"chain":chain}));
            }
            "tag" if chain.len() <= 8 => {
                let object = api(
                    &format!("repositories/{}/git/tags/{sha}", package::REPOSITORY),
                    64 * 1024,
                    deadline,
                )?;
                if object["sha"] != sha {
                    return Err("annotated tag SHA mismatch".into());
                }
                kind = object["object"]["type"]
                    .as_str()
                    .ok_or("annotated tag lacks target type")?
                    .to_owned();
                sha = object["object"]["sha"]
                    .as_str()
                    .ok_or("annotated tag lacks target SHA")?
                    .to_owned();
            }
            _ => return Err("unsupported or excessive tag chain".into()),
        }
    }
    Err("tag chain exhausted".into())
}

fn asset(release: &Value, name: &str, size: u64, digest: &str) -> Result<u64> {
    let assets = release["assets"]
        .as_array()
        .ok_or("missing release assets")?;
    if assets.len() > 128 {
        return Err("release asset count exceeded".into());
    }
    let mut matching = assets.iter().filter(|asset| asset["name"] == name);
    let selected = matching.next().ok_or("required release asset missing")?;
    if matching.next().is_some()
        || selected["size"].as_u64() != Some(size)
        || selected["digest"] != format!("sha256:{digest}")
        || selected["state"] != "uploaded"
    {
        return Err("ambiguous, corrupt or unfinished release asset".into());
    }
    selected["id"]
        .as_u64()
        .filter(|id| *id != 0)
        .ok_or_else(|| "invalid numeric release asset identity".into())
}

fn assemble(
    release: &Value,
    binding: &Value,
    builds: [&Build; 2],
    source_size: u64,
) -> Result<Value> {
    let revision = &builds[0].revision;
    let release_id = release["id"]
        .as_u64()
        .filter(|id| *id != 0)
        .ok_or("missing release identity")?;
    if builds[0].architecture != "amd64"
        || builds[1].architecture != "arm64"
        || builds[1].revision != *revision
        || binding["commit"] != *revision
        || release["tag_name"] != binding["tag"]
        || builds[0].source_sha256 != builds[1].source_sha256
        || builds[0].source_archive != builds[1].source_archive
    {
        return Err("architecture, source or resolved Release-tag mismatch".into());
    }
    let mut ids = BTreeSet::new();
    let source_id = asset(
        release,
        &builds[0].source_archive,
        source_size,
        &builds[0].source_sha256,
    )?;
    ids.insert(source_id);
    let mut plugins = Vec::new();
    for id in package::IDS {
        let mut manifest = None;
        let mut manifest_sha256 = None;
        let mut artifacts = Vec::new();
        for build in builds {
            let package = build
                .packages
                .iter()
                .find(|package| package.manifest.id == id)
                .ok_or("missing independent plugin")?;
            if manifest
                .as_ref()
                .is_some_and(|value| value != &package.manifest)
                || manifest_sha256
                    .as_ref()
                    .is_some_and(|value| value != &package.manifest_sha256)
            {
                return Err("cross-architecture shared manifest mismatch".into());
            }
            manifest = Some(package.manifest.clone());
            manifest_sha256 = Some(package.manifest_sha256.clone());
            let id = asset(
                release,
                &package.name,
                package.artifact.size,
                &package.artifact.sha256,
            )?;
            if !ids.insert(id) {
                return Err("duplicate numeric asset identity".into());
            }
            let mut artifact = serde_json::to_value(&package.artifact)?;
            artifact
                .as_object_mut()
                .ok_or("invalid artifact object")?
                .insert("asset_id".into(), id.into());
            artifacts.push(artifact);
        }
        plugins.push(json!({"id":id,"versions":[{"revision":revision,"manifest":manifest,"manifest_sha256":manifest_sha256,"release_id":release_id,"artifacts":artifacts}]}));
    }
    Ok(json!({"schema_version":1,"plugins":plugins}))
}

pub fn generate(
    root: &Path,
    amd64: &Path,
    arm64: &Path,
    release_id: u64,
    output: &Path,
) -> Result<()> {
    if release_id == 0 {
        return Err("release ID must be nonzero".into());
    }
    let first = package::load(root, amd64)?;
    let second = package::load(root, arm64)?;
    let deadline = Instant::now() + Duration::from_secs(180);
    let repository = api(
        &format!("repositories/{}", package::REPOSITORY),
        64 * 1024,
        deadline,
    )?;
    if repository["id"] != package::REPOSITORY
        || repository["owner"]["id"] != PUBLISHER
        || repository["full_name"] != "Among-Terminal/among-plugins"
        || repository["private"] != false
    {
        return Err("official numeric repository/publisher mismatch".into());
    }
    let path = format!("repositories/{}/releases/{release_id}", package::REPOSITORY);
    let release = api(&path, 256 * 1024, deadline)?;
    if release["id"] != release_id {
        return Err("release identity mismatch".into());
    }
    let tag = release["tag_name"]
        .as_str()
        .ok_or("release lacks actual tag_name")?;
    let observed = binding(tag, deadline)?;
    let source_size =
        archive::read(&amd64.join(&first.source_archive), archive::MAX_FILE)?.len() as u64;
    let catalogue = assemble(&release, &observed, [&first, &second], source_size)?;
    let confirmed_release = api(&path, 256 * 1024, deadline)?;
    let confirmed_binding = binding(tag, deadline)?;
    let confirmed_catalogue = assemble(
        &confirmed_release,
        &confirmed_binding,
        [&first, &second],
        source_size,
    )?;
    if catalogue != confirmed_catalogue || observed != confirmed_binding {
        return Err("release asset or complete tag chain changed during inspection".into());
    }
    let state = confirmed_state(&release, &confirmed_release)?;
    let bytes = encode_catalogue(&confirmed_catalogue)?;
    fs::create_dir(output)?;
    archive::write_new(&output.join("among-plugins.json"), &bytes)?;
    archive::json(
        &output.join("release-binding.json"),
        &json!({"repository_id":package::REPOSITORY,"publisher_id":PUBLISHER,"release_id":state.id,"draft":state.draft,"prerelease":state.prerelease,"binding":confirmed_binding,"source_asset_id":asset(&confirmed_release,&first.source_archive,source_size,&first.source_sha256)?}),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_state_changes_and_oversized_catalogues_refuse_before_output() {
        let original = json!({"id":123,"tag_name":"test-tag","draft":true,"prerelease":true});
        assert!(confirmed_state(&original, &original).unwrap().draft);
        let mut changed = original.clone();
        changed["draft"] = false.into();
        assert!(confirmed_state(&original, &changed).is_err());
        changed = original.clone();
        changed["prerelease"] = false.into();
        assert!(confirmed_state(&original, &changed).is_err());
        assert!(confirmed_state(&original, &json!({"id":123,"tag_name":"test-tag"})).is_err());
        assert!(encode_catalogue(&json!({"schema_version":1,"plugins":[]})).is_ok());
        assert!(encode_catalogue(&json!({"members":"x".repeat(MAX_CATALOGUE_BYTES)})).is_err());
    }
    #[test]
    fn exact_assets_reject_missing_corrupt_and_ambiguous_membership() {
        let digest = "a".repeat(64);
        let good = json!({"name":"clock.tar.gz","size":42,"digest":format!("sha256:{digest}"),"id":12,"state":"uploaded"});
        assert_eq!(
            asset(
                &json!({"assets":[good.clone()]}),
                "clock.tar.gz",
                42,
                &digest
            )
            .unwrap(),
            12
        );
        assert!(asset(&json!({"assets":[]}), "clock.tar.gz", 42, &digest).is_err());
        assert!(asset(
            &json!({"assets":[good.clone(),good.clone()]}),
            "clock.tar.gz",
            42,
            &digest
        )
        .is_err());
        assert!(asset(
            &json!({"assets":[good.clone()]}),
            "clock.tar.gz",
            43,
            &digest
        )
        .is_err());
        assert!(asset(
            &json!({"assets":[good]}),
            "clock.tar.gz",
            42,
            &"b".repeat(64)
        )
        .is_err());
    }
    #[test]
    fn exact_tag_names_do_not_fall_back_to_branches_or_prefixes() {
        assert!(tag_name("plugins/v0.1.0-rc.1"));
        for name in [
            "",
            ".hidden",
            "../main",
            "main..tag",
            "a//b",
            "a.lock",
            "a?ref=main",
            "refs/tags/x#y",
        ] {
            assert!(!tag_name(name));
        }
    }

    #[test]
    fn catalogue_requires_both_platforms_one_source_and_distinct_real_asset_bindings() {
        // Deliberately synthetic metadata: these are parser controls, not a
        // receipt for a release or evidence of GitHub tag resolution.
        let digest = "a".repeat(64);
        let revision = "b".repeat(40);
        let mut assets = vec![
            json!({"name":"source.tar.gz","size":9,"digest":format!("sha256:{digest}"),"id":1,"state":"uploaded"}),
        ];
        let mut builds = Vec::new();
        for (platform, architecture) in ["amd64", "arm64"].iter().enumerate() {
            let mut packages = Vec::new();
            for (index, id) in package::IDS.iter().enumerate() {
                let name = format!("{id}-{architecture}.tar.gz");
                assets.push(json!({"name":name,"size":42,"digest":format!("sha256:{digest}"),"id":(2+platform*3+index) as u64,"state":"uploaded"}));
                packages.push(json!({"name":name,"manifest":{"schema_version":1,"id":id,"version":"test","name":id,"description":"fixture","entrypoint":format!("bin/{id}-managed"),"api":{"major":1,"minor":2},"required_features":["status"],"optional_features":[]},"manifest_sha256":digest,"artifact":{"os":"linux","architecture":architecture,"runtime":{"kind":"native","libc":"glibc","minimum":"2.34"},"dependencies":[],"format":"tar.gz","size":42,"sha256":digest,"members":[]},"needed_libraries":[]}));
            }
            builds.push(serde_json::from_value::<Build>(json!({"schema_version":1,"repository_id":package::REPOSITORY,"revision":revision,"architecture":architecture,"source_archive":"source.tar.gz","source_sha256":digest,"packages":packages})).unwrap());
        }
        let mut release = json!({"id":123,"tag_name":"test-tag","assets":assets});
        let observed = json!({"tag":"test-tag","commit":revision});
        let catalogue = assemble(&release, &observed, [&builds[0], &builds[1]], 9).unwrap();
        assert_eq!(catalogue["plugins"].as_array().unwrap().len(), 3);
        assert_eq!(
            catalogue["plugins"][0]["versions"][0]["artifacts"][1]["architecture"],
            "arm64"
        );
        assert!(assemble(
            &release,
            &json!({"tag":"test-tag","commit":"c".repeat(40)}),
            [&builds[0], &builds[1]],
            9
        )
        .is_err());
        assert!(assemble(&release, &observed, [&builds[0], &builds[0]], 9).is_err());
        let mut changed = builds[1].clone();
        changed.source_sha256 = "c".repeat(64);
        assert!(assemble(&release, &observed, [&builds[0], &changed], 9).is_err());
        release["assets"][2]["id"] = release["assets"][1]["id"].clone();
        assert!(assemble(&release, &observed, [&builds[0], &builds[1]], 9).is_err());
    }
}
