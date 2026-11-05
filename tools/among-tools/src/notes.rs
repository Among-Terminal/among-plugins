//! Renders the description of a plugin snapshot release from the same
//! inventory, catalogue and binding the release workflow publishes. The text
//! is for people; the machine binding the workflow later checks stays in the
//! last lines, inside an HTML comment, so GitHub does not show it.
//!
//! Every package of the inventory must be linked exactly once, and every
//! package the catalogue names for this release must be in the inventory, so
//! the description cannot point at a file the release lacks or leave one out.

use crate::author::{self, Binding, Inventory, BINDING_PREFIX};
use crate::{archive, strict, Result};
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::Path};

const ARCHITECTURES: [&str; 2] = ["amd64", "arm64"];
const SUMS: &str = "SHA256SUMS";
const CATALOGUE: &str = "among-plugins.json";
const OFFICIAL_REPOSITORY: &str = "Among-Terminal/among-plugins";

/// Text that ends up in a Markdown table cell or an inline code span must not
/// be able to break out of it, so only the characters of identifiers, versions
/// and feature names are accepted.
fn plain<'a>(what: &str, text: &'a str) -> Result<&'a str> {
    if text.is_empty()
        || !text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
    {
        return Err(format!("{what} {text:?} cannot be shown in the description").into());
    }
    Ok(text)
}

fn row_links(
    site: &str,
    tag: &str,
    version: &Value,
    inventory: &Inventory,
    linked: &mut BTreeSet<String>,
) -> Result<Vec<String>> {
    // A plugin carried over from an earlier release has its packages there.
    let (holder, carried) = match version.get("release") {
        Some(reference) => (
            plain(
                "carried release tag",
                reference["tag"]
                    .as_str()
                    .ok_or("carried plugin has no release tag")?,
            )?,
            true,
        ),
        None => (tag, false),
    };
    let artifacts = version["artifacts"]
        .as_array()
        .ok_or("catalogue version lists no packages")?;
    let mut cells = Vec::new();
    for architecture in ARCHITECTURES {
        let artifact = artifacts
            .iter()
            .find(|artifact| artifact["architecture"] == architecture)
            .ok_or_else(|| format!("catalogue lacks the {architecture} package"))?;
        let name = plain(
            "package name",
            artifact["asset"]
                .as_str()
                .ok_or("package has no asset name")?,
        )?;
        if !carried {
            if !inventory.assets.iter().any(|asset| asset.name == name) {
                return Err(format!("{name} is in the catalogue but not in the inventory").into());
            }
            if !linked.insert(name.to_owned()) {
                return Err(format!("{name} would be linked twice").into());
            }
        }
        cells.push(format!(
            "[{name}]({site}/releases/download/{holder}/{name}) ({} KiB)",
            artifact["size"]
                .as_u64()
                .ok_or("package has no size")?
                .div_ceil(1024)
        ));
    }
    Ok(cells)
}

/// The description, with the binding comment at its end.
pub fn render(
    repository: &str,
    inventory: &Inventory,
    catalogue: &Value,
    binding_line: &str,
    prerelease: bool,
    prepared_run: Option<u64>,
) -> Result<String> {
    let binding: Binding = serde_json::from_value(strict::parse(
        binding_line
            .strip_prefix(BINDING_PREFIX)
            .ok_or("binding lacks its prefix")?
            .trim_end()
            .as_bytes(),
    )?)?;
    if binding.tag != inventory.tag
        || binding.commit != inventory.revision
        || binding.inventory_sha256 != inventory.inventory_sha256
        || binding.repository_id != inventory.repository_id
    {
        return Err("binding and inventory describe different releases".into());
    }
    let tag = plain("tag", &inventory.tag)?;
    let commit = plain("commit", &inventory.revision)?;
    let site = format!("https://github.com/{repository}");
    let candidate = tag.contains("-rc.");
    if candidate && !prerelease {
        return Err("a release candidate is published as a prerelease".into());
    }

    // Only this repository's own releases may call themselves official; the
    // reusable workflow also publishes the releases of other plugin repositories.
    let official = repository == OFFICIAL_REPOSITORY;
    let what = if official {
        "the official Among Terminal plugins".to_owned()
    } else {
        format!("the plugins of `{repository}`")
    };

    let mut body = String::new();
    if candidate {
        body.push_str(&format!(
            "`{tag}` is a **release candidate** of {what}. \
It is published for testing and is not a stable release.\n\n"
        ));
    } else if prerelease {
        body.push_str(&format!("`{tag}` is a **prerelease** of {what}.\n\n"));
    } else {
        body.push_str(&format!("`{tag}` is a release of {what}.\n\n"));
    }
    body.push_str(
        "A release is a snapshot of the whole repository: one tag and one catalogue \
(`among-plugins.json`) that lists exactly one version of every plugin. Each plugin has its own version.\n\n",
    );
    if prerelease {
        body.push_str(
            "A prerelease is never the repository's latest release. Among installs it only in its plugin \
development mode.\n\n",
        );
    }

    body.push_str(
        "## Plugins\n\n| Plugin | Version | Among plugin API | Requires | amd64 package | arm64 package |\n|---|---|---|---|---|---|\n",
    );
    let mut linked = BTreeSet::new();
    let mut carried_over = false;
    let plugins = catalogue["plugins"]
        .as_array()
        .ok_or("catalogue lists no plugins")?;
    for plugin in plugins {
        let id = plain(
            "plugin id",
            plugin["id"].as_str().ok_or("plugin has no id")?,
        )?;
        let versions = plugin["versions"]
            .as_array()
            .filter(|versions| versions.len() == 1)
            .ok_or("a snapshot lists exactly one version of each plugin")?;
        let version = &versions[0];
        let manifest = &version["manifest"];
        let number = plain(
            "plugin version",
            manifest["version"]
                .as_str()
                .ok_or("plugin has no version")?,
        )?;
        let api = format!(
            "{}.{}",
            manifest["api"]["major"]
                .as_u64()
                .ok_or("plugin has no API major")?,
            manifest["api"]["minor"]
                .as_u64()
                .ok_or("plugin has no API minor")?
        );
        let mut requires = Vec::new();
        for feature in manifest["required_features"]
            .as_array()
            .into_iter()
            .flatten()
        {
            requires.push(format!(
                "`{}`",
                plain("required feature", feature.as_str().unwrap_or_default())?
            ));
        }
        let requires = if requires.is_empty() {
            "none".to_owned()
        } else {
            requires.join(", ")
        };
        let cells = row_links(&site, tag, version, inventory, &mut linked)?;
        let note = if version.get("release").is_some() {
            carried_over = true;
            " (carried over)"
        } else {
            ""
        };
        body.push_str(&format!(
            "| `{id}` | {number}{note} | {api} | {requires} | {} | {} |\n",
            cells[0], cells[1]
        ));
    }
    body.push('\n');
    if carried_over {
        body.push_str(
            "Plugins marked as carried over did not change since an earlier release; their packages stay \
attached to that release, which the links point to.\n\n",
        );
    }

    body.push_str("## Install\n\n");
    if official {
        body.push_str(
            "In Among open **Preferences -> Plugins** and add the source **Official plugins for Among Terminal**, \
then choose a plugin and review its access request before you enable it.\n\n",
        );
    } else {
        body.push_str(&format!(
            "In Among open **Preferences -> Plugins**, use **Inspect repository** under Plugin sources with \
`{repository}` and add it as a source, then choose a plugin and review its access request before you enable it.\n\n"
        ));
    }
    if prerelease {
        body.push_str(
            "To try this prerelease, start Among with `among --plugin-development` and use **Use latest \
prerelease** on the source row.\n\n",
        );
    }

    body.push_str("## Verify\n\nDownload the packages and `SHA256SUMS` into one directory and run:\n\n```sh\nsha256sum --check --ignore-missing SHA256SUMS\n```\n\n");
    for name in [SUMS, CATALOGUE] {
        if !inventory.assets.iter().any(|asset| asset.name == name) {
            return Err(format!("the inventory lacks {name}").into());
        }
    }

    let source = inventory
        .assets
        .iter()
        .find(|asset| asset.name.starts_with("source-"))
        .ok_or("the inventory lacks the source archive")?;
    linked.insert(source.name.clone());
    body.push_str(&format!(
        "## Source\n\n- Commit: [`{commit}`]({site}/commit/{commit})\n- Source archive: [{name}]({site}/releases/download/{tag}/{name})\n- Publication run: [{run}]({site}/actions/runs/{run})\n",
        name = plain("source archive name", &source.name)?,
        run = binding.run_id,
    ));
    if let Some(run) = prepared_run {
        body.push_str(&format!(
            "- Preparation run of these bytes: [{run}]({site}/actions/runs/{run})\n"
        ));
    }

    let unlinked: Vec<&str> = inventory
        .assets
        .iter()
        .map(|asset| asset.name.as_str())
        .filter(|name| ![SUMS, CATALOGUE].contains(name) && !linked.contains(*name))
        .collect();
    if !unlinked.is_empty() {
        return Err(format!("assets without a link in the description: {unlinked:?}").into());
    }

    // GitHub does not render an HTML comment. The line stays byte for byte the
    // one the release check compares with, on a line of its own.
    body.push_str("\n<!--\n");
    body.push_str(binding_line.trim_end());
    body.push_str("\n-->\n");

    if author::body_binding(&json!({ "body": body }))? != binding {
        return Err("the description does not carry exactly the expected binding".into());
    }
    Ok(body)
}

/// `release-notes`: writes the description of the release for `inventory`.
pub fn write(
    repository: &str,
    inventory: &Path,
    catalogue: &Path,
    binding: &Path,
    channel: &str,
    prepared_run: Option<u64>,
    output: &Path,
) -> Result<()> {
    if repository.split('/').count() != 2
        || !repository
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
    {
        return Err("repository must be owner/name".into());
    }
    let prerelease = match channel {
        "stable" => false,
        "prerelease" => true,
        _ => return Err("channel must be stable or prerelease".into()),
    };
    if prepared_run == Some(0) {
        return Err("preparation run ID is malformed".into());
    }
    let inventory = author::read_inventory(inventory)?;
    let catalogue = strict::parse(&archive::read(catalogue, 1024 * 1024)?)?;
    let binding = String::from_utf8(archive::read(binding, 64 * 1024)?)?;
    let text = render(
        repository,
        &inventory,
        &catalogue,
        &binding,
        prerelease,
        prepared_run,
    )?;
    archive::write_new(output, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::author::Asset;

    const COMMIT: &str = "a7aefca1a36125986c8ecc662400d4293c39b3cd";

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.into(),
            size: 10,
            sha256: "0".repeat(64),
        }
    }

    fn inventory(tag: &str) -> Inventory {
        let mut assets = vec![
            asset(SUMS),
            asset(CATALOGUE),
            asset(&format!("source-{COMMIT}.tar.gz")),
        ];
        for name in ["clock", "system-status"] {
            for architecture in ARCHITECTURES {
                assets.push(asset(&format!("{name}-0.2.0-linux-{architecture}.tar.gz")));
            }
        }
        Inventory {
            repository_id: 7,
            tag: tag.into(),
            revision: COMMIT.into(),
            inventory_sha256: "f".repeat(64),
            assets,
        }
    }

    fn binding_line(tag: &str) -> String {
        format!(
            "{BINDING_PREFIX}{}\n",
            serde_json::to_string(&Binding {
                repository_id: 7,
                workflow_ref: "o/r/.github/workflows/w.yml@refs/tags/v".into(),
                run_id: 41,
                tag: tag.into(),
                commit: COMMIT.into(),
                inventory_sha256: "f".repeat(64),
            })
            .unwrap()
        )
    }

    fn version(id: &str, features: &[&str], release: Option<(&str, u64)>) -> Value {
        let mut version = json!({
            "manifest": {"version": "0.2.0", "api": {"major": 1, "minor": 2}, "required_features": features},
            "artifacts": ARCHITECTURES.map(|architecture| json!({
                "architecture": architecture,
                "asset": format!("{id}-0.2.0-linux-{architecture}.tar.gz"),
                "size": 2049,
            })),
        });
        if let Some((tag, number)) = release {
            version["release"] = json!({"id": number, "tag": tag});
        }
        version
    }

    fn catalogue(second_release: Option<(&str, u64)>) -> Value {
        json!({"plugins": [
            {"id": "clock", "versions": [version("clock", &["status"], None)]},
            {"id": "system-status", "versions": [version("system-status", &["status", "system.metrics"], second_release)]},
        ]})
    }

    fn describe(
        inventory: &Inventory,
        catalogue: &Value,
        line: &str,
        prerelease: bool,
    ) -> Result<String> {
        render("o/r", inventory, catalogue, line, prerelease, Some(99))
    }

    fn refusal(result: Result<String>) -> String {
        result.unwrap_err().to_string()
    }

    #[test]
    fn a_candidate_is_described_for_people_and_keeps_its_binding_last() {
        let tag = "v0.2.0-rc.1";
        let line = binding_line(tag);
        let text = describe(&inventory(tag), &catalogue(None), &line, true).unwrap();
        assert!(text.contains("**release candidate**"), "{text}");
        assert!(text.contains("published for testing"));
        assert!(text.contains("| `system-status` | 0.2.0 | 1.2 | `status`, `system.metrics` |"));
        assert!(text.contains("[clock-0.2.0-linux-arm64.tar.gz](https://github.com/o/r/releases/download/v0.2.0-rc.1/clock-0.2.0-linux-arm64.tar.gz) (3 KiB)"));
        assert!(text.contains("sha256sum --check --ignore-missing SHA256SUMS"));
        assert!(text.contains("of the plugins of `o/r`."), "{text}");
        assert!(text.contains("**Inspect repository**"));
        assert!(
            !text.contains("fficial"),
            "another repository is not official: {text}"
        );
        assert!(text.contains("among --plugin-development"));
        assert!(text.contains("[41](https://github.com/o/r/actions/runs/41)"));
        assert!(text.contains(
            "Preparation run of these bytes: [99](https://github.com/o/r/actions/runs/99)"
        ));
        assert!(text.ends_with(&format!("\n<!--\n{line}-->\n")), "{text}");
        assert_eq!(text.matches(BINDING_PREFIX).count(), 1);
    }

    #[test]
    fn only_this_repository_calls_its_plugins_official() {
        let tag = "v0.2.0-rc.1";
        let text = render(
            OFFICIAL_REPOSITORY,
            &inventory(tag),
            &catalogue(None),
            &binding_line(tag),
            true,
            None,
        )
        .unwrap();
        assert!(
            text.contains("of the official Among Terminal plugins."),
            "{text}"
        );
        assert!(text.contains("**Official plugins for Among Terminal**"));
        assert!(!text.contains("Preparation run"));
    }

    #[test]
    fn a_stable_release_does_not_ask_for_development_mode() {
        let tag = "v0.2.0";
        let text = describe(&inventory(tag), &catalogue(None), &binding_line(tag), false).unwrap();
        assert!(!text.contains("--plugin-development"));
        assert!(!text.contains("candidate"));
    }

    #[test]
    fn a_carried_plugin_links_to_the_release_that_holds_its_files() {
        let tag = "v0.2.1";
        let mut inventory = inventory(tag);
        inventory
            .assets
            .retain(|asset| !asset.name.starts_with("system-status"));
        let text = describe(
            &inventory,
            &catalogue(Some(("v0.2.0", 5))),
            &binding_line(tag),
            false,
        )
        .unwrap();
        assert!(text.contains("0.2.0 (carried over)"));
        assert!(text.contains("releases/download/v0.2.0/system-status-0.2.0-linux-amd64.tar.gz"));
    }

    #[test]
    fn a_package_missing_from_the_inventory_or_left_unlinked_is_refused() {
        let tag = "v0.2.0";
        let mut missing = inventory(tag);
        missing
            .assets
            .retain(|asset| asset.name != "clock-0.2.0-linux-amd64.tar.gz");
        assert!(refusal(describe(
            &missing,
            &catalogue(None),
            &binding_line(tag),
            false
        ))
        .contains("not in the inventory"));
        let mut extra = inventory(tag);
        extra.assets.push(asset("stray-1.0.0-linux-amd64.tar.gz"));
        assert!(refusal(describe(
            &extra,
            &catalogue(None),
            &binding_line(tag),
            false
        ))
        .contains("without a link"));
    }

    #[test]
    fn a_binding_of_another_release_or_hostile_text_is_refused() {
        let tag = "v0.2.0";
        assert!(refusal(describe(
            &inventory(tag),
            &catalogue(None),
            &binding_line("v0.3.0"),
            false
        ))
        .contains("different releases"));
        assert!(refusal(describe(
            &inventory("v0.2.0-rc.1"),
            &catalogue(None),
            &binding_line("v0.2.0-rc.1"),
            false
        ))
        .contains("published as a prerelease"));
        let mut hostile = catalogue(None);
        hostile["plugins"][0]["id"] = json!("a|b");
        assert!(refusal(describe(
            &inventory(tag),
            &hostile,
            &binding_line(tag),
            false
        ))
        .contains("cannot be shown"));
    }
}
