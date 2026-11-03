// Release planning for the plugin repository. Plugins version independently and
// a release is one snapshot whose catalogue lists one version per plugin, so a
// release has to decide three things: which plugins changed, by how much their
// versions rise, and what the snapshot is called. Release-please attributes
// commits to a component by path, and here the code of every plugin lives in
// shared modules, so the decision is made from the packaged content and from the
// Conventional Commit titles since the previous release instead.
//
// Plugins that did not change are never rebuilt or shipped again: the new
// catalogue carries their previous entry over verbatim and points at the
// immutable release that holds their files.

use crate::changed::{
    self, differences, fetch_previous, from_builds, from_catalogue, read_json, valid_repository,
    Plugin, Version,
};
use crate::{archive, catalogue, process, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

// The label of the first snapshot. Later labels continue from the previous tag.
const FIRST_LABEL: &str = "v0.2.0";

const TYPES: [&str; 9] = [
    "feat", "fix", "docs", "test", "ci", "build", "perf", "refactor", "chore",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Patch,
    Minor,
    Major,
}

impl Level {
    fn name(self) -> &'static str {
        match self {
            Level::Patch => "patch",
            Level::Minor => "minor",
            Level::Major => "major",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Commit {
    kind: String,
    scope: Option<String>,
    breaking: bool,
}

// `type(scope)!: summary` plus an optional `BREAKING CHANGE:` footer. Anything
// else is not a Conventional Commit and takes no part in the decision.
pub fn parse_commit(subject: &str, body: &str) -> Option<Commit> {
    let (head, summary) = subject.split_once(": ")?;
    if summary.trim().is_empty() {
        return None;
    }
    let (head, bang) = match head.strip_suffix('!') {
        Some(head) => (head, true),
        None => (head, false),
    };
    let (kind, scope) = match head.split_once('(') {
        Some((kind, rest)) => {
            let scope = rest.strip_suffix(')')?;
            if scope.is_empty() {
                return None;
            }
            (kind, Some(scope.to_owned()))
        }
        None => (head, None),
    };
    if !TYPES.contains(&kind) {
        return None;
    }
    let footer = body
        .lines()
        .any(|line| line.starts_with("BREAKING CHANGE:") || line.starts_with("BREAKING-CHANGE:"));
    Some(Commit {
        kind: kind.to_owned(),
        scope,
        breaking: bang || footer,
    })
}

fn level_of(commit: &Commit) -> Option<Level> {
    if commit.breaking {
        return Some(Level::Major);
    }
    match commit.kind.as_str() {
        "feat" => Some(Level::Minor),
        "fix" | "perf" => Some(Level::Patch),
        _ => None,
    }
}

// The commits that speak for one plugin: those scoped to it and those whose
// scope names no plugin (shared code, `ci`, no scope). A commit scoped to
// another plugin does not. A changed plugin without any such commit still gets
// a patch, because its packaged content did change.
fn plugin_level(id: &str, ids: &BTreeSet<String>, commits: &[Commit]) -> Level {
    commits
        .iter()
        .filter(|commit| match &commit.scope {
            Some(scope) => scope == id || !ids.contains(scope),
            None => true,
        })
        .filter_map(level_of)
        .max()
        .unwrap_or(Level::Patch)
}

fn bump(version: &str, level: Level) -> Result<String> {
    let parsed = Version::parse(version)?;
    if !parsed.pre.is_empty() {
        return Err(format!("`{version}` is a prerelease; set the next version by hand").into());
    }
    let [major, minor, patch] = parsed.core;
    // While the major version is zero a breaking change raises the minor one.
    let level = if major == 0 && level == Level::Major {
        Level::Minor
    } else {
        level
    };
    Ok(match level {
        Level::Major => format!("{}.0.0", major + 1),
        Level::Minor => format!("{major}.{}.0", minor + 1),
        Level::Patch => format!("{major}.{minor}.{}", patch + 1),
    })
}

fn delta(from: &str, to: &str) -> Result<Level> {
    let (from, to) = (Version::parse(from)?, Version::parse(to)?);
    Ok(if from.core[0] != to.core[0] {
        Level::Major
    } else if from.core[1] != to.core[1] {
        Level::Minor
    } else {
        Level::Patch
    })
}

fn next_label(previous: Option<&str>, level: Level, candidate: Option<u32>) -> Result<String> {
    let base = match previous {
        None => FIRST_LABEL.to_owned(),
        Some(tag) => {
            let version = tag
                .strip_prefix('v')
                .ok_or_else(|| format!("previous tag `{tag}` does not start with v"))?;
            let parsed = Version::parse(version)?;
            if parsed.pre.is_empty() {
                format!("v{}", bump(version, level)?)
            } else {
                // The candidate's stable release is still to come: the label
                // stays and only the candidate counter moves.
                format!("v{}.{}.{}", parsed.core[0], parsed.core[1], parsed.core[2])
            }
        }
    };
    Ok(match candidate {
        Some(number) if number > 0 => format!("{base}-rc.{number}"),
        Some(_) => return Err("the candidate number starts at 1".into()),
        None => base,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub struct Bump {
    pub plugin: String,
    pub from: String,
    pub to: String,
    pub level: Level,
    // False when the manifest already carries the raised version.
    pub write: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub label: String,
    pub bumps: Vec<Bump>,
    pub unchanged: Vec<String>,
    pub new: Vec<String>,
}

impl Plan {
    pub fn to_json(&self) -> Value {
        json!({
            "label": self.label,
            "bumps": self.bumps.iter().map(|bump| json!({
                "plugin": bump.plugin, "from": bump.from, "to": bump.to,
                "level": bump.level.name(), "write": bump.write})).collect::<Vec<_>>(),
            "unchanged": self.unchanged,
            "new": self.new,
        })
    }
}

pub(crate) fn plan(
    previous: &BTreeMap<String, Plugin>,
    current: &BTreeMap<String, Plugin>,
    commits: &[Commit],
    previous_label: Option<&str>,
    candidate: Option<u32>,
) -> Result<Plan> {
    let ids: BTreeSet<String> = current.keys().cloned().collect();
    let (mut bumps, mut unchanged, mut new) = (Vec::new(), Vec::new(), Vec::new());
    for (id, now) in current {
        let Some(before) = previous.get(id) else {
            new.push(id.clone());
            continue;
        };
        let (old, cur) = (
            Version::parse(&before.version)?,
            Version::parse(&now.version)?,
        );
        let changed = !differences(before, now).is_empty();
        match (cur.cmp(&old), changed) {
            (std::cmp::Ordering::Less, _) => {
                return Err(format!(
                    "{id}: version fell from {} to {}",
                    before.version, now.version
                )
                .into())
            }
            (std::cmp::Ordering::Equal, false) => unchanged.push(id.clone()),
            (std::cmp::Ordering::Greater, false) => {
                return Err(format!(
                    "{id}: version rose from {} to {} but the packaged content is unchanged",
                    before.version, now.version
                )
                .into())
            }
            (std::cmp::Ordering::Equal, true) => {
                let level = plugin_level(id, &ids, commits);
                bumps.push(Bump {
                    plugin: id.clone(),
                    from: before.version.clone(),
                    to: bump(&before.version, level)?,
                    level,
                    write: true,
                });
            }
            (std::cmp::Ordering::Greater, true) => {
                // The manifest already carries a raised version, as on the
                // release commit. It must be the one the commits call for.
                let level = plugin_level(id, &ids, commits);
                let expected = bump(&before.version, level)?;
                if expected != now.version {
                    return Err(format!(
                        "{id}: version {} differs from the computed {expected} (from {})",
                        now.version, before.version
                    )
                    .into());
                }
                bumps.push(Bump {
                    plugin: id.clone(),
                    from: before.version.clone(),
                    to: now.version.clone(),
                    level: delta(&before.version, &now.version)?,
                    write: false,
                });
            }
        }
    }
    if bumps.is_empty() && new.is_empty() {
        return Err("nothing to release: no plugin changed since the previous release".into());
    }
    let largest = bumps
        .iter()
        .map(|bump| bump.level)
        .max()
        .unwrap_or(Level::Minor);
    Ok(Plan {
        label: next_label(previous_label, largest, candidate)?,
        bumps,
        unchanged,
        new,
    })
}

// Writes the raised versions into the manifests. Only the text of the `version`
// value changes, so the rest of each file keeps its layout.
pub fn apply(root: &Path, plan: &Plan) -> Result<()> {
    for bump in plan.bumps.iter().filter(|bump| bump.write) {
        if !bump
            .plugin
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(format!("`{}` is not a plugin id", bump.plugin).into());
        }
        let path = root
            .join("plugins")
            .join(&bump.plugin)
            .join("among-plugin.json");
        let text = std::fs::read_to_string(&path)?;
        let old = format!("\"version\": \"{}\"", bump.from);
        if text.matches(&old).count() != 1 {
            return Err(format!("{}: expected exactly one `{old}`", path.display()).into());
        }
        let updated = text.replacen(&old, &format!("\"version\": \"{}\"", bump.to), 1);
        let parsed: Value = serde_json::from_str(&updated)?;
        if parsed["version"] != bump.to {
            return Err(format!("{}: version was not updated", path.display()).into());
        }
        std::fs::write(&path, updated)?;
    }
    Ok(())
}

fn commits_since(root: &Path, tag: Option<&str>) -> Result<Vec<Commit>> {
    let Some(tag) = tag else {
        return Ok(Vec::new());
    };
    if !catalogue::tag_name(tag) {
        return Err(format!("`{tag}` is not a usable tag name").into());
    }
    let log = process::text(Command::new("git").arg("-C").arg(root).args([
        "log",
        "--format=%s%x1f%b%x1e",
        &format!("refs/tags/{tag}..HEAD"),
    ]))?;
    Ok(log
        .split('\u{1e}')
        .filter_map(|entry| {
            let (subject, body) = entry.trim_start_matches('\n').split_once('\u{1f}')?;
            parse_commit(subject, body)
        })
        .collect())
}

#[derive(Clone, Copy)]
pub struct PlanArguments<'a> {
    pub root: &'a Path,
    pub repository: &'a str,
    pub candidate: Option<u32>,
    pub builds: &'a [&'a Path],
}

// The build records must describe the commit that is checked out; plans made
// from the packages of another commit would bump the wrong content.
fn require_builds_at_head(root: &Path, builds: &[Value]) -> Result<()> {
    let head = git(root, &["rev-parse", "HEAD"])?;
    for build in builds {
        if build["revision"].as_str() != Some(head.trim()) {
            return Err(format!(
                "a build record is for {}, not for the checked out {}",
                build["revision"].as_str().unwrap_or("an unknown commit"),
                head.trim()
            )
            .into());
        }
    }
    Ok(())
}

// The previous release is fetched here, once, so that its tag and its
// catalogue cannot come from different releases.
fn plan_against(
    root: &Path,
    previous_tag: Option<&str>,
    previous_catalogue: &Value,
    builds: &[Value],
    candidate: Option<u32>,
) -> Result<Plan> {
    require_builds_at_head(root, builds)?;
    let commits = commits_since(root, previous_tag)?;
    plan(
        &from_catalogue(previous_catalogue)?,
        &from_builds(builds)?,
        &commits,
        previous_tag,
        candidate,
    )
}

fn read_builds(paths: &[&Path]) -> Result<Vec<Value>> {
    paths.iter().map(|path| read_json(path)).collect()
}

pub fn run_plan(arguments: &PlanArguments) -> Result<Plan> {
    let (tag, bytes) = fetch_previous(arguments.repository, None)?;
    plan_against(
        arguments.root,
        tag.as_deref(),
        &serde_json::from_slice(&bytes)?,
        &read_builds(arguments.builds)?,
        arguments.candidate,
    )
}

// Writes `catalogue.json` and, when there is a previous release, its record
// `release.json` into the new directory, so that a release can carry unchanged
// plugins over from it. `exclude` leaves one tag out.
pub fn write_previous_release(
    repository: &str,
    exclude: Option<&str>,
    directory: &Path,
) -> Result<()> {
    let (tag, bytes) = fetch_previous(repository, exclude)?;
    let record = tag
        .as_deref()
        .map(|tag| release_json(repository, &format!("releases/tags/{tag}")))
        .transpose()?;
    std::fs::create_dir(directory)?;
    archive::write_new(&directory.join("catalogue.json"), &bytes)?;
    match (tag, record) {
        (Some(tag), Some(record)) => {
            let mut text = serde_json::to_vec_pretty(&record)?;
            text.push(b'\n');
            archive::write_new(&directory.join("release.json"), &text)?;
            eprintln!("previous release: {tag}");
        }
        _ => eprintln!("no published release has a catalogue: every plugin counts as new"),
    }
    Ok(())
}

// The label a commit on the default branch asks to be tagged with: the subject
// of the squash commit of a release pull request, `chore(release): v<label>`
// with an optional ` (#number)`. `None` for any other commit. A release commit
// that touches anything but plugin manifests is refused, so a pull request
// title alone cannot make the workflow tag arbitrary content.
pub fn snapshot_label(subject: &str, files: &[&str]) -> Result<Option<String>> {
    let Some(rest) = subject.strip_prefix("chore(release): ") else {
        return Ok(None);
    };
    let label = match rest.split_once(" (#") {
        Some((label, number)) => {
            let digits = number
                .strip_suffix(')')
                .ok_or("malformed pull request number")?;
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("malformed pull request number".into());
            }
            label
        }
        None => rest,
    };
    let version = label
        .strip_prefix('v')
        .ok_or_else(|| format!("release label `{label}` does not start with v"))?;
    let (core, candidate) = match version.split_once("-rc.") {
        Some((core, number)) => (core, Some(number)),
        None => (version, None),
    };
    let parsed = Version::parse(core)?;
    if !parsed.pre.is_empty()
        || candidate.is_some_and(|number| {
            number == "0"
                || number.is_empty()
                || !number.bytes().all(|byte| byte.is_ascii_digit())
                || number.starts_with('0')
        })
    {
        return Err(format!("`{label}` is not a release label").into());
    }
    for file in files {
        let manifest = file
            .strip_prefix("plugins/")
            .and_then(|rest| rest.strip_suffix("/among-plugin.json"))
            .is_some_and(|id| {
                !id.is_empty()
                    && id.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            });
        if !manifest {
            return Err(format!("release commit `{label}` also changes {file}").into());
        }
    }
    if files.is_empty() {
        return Err(format!("release commit `{label}` changes no plugin manifest").into());
    }
    Ok(Some(label.to_owned()))
}

fn git(root: &Path, arguments: &[&str]) -> Result<String> {
    process::text(Command::new("git").arg("-C").arg(root).args(arguments))
}

fn description(plan: &Plan) -> String {
    let mut text = format!("Release `{}`.\n\n", plan.label);
    for bump in &plan.bumps {
        text.push_str(&format!(
            "- `{}`: {} -> {} ({})\n",
            bump.plugin,
            bump.from,
            bump.to,
            bump.level.name()
        ));
    }
    for id in &plan.new {
        text.push_str(&format!("- `{id}`: new plugin\n"));
    }
    if !plan.unchanged.is_empty() {
        text.push_str(&format!(
            "\nUnchanged and carried over from the previous release: {}.\n",
            plan.unchanged.join(", ")
        ));
    }
    text
}

// Opens the release pull request with the operator's own credentials, so the
// ordinary pull request checks run on it. It raises the manifests on a branch
// `release/<label>`; merging the pull request is the owner's step, and it
// creates no tag and no release.
pub fn run_release_pr(arguments: &PlanArguments, dry_run: bool) -> Result<()> {
    release_pr_with(arguments, dry_run, run_plan)
}

// The planner is a parameter so that the guards and the clean-up can be tested
// without GitHub.
fn release_pr_with(
    arguments: &PlanArguments,
    dry_run: bool,
    planner: impl Fn(&PlanArguments) -> Result<Plan>,
) -> Result<()> {
    let root = arguments.root;
    if !git(root, &["status", "--porcelain"])?.trim().is_empty() {
        return Err("the working tree is not clean".into());
    }
    if git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?.trim() != "main" {
        return Err("run the release pull request from main".into());
    }
    git(root, &["fetch", "-q", "origin", "main"])?;
    if git(root, &["rev-parse", "HEAD"])? != git(root, &["rev-parse", "origin/main"])? {
        return Err("main is not equal to origin/main".into());
    }
    let plan = planner(arguments)?;
    print!("{}", description(&plan));
    // A label that is already a tag, here or on the remote, is a release that
    // exists; a pull request for it would be a second one.
    if git(root, &["tag", "--list", &plan.label])?.trim() == plan.label
        || !git(
            root,
            &[
                "ls-remote",
                "--tags",
                "origin",
                &format!("refs/tags/{}", plan.label),
            ],
        )?
        .trim()
        .is_empty()
    {
        return Err(format!("the tag {} already exists", plan.label).into());
    }
    if dry_run {
        return Ok(());
    }
    let branch = format!("release/{}", plan.label);
    git(root, &["switch", "-c", &branch])?;
    // From here every failure puts the working tree back on main, so that a
    // retry starts from the state the operator began with.
    let outcome = publish_branch(root, &plan, &branch);
    if outcome.is_err() {
        let _ = git(root, &["checkout", "-q", "--", "."]);
        let _ = git(root, &["switch", "-q", "main"]);
        let _ = git(root, &["branch", "-q", "-D", &branch]);
    }
    outcome
}

fn publish_branch(root: &Path, plan: &Plan, branch: &str) -> Result<()> {
    apply(root, plan)?;
    let touched = git(root, &["diff", "--name-only"])?;
    if touched
        .lines()
        .any(|line| !(line.starts_with("plugins/") && line.ends_with("/among-plugin.json")))
    {
        return Err(format!("unexpected changes besides manifests:\n{touched}").into());
    }
    let title = format!("chore(release): {}", plan.label);
    git(
        root,
        &["commit", "-q", "-a", "-m", &title, "-m", &description(plan)],
    )?;
    git(root, &["push", "-q", "-u", "origin", branch])?;
    process::text(Command::new("gh").current_dir(root).args([
        "pr",
        "create",
        "--base",
        "main",
        "--head",
        branch,
        "--title",
        &title,
        "--body",
        &description(plan),
    ]))
    .map_err(|error| {
        format!("the branch {branch} was pushed but the pull request was not opened: {error}")
    })?;
    Ok(())
}

// The strict check of a release commit, run on the default branch before the
// snapshot tag exists. The working tree is that commit. It confirms that the
// commit is a release commit, that it changes nothing but the `version` line of
// plugin manifests, that the build records are those of this very commit, that
// every changed plugin carries a raised version, that the versions are those the
// commits since the previous release call for, and that the label in the title is
// the computed next label. It returns the label to tag.
pub fn snapshot_verify(root: &Path, repository: &str, builds: &[&Path]) -> Result<String> {
    let subject = git(root, &["show", "-s", "--format=%s", "HEAD"])?;
    let files = git(
        root,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "--no-renames",
            "HEAD",
        ],
    )?;
    let files: Vec<&str> = files.lines().collect();
    let label =
        snapshot_label(subject.trim_end(), &files)?.ok_or("the commit is not a release commit")?;
    let diff = git(root, &["diff", "-U0", "HEAD^", "HEAD", "--", "plugins"])?;
    only_version_lines(&diff)?;
    let (tag, bytes) = fetch_previous(repository, Some(&label))?;
    let catalogue: Value = serde_json::from_slice(&bytes)?;
    let records = read_builds(builds)?;
    require_builds_at_head(root, &records)?;
    let report = changed::check(&catalogue, &records, false)?;
    if report.failures > 0 {
        return Err(format!(
            "the strict version check fails:\n{}",
            report.lines.join("\n")
        )
        .into());
    }
    let candidate = label
        .split_once("-rc.")
        .map(|(_, number)| number.parse::<u32>())
        .transpose()?;
    let plan = plan_against(root, tag.as_deref(), &catalogue, &records, candidate)?;
    if plan.label != label {
        return Err(format!(
            "the release commit is titled {label} but the next label is {}",
            plan.label
        )
        .into());
    }
    Ok(label)
}

// A unified diff (zero context) of the plugin directory that touches nothing
// but `"version"` lines: every changed line is such a line, and removed and
// added lines pair up.
fn only_version_lines(diff: &str) -> Result<()> {
    let (mut removed, mut added) = (0, 0);
    for line in diff.lines() {
        let (sign, content) = match line.split_at_checked(1) {
            Some(("+", rest)) if !line.starts_with("+++") => (1, rest),
            Some(("-", rest)) if !line.starts_with("---") => (-1, rest),
            _ => continue,
        };
        if !content.trim_start().starts_with("\"version\":") {
            return Err(format!("the release commit changes more than versions: {line}").into());
        }
        if sign > 0 {
            added += 1;
        } else {
            removed += 1;
        }
    }
    if added == 0 || added != removed {
        return Err("the release commit must replace version lines one for one".into());
    }
    Ok(())
}

// The plugins whose entry in the new catalogue is the previous one, unchanged.
fn unchanged_ids(
    previous: &BTreeMap<String, Plugin>,
    current: &BTreeMap<String, Plugin>,
) -> Result<BTreeSet<String>> {
    let mut found = BTreeSet::new();
    for (id, now) in current {
        let Some(before) = previous.get(id) else {
            continue;
        };
        let same = differences(before, now).is_empty();
        let same_version = Version::parse(&before.version)? == Version::parse(&now.version)?;
        match (same, same_version) {
            (true, true) => {
                found.insert(id.clone());
            }
            (true, false) => {
                return Err(
                    format!("{id}: version changed but the packaged content did not").into(),
                )
            }
            (false, true) => {
                return Err(
                    format!("{id}: packaged content changed but the version did not").into(),
                )
            }
            (false, false) => {}
        }
    }
    Ok(found)
}

// A release that other releases may reference: published and immutable, so its
// tag and files cannot change under the entries that point at it.
fn check_release(release: &Value, id: u64, tag: &str) -> Result<()> {
    if release["id"].as_u64() != Some(id) {
        return Err(format!("release {tag} has another id than {id}").into());
    }
    if release["tag_name"].as_str() != Some(tag) {
        return Err(format!("release {id} is not tagged {tag}").into());
    }
    if release["draft"].as_bool() != Some(false) {
        return Err(format!("release {tag} is not published").into());
    }
    if release["immutable"].as_bool() != Some(true) {
        return Err(format!("release {tag} is not immutable").into());
    }
    Ok(())
}

pub(crate) fn reference(version: &Value) -> Result<Option<(u64, String)>> {
    match version.get("release") {
        None => Ok(None),
        Some(release) => Ok(Some((
            release["id"]
                .as_u64()
                .ok_or("release reference has no id")?,
            release["tag"]
                .as_str()
                .ok_or("release reference has no tag")?
                .to_owned(),
        ))),
    }
}

// Replaces the entry of every unchanged plugin in the freshly generated
// catalogue by the entry of the previous catalogue, verbatim. An entry that
// has no `release` reference lived in the previous release itself, so it gets
// a reference to it; an entry that already carries one keeps it. It is an error
// when an unchanged plugin has no entry in the previous catalogue.
// Refuses a previous release that others may not point at: a draft or a
// release that is not immutable. Returns its id and tag.
pub(crate) fn check_previous(previous: &Previous) -> Result<(u64, String)> {
    let id = previous.release["id"]
        .as_u64()
        .ok_or("previous release has no id")?;
    let tag = previous.release["tag_name"]
        .as_str()
        .ok_or("previous release has no tag")?;
    check_release(&previous.release, id, tag)?;
    Ok((id, tag.to_owned()))
}

pub(crate) fn carry_over(
    previous: &Previous,
    fresh: &Value,
    unchanged: &BTreeSet<String>,
) -> Result<Value> {
    let (id, tag) = check_previous(previous)?;
    let tag = tag.as_str();
    let previous = &previous.catalogue;
    let mut result = fresh.clone();
    let entries = result["plugins"]
        .as_array_mut()
        .ok_or("fresh catalogue has no plugin list")?;
    let mut carried = BTreeSet::new();
    for entry in entries.iter_mut() {
        let plugin = entry["id"].as_str().ok_or("plugin has no id")?.to_owned();
        if !unchanged.contains(&plugin) {
            continue;
        }
        let old = previous["plugins"]
            .as_array()
            .and_then(|all| all.iter().find(|candidate| candidate["id"] == plugin))
            .ok_or_else(|| {
                format!("{plugin}: not in the previous catalogue, nothing to carry over")
            })?;
        let mut old = old.clone();
        let versions = old["versions"]
            .as_array_mut()
            .filter(|versions| versions.len() == 1)
            .ok_or_else(|| format!("{plugin}: the previous entry must have one version"))?;
        if reference(&versions[0])?.is_none() {
            versions[0]["release"] = json!({"id": id, "tag": tag});
        }
        *entry = old;
        carried.insert(plugin);
    }
    if let Some(missing) = unchanged.difference(&carried).next() {
        return Err(format!("{missing}: unchanged but absent from the fresh catalogue").into());
    }
    Ok(result)
}

fn release_json(repository: &str, path: &str) -> Result<Value> {
    let bytes = process::capture(
        Command::new("gh").args([
            "api",
            "--hostname",
            "github.com",
            &format!("repos/{repository}/{path}"),
        ]),
        &[],
        4 * 1024 * 1024,
    )?;
    Ok(serde_json::from_slice(&bytes)?)
}

// The previous release as a release carries it: its catalogue and its record.
pub(crate) struct Previous {
    pub(crate) catalogue: Value,
    pub(crate) release: Value,
}

pub(crate) fn unchanged_in_builds(
    previous: &Previous,
    builds: &[Value],
) -> Result<BTreeSet<String>> {
    unchanged_ids(&from_catalogue(&previous.catalogue)?, &from_builds(builds)?)
}

// What a catalogue that carries plugins over must satisfy against the previous
// catalogue it was made from: every entry with a `release` reference is the
// previous entry byte for byte (parsed, compared without the reference), and its
// reference is the previous entry's own, or the previous release when that entry
// had none. Returns the releases it refers to.
pub(crate) fn check_references(
    catalogue: &Value,
    previous_catalogue: &Value,
    previous_id: u64,
    previous_tag: &str,
) -> Result<BTreeSet<(u64, String)>> {
    let mut referenced = BTreeSet::new();
    for plugin in catalogue["plugins"]
        .as_array()
        .ok_or("catalogue has no plugins")?
    {
        let id = plugin["id"].as_str().ok_or("plugin has no id")?;
        let [version] = plugin["versions"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        else {
            return Err(format!("{id}: a release catalogue lists exactly one version").into());
        };
        let Some(found) = reference(version)? else {
            continue;
        };
        let old = previous_catalogue["plugins"]
            .as_array()
            .and_then(|all| all.iter().find(|candidate| candidate["id"] == id))
            .and_then(|entry| entry["versions"].as_array())
            .and_then(|versions| versions.first())
            .ok_or_else(|| format!("{id}: a carried plugin is not in the previous catalogue"))?;
        let expected = reference(old)?.unwrap_or((previous_id, previous_tag.to_owned()));
        if found != expected {
            return Err(format!("{id}: the release reference is not the previous entry's").into());
        }
        let (mut now, mut before) = (version.clone(), old.clone());
        now.as_object_mut()
            .ok_or("version is not an object")?
            .remove("release");
        before
            .as_object_mut()
            .ok_or("version is not an object")?
            .remove("release");
        if now != before {
            return Err(format!("{id}: the carried entry differs from the previous one").into());
        }
        referenced.insert(found);
    }
    Ok(referenced)
}

// `verify-references`: the catalogue's carried entries are the previous
// release's, and every release they point at is published and immutable.
pub fn run_verify_references(
    repository: &str,
    catalogue: &Path,
    previous_catalogue: &Path,
    previous_tag: &str,
) -> Result<()> {
    valid_repository(repository)?;
    if !catalogue::tag_name(previous_tag) {
        return Err(format!("`{previous_tag}` is not a usable tag name").into());
    }
    let record = release_json(repository, &format!("releases/tags/{previous_tag}"))?;
    let previous_id = record["id"].as_u64().ok_or("previous release has no id")?;
    let referenced = check_references(
        &read_json(catalogue)?,
        &read_json(previous_catalogue)?,
        previous_id,
        previous_tag,
    )?;
    for (id, tag) in &referenced {
        check_release(
            &release_json(repository, &format!("releases/{id}"))?,
            *id,
            tag,
        )?;
    }
    println!(
        "{} release(s) referenced, all published and immutable",
        referenced.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(subject: &str) -> Commit {
        parse_commit(subject, "").unwrap_or_else(|| panic!("not conventional: {subject}"))
    }

    #[test]
    fn conventional_titles_are_parsed_and_others_ignored() {
        assert_eq!(
            commit("feat(clock)!: drop the 12 hour mode"),
            Commit {
                kind: "feat".into(),
                scope: Some("clock".into()),
                breaking: true
            }
        );
        assert!(!commit("fix: typo").breaking);
        assert!(
            parse_commit("fix: x", "text\nBREAKING CHANGE: gone")
                .unwrap()
                .breaking
        );
        for bad in [
            "Merge main",
            "feature: x",
            "feat:x",
            "feat(): x",
            "fix(a: x",
            "feat: ",
        ] {
            assert!(parse_commit(bad, "").is_none(), "{bad}");
        }
    }

    #[test]
    fn a_plugin_hears_its_own_and_shared_commits_but_not_another_plugins() {
        let ids: BTreeSet<String> = ["clock", "pane-pwd"].map(String::from).into();
        let commits = [commit("feat(pane-pwd): new"), commit("fix(sdk): shared")];
        assert_eq!(plugin_level("clock", &ids, &commits), Level::Patch);
        assert_eq!(plugin_level("pane-pwd", &ids, &commits), Level::Minor);
        // A feature in shared code (a scope that names no plugin) counts for every plugin.
        let shared = [commit("fix(clock): own"), commit("feat(sdk): shared")];
        assert_eq!(plugin_level("clock", &ids, &shared), Level::Minor);
        assert_eq!(plugin_level("pane-pwd", &ids, &shared), Level::Minor);
        let unscoped = [commit("feat: shared feature")];
        assert_eq!(plugin_level("clock", &ids, &unscoped), Level::Minor);
        assert_eq!(
            plugin_level("clock", &ids, &[commit("docs: x")]),
            Level::Patch
        );
        assert_eq!(plugin_level("clock", &ids, &[]), Level::Patch);
        let breaking = [commit("fix(clock)!: break"), commit("feat(clock): x")];
        assert_eq!(plugin_level("clock", &ids, &breaking), Level::Major);
    }

    #[test]
    fn bumps_follow_semver_and_treat_breaking_as_minor_below_one() {
        assert_eq!(bump("0.2.0", Level::Patch).unwrap(), "0.2.1");
        assert_eq!(bump("0.2.3", Level::Minor).unwrap(), "0.3.0");
        assert_eq!(bump("0.2.3", Level::Major).unwrap(), "0.3.0");
        assert_eq!(bump("1.2.3", Level::Major).unwrap(), "2.0.0");
        assert_eq!(bump("1.2.3", Level::Minor).unwrap(), "1.3.0");
        assert!(bump("1.2.3-rc.1", Level::Patch).is_err());
        assert!(bump("1.2", Level::Patch).is_err());
    }

    #[test]
    fn labels_continue_from_the_previous_tag() {
        assert_eq!(next_label(None, Level::Patch, None).unwrap(), "v0.2.0");
        assert_eq!(
            next_label(None, Level::Patch, Some(1)).unwrap(),
            "v0.2.0-rc.1"
        );
        assert_eq!(
            next_label(Some("v0.2.0"), Level::Patch, None).unwrap(),
            "v0.2.1"
        );
        assert_eq!(
            next_label(Some("v0.2.1"), Level::Minor, None).unwrap(),
            "v0.3.0"
        );
        assert_eq!(
            next_label(Some("v0.2.0-rc.1"), Level::Minor, Some(2)).unwrap(),
            "v0.2.0-rc.2"
        );
        assert_eq!(
            next_label(Some("v0.2.0-rc.1"), Level::Patch, None).unwrap(),
            "v0.2.0"
        );
        assert!(next_label(Some("0.2.0"), Level::Patch, None).is_err());
        assert!(next_label(None, Level::Patch, Some(0)).is_err());
    }

    fn member(path: &str, sha: &str) -> Value {
        json!({"path": path, "sha256": sha, "size": 1, "executable": false})
    }

    fn catalogue_of(plugins: &[(&str, &str, &str)]) -> Value {
        json!({"schema_version": 2, "plugins": plugins.iter().map(|(id, version, binary)| json!({
            "id": id, "versions": [{
                "manifest": {"id": id, "version": version, "api": {"major": 1, "minor": 2}},
                "revision": "r",
                "artifacts": [{"architecture": "amd64", "asset": format!("{id}.tar.gz"),
                    "members": [member("bin/x", binary)]}]}]})).collect::<Vec<_>>()})
    }

    fn build_of(plugins: &[(&str, &str, &str)]) -> Value {
        json!({"architecture": "amd64", "packages": plugins.iter().map(|(id, version, binary)| json!({
            "manifest": {"id": id, "version": version, "api": {"major": 1, "minor": 2}},
            "artifact": {"architecture": "amd64", "members": [member("bin/x", binary)]}}))
            .collect::<Vec<_>>()})
    }

    fn planned(
        previous: &[(&str, &str, &str)],
        current: &[(&str, &str, &str)],
        commits: &[Commit],
        tag: Option<&str>,
    ) -> Result<Plan> {
        plan(
            &from_catalogue(&catalogue_of(previous))?,
            &from_builds(&[build_of(current)])?,
            commits,
            tag,
            None,
        )
    }

    #[test]
    fn only_changed_plugins_are_bumped_and_the_label_takes_the_largest_level() {
        let previous = [("clock", "0.2.0", "a"), ("pane-pwd", "0.2.0", "b")];
        let current = [("clock", "0.2.0", "a2"), ("pane-pwd", "0.2.0", "b")];
        let commits = [commit("feat(clock): x"), commit("fix(pane-pwd): y")];
        let plan = planned(&previous, &current, &commits, Some("v0.2.0")).unwrap();
        assert_eq!(plan.unchanged, ["pane-pwd"]);
        assert_eq!(plan.bumps.len(), 1);
        assert_eq!(
            (plan.bumps[0].to.as_str(), plan.bumps[0].level),
            ("0.3.0", Level::Minor)
        );
        assert_eq!(plan.label, "v0.3.0");
    }

    #[test]
    fn a_changed_plugin_without_commits_gets_a_patch() {
        let plan = planned(
            &[("clock", "0.2.0", "a")],
            &[("clock", "0.2.0", "a2")],
            &[],
            Some("v0.2.0"),
        )
        .unwrap();
        assert_eq!(plan.bumps[0].to, "0.2.1");
        assert_eq!(plan.label, "v0.2.1");
    }

    #[test]
    fn nothing_changed_means_nothing_to_release() {
        let error = planned(
            &[("clock", "0.2.0", "a")],
            &[("clock", "0.2.0", "a")],
            &[],
            Some("v0.2.0"),
        );
        assert!(error.is_err());
    }

    #[test]
    fn an_already_raised_version_is_kept_and_not_rewritten() {
        // The release commit: the manifest already holds the version the
        // commits call for (a feature, so 0.3.0), and the label follows it.
        let commits = [commit("feat(clock): x")];
        let plan = planned(
            &[("clock", "0.2.0", "a")],
            &[("clock", "0.3.0", "a2")],
            &commits,
            Some("v0.2.0"),
        )
        .unwrap();
        assert_eq!(
            (plan.bumps[0].write, plan.bumps[0].level),
            (false, Level::Minor)
        );
        assert_eq!(plan.label, "v0.3.0");
        // A hand-picked version that the commits do not call for is refused.
        let error = planned(
            &[("clock", "0.2.0", "a")],
            &[("clock", "0.4.0", "a2")],
            &commits,
            Some("v0.2.0"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("differs from the computed 0.3.0"), "{error}");
    }

    #[test]
    fn inconsistent_versions_stop_the_plan() {
        let previous = [("clock", "0.2.0", "a")];
        assert!(planned(&previous, &[("clock", "0.2.1", "a")], &[], Some("v0.2.0")).is_err());
        assert!(planned(&previous, &[("clock", "0.1.0", "a2")], &[], Some("v0.2.0")).is_err());
    }

    #[test]
    fn the_first_release_has_only_new_plugins_and_the_first_label() {
        let plan = plan(
            &BTreeMap::new(),
            &from_builds(&[build_of(&[("clock", "0.2.0", "a")])]).unwrap(),
            &[],
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            (plan.label.as_str(), plan.new.len(), plan.bumps.len()),
            ("v0.2.0", 1, 0)
        );
    }

    /// A directory unique to one test that is removed when the test ends,
    /// whether it passes or fails.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let number = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "among-release-{name}-{}-{number}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Runs git in `root` with no user configuration, so that a developer's
    /// signing or hooks cannot change what a test does.
    fn run_git(root: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?}");
    }

    #[test]
    fn apply_changes_only_the_version_text() {
        let scratch = TempDir::new("apply");
        let root = scratch.0.clone();
        let directory = root.join("plugins/clock");
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("among-plugin.json");
        std::fs::write(
            &file,
            "{\n  \"id\": \"clock\",\n  \"version\": \"0.2.0\",\n  \"x\": 1}\n",
        )
        .unwrap();
        let plan = Plan {
            label: "v0.2.1".into(),
            bumps: vec![Bump {
                plugin: "clock".into(),
                from: "0.2.0".into(),
                to: "0.2.1".into(),
                level: Level::Patch,
                write: true,
            }],
            unchanged: vec![],
            new: vec![],
        };
        apply(&root, &plan).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "{\n  \"id\": \"clock\",\n  \"version\": \"0.2.1\",\n  \"x\": 1}\n"
        );
        // A second application finds no `0.2.0` to replace and refuses.
        assert!(apply(&root, &plan).is_err());
    }

    #[test]
    fn commits_are_read_from_the_previous_tag_to_head() {
        let scratch = TempDir::new("git");
        let root = scratch.0.clone();
        let run = |arguments: &[&str]| run_git(&root, arguments);
        run(&["init", "-q"]);
        run(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "feat(clock): before the tag",
        ]);
        run(&["tag", "v0.2.0"]);
        run(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "fix(pane-pwd): after",
            "-m",
            "BREAKING CHANGE: x",
        ]);
        run(&["commit", "-q", "--allow-empty", "-m", "Merge something"]);
        let commits = commits_since(&root, Some("v0.2.0")).unwrap();
        assert_eq!(commits.len(), 1, "{commits:?}");
        assert_eq!(commits[0].scope.as_deref(), Some("pane-pwd"));
        assert!(commits[0].breaking);
        assert!(commits_since(&root, None).unwrap().is_empty());
        assert!(commits_since(&root, Some("--all")).is_err());
    }

    /// A clone of a bare remote on `main` with one plugin manifest, as the
    /// operator of a release pull request has it.
    fn release_fixture(name: &str) -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
        let scratch = TempDir::new(name);
        let (remote, work) = (scratch.0.join("remote.git"), scratch.0.join("work"));
        std::fs::create_dir_all(&remote).unwrap();
        run_git(&remote, &["init", "-q", "--bare"]);
        std::fs::create_dir_all(work.join("plugins/clock")).unwrap();
        run_git(&work, &["init", "-q"]);
        run_git(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        std::fs::write(
            work.join("plugins/clock/among-plugin.json"),
            "{\n  \"id\": \"clock\",\n  \"version\": \"0.2.0\"\n}\n",
        )
        .unwrap();
        run_git(&work, &["add", "-A"]);
        run_git(&work, &["commit", "-q", "-m", "feat: start"]);
        run_git(&work, &["push", "-q", "-u", "origin", "main"]);
        (scratch, remote, work)
    }

    fn fixed_plan() -> Plan {
        Plan {
            label: "v0.2.1".into(),
            bumps: vec![Bump {
                plugin: "clock".into(),
                from: "0.2.0".into(),
                to: "0.2.1".into(),
                level: Level::Patch,
                write: true,
            }],
            unchanged: vec![],
            new: vec![],
        }
    }

    fn open_release_pr(work: &Path, dry_run: bool) -> Result<()> {
        let arguments = PlanArguments {
            root: work,
            repository: "Among-Terminal/among-plugins",
            candidate: None,
            builds: &[],
        };
        release_pr_with(&arguments, dry_run, |_| Ok(fixed_plan()))
    }

    fn branches(work: &Path) -> String {
        git(work, &["branch", "--list"]).unwrap()
    }

    #[test]
    fn a_release_pull_request_is_refused_from_a_dirty_tree_another_branch_or_a_stale_main() {
        let (_scratch, _remote, work) = release_fixture("guards");
        std::fs::write(work.join("stray.txt"), "x").unwrap();
        let error = open_release_pr(&work, false).unwrap_err().to_string();
        assert!(error.contains("not clean"), "{error}");
        std::fs::remove_file(work.join("stray.txt")).unwrap();

        run_git(
            &work,
            &["commit", "-q", "--allow-empty", "-m", "fix: unpushed"],
        );
        let error = open_release_pr(&work, false).unwrap_err().to_string();
        assert!(error.contains("not equal to origin/main"), "{error}");
        run_git(&work, &["reset", "-q", "--hard", "origin/main"]);

        run_git(&work, &["switch", "-q", "-c", "topic"]);
        let error = open_release_pr(&work, false).unwrap_err().to_string();
        assert!(error.contains("from main"), "{error}");
        assert!(!branches(&work).contains("release/"));
    }

    #[test]
    fn a_release_pull_request_is_refused_when_the_tag_exists_locally_or_on_the_remote() {
        let (_scratch, _remote, work) = release_fixture("tags");
        run_git(&work, &["tag", "v0.2.1"]);
        let error = open_release_pr(&work, false).unwrap_err().to_string();
        assert!(error.contains("v0.2.1 already exists"), "{error}");
        // Only on the remote: pushed, then deleted locally.
        run_git(&work, &["push", "-q", "origin", "v0.2.1"]);
        run_git(&work, &["tag", "-d", "v0.2.1"]);
        let error = open_release_pr(&work, false).unwrap_err().to_string();
        assert!(error.contains("v0.2.1 already exists"), "{error}");
        assert!(!branches(&work).contains("release/"));
    }

    #[test]
    fn a_dry_run_changes_nothing() {
        let (_scratch, _remote, work) = release_fixture("dry");
        open_release_pr(&work, true).unwrap();
        assert!(!branches(&work).contains("release/"));
        assert!(git(&work, &["status", "--porcelain"]).unwrap().is_empty());
    }

    #[test]
    fn a_failed_push_puts_the_working_tree_back_on_main() {
        let (_scratch, remote, work) = release_fixture("push");
        // The remote refuses every push from now on.
        let hook = remote.join("hooks/pre-receive");
        std::fs::create_dir_all(remote.join("hooks")).unwrap();
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let error = open_release_pr(&work, false).unwrap_err().to_string();
        assert!(
            error.contains("push") || error.contains("subprocess failed"),
            "{error}"
        );
        assert_eq!(
            git(&work, &["rev-parse", "--abbrev-ref", "HEAD"])
                .unwrap()
                .trim(),
            "main"
        );
        assert!(!branches(&work).contains("release/"));
        assert!(git(&work, &["status", "--porcelain"]).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(work.join("plugins/clock/among-plugin.json")).unwrap(),
            "{\n  \"id\": \"clock\",\n  \"version\": \"0.2.0\"\n}\n"
        );
    }

    #[test]
    fn build_records_must_describe_the_checked_out_commit() {
        let (_scratch, _remote, work) = release_fixture("builds");
        let head = git(&work, &["rev-parse", "HEAD"]).unwrap();
        let record = |revision: &str| json!({"revision": revision, "packages": []});
        assert!(require_builds_at_head(&work, &[record(head.trim())]).is_ok());
        let error = require_builds_at_head(&work, &[record(head.trim()), record(&"0".repeat(40))])
            .unwrap_err()
            .to_string();
        assert!(error.contains("not for the checked out"), "{error}");
        assert!(require_builds_at_head(&work, &[json!({"packages": []})]).is_err());
    }

    #[test]
    fn a_release_commit_may_change_only_version_lines() {
        let clean =
            "diff --git a/plugins/clock/among-plugin.json b/plugins/clock/among-plugin.json\n\
            --- a/plugins/clock/among-plugin.json\n+++ b/plugins/clock/among-plugin.json\n\
            @@ -3 +3 @@\n-  \"version\": \"0.2.0\",\n+  \"version\": \"0.2.1\",\n";
        assert!(only_version_lines(clean).is_ok());
        let extra = format!("{clean}-  \"name\": \"Clock\",\n+  \"name\": \"Clocks\",\n");
        assert!(only_version_lines(&extra).is_err());
        let unpaired = format!("{clean}+  \"version\": \"9.9.9\",\n");
        assert!(only_version_lines(&unpaired).is_err());
        assert!(only_version_lines("").is_err());
        let renamed_key = "-  \"version\": \"0.2.0\",\n+  \"versions\": \"0.2.1\",\n";
        assert!(only_version_lines(renamed_key).is_err());
    }

    #[test]
    fn only_release_commits_that_touch_nothing_but_manifests_name_a_label() {
        let manifests = [
            "plugins/clock/among-plugin.json",
            "plugins/pane-pwd/among-plugin.json",
        ];
        let label = |subject: &str, files: &[&str]| snapshot_label(subject, files);
        assert_eq!(
            label("chore(release): v0.2.1", &manifests)
                .unwrap()
                .as_deref(),
            Some("v0.2.1")
        );
        assert_eq!(
            label("chore(release): v0.2.0-rc.1 (#18)", &manifests)
                .unwrap()
                .as_deref(),
            Some("v0.2.0-rc.1")
        );
        assert_eq!(label("feat: x", &manifests).unwrap(), None);
        assert_eq!(label("chore: release v0.2.1", &manifests).unwrap(), None);
        for bad in [
            "chore(release): 0.2.1",
            "chore(release): v0.2",
            "chore(release): v0.2.1-beta",
            "chore(release): v0.2.1-rc.0",
            "chore(release): v0.2.1-rc.01",
            "chore(release): v0.2.1 (#x)",
            "chore(release): v0.2.1 (#1",
            "chore(release): ../v0.2.1",
        ] {
            assert!(label(bad, &manifests).is_err(), "{bad}");
        }
        assert!(label("chore(release): v0.2.1", &["src/clock.rs"]).is_err());
        assert!(label("chore(release): v0.2.1", &["plugins/clock/other.json"]).is_err());
        assert!(label(
            "chore(release): v0.2.1",
            &["plugins/Clock/among-plugin.json"]
        )
        .is_err());
        assert!(label("chore(release): v0.2.1", &[]).is_err());
    }

    fn release(id: u64, tag: &str, draft: bool, immutable: bool) -> Value {
        json!({"id": id, "tag_name": tag, "draft": draft, "immutable": immutable})
    }

    fn carry(
        catalogue: &Value,
        record: Value,
        fresh: &Value,
        unchanged: &BTreeSet<String>,
    ) -> Result<Value> {
        let previous = Previous {
            catalogue: catalogue.clone(),
            release: record,
        };
        carry_over(&previous, fresh, unchanged)
    }

    #[test]
    fn unchanged_plugins_keep_their_previous_entry_and_point_at_its_release() {
        let previous = catalogue_of(&[("clock", "0.2.0", "a"), ("pane-pwd", "0.2.0", "b")]);
        let fresh = catalogue_of(&[("clock", "0.2.0", "REBUILT"), ("pane-pwd", "0.2.1", "b2")]);
        let unchanged = BTreeSet::from(["clock".to_owned()]);
        let result = carry(
            &previous,
            release(7, "v0.2.0", false, true),
            &fresh,
            &unchanged,
        )
        .unwrap();
        let clock = &result["plugins"][0]["versions"][0];
        assert_eq!(clock["artifacts"][0]["members"][0]["sha256"], "a");
        assert_eq!(clock["release"], json!({"id": 7, "tag": "v0.2.0"}));
        assert_eq!(clock["manifest"]["version"], "0.2.0");
        // The changed plugin is the fresh entry, with no reference.
        let changed = &result["plugins"][1]["versions"][0];
        assert_eq!(changed["manifest"]["version"], "0.2.1");
        assert!(changed.get("release").is_none());
    }

    #[test]
    fn an_entry_that_already_points_elsewhere_keeps_its_reference() {
        let mut previous = catalogue_of(&[("clock", "0.2.0", "a")]);
        previous["plugins"][0]["versions"][0]["release"] = json!({"id": 3, "tag": "v0.2.0"});
        let fresh = catalogue_of(&[("clock", "0.2.0", "REBUILT")]);
        let unchanged = BTreeSet::from(["clock".to_owned()]);
        let result = carry(
            &previous,
            release(9, "v0.2.1", false, true),
            &fresh,
            &unchanged,
        )
        .unwrap();
        assert_eq!(
            result["plugins"][0]["versions"][0]["release"],
            json!({"id": 3, "tag": "v0.2.0"})
        );
    }

    #[test]
    fn carrying_over_refuses_missing_entries_and_unsafe_releases() {
        let previous = catalogue_of(&[("clock", "0.2.0", "a")]);
        let fresh = catalogue_of(&[("clock", "0.2.0", "x"), ("pane-pwd", "0.2.0", "y")]);
        let good = release(7, "v0.2.0", false, true);
        let missing = BTreeSet::from(["pane-pwd".to_owned()]);
        assert!(carry(&previous, good.clone(), &fresh, &missing).is_err());
        let absent = BTreeSet::from(["system-status".to_owned()]);
        assert!(carry(&previous, good, &fresh, &absent).is_err());
        let unchanged = BTreeSet::from(["clock".to_owned()]);
        assert!(carry(
            &previous,
            release(7, "v0.2.0", true, true),
            &fresh,
            &unchanged
        )
        .is_err());
        assert!(carry(
            &previous,
            release(7, "v0.2.0", false, false),
            &fresh,
            &unchanged
        )
        .is_err());
    }

    #[test]
    fn release_references_are_checked_for_id_tag_publication_and_immutability() {
        assert!(check_release(&release(1, "v1", false, true), 1, "v1").is_ok());
        assert!(check_release(&release(1, "v1", false, true), 2, "v1").is_err());
        assert!(check_release(&release(1, "v1", false, true), 1, "v2").is_err());
        assert!(check_release(&release(1, "v1", true, true), 1, "v1").is_err());
        assert!(check_release(&release(1, "v1", false, false), 1, "v1").is_err());
        assert!(
            check_release(&json!({"id": 1, "tag_name": "v1", "draft": false}), 1, "v1").is_err()
        );
    }

    #[test]
    fn the_unchanged_set_requires_content_and_version_to_agree() {
        let previous = from_catalogue(&catalogue_of(&[
            ("a", "0.2.0", "x"),
            ("b", "0.2.0", "y"),
            ("c", "0.2.0", "z"),
        ]))
        .unwrap();
        let current = from_builds(&[build_of(&[
            ("a", "0.2.0", "x"),
            ("b", "0.3.0", "y2"),
            ("d", "0.1.0", "n"),
        ])])
        .unwrap();
        assert_eq!(
            unchanged_ids(&previous, &current).unwrap(),
            BTreeSet::from(["a".to_owned()])
        );
        let stale = from_builds(&[build_of(&[("a", "0.2.0", "x2")])]).unwrap();
        assert!(unchanged_ids(&previous, &stale).is_err());
        let raised = from_builds(&[build_of(&[("a", "0.2.1", "x")])]).unwrap();
        assert!(unchanged_ids(&previous, &raised).is_err());
    }
}
