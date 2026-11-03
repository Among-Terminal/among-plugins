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
    // Without --rc the release is stable and is compared with the last stable one.
    let (tag, bytes) = fetch_previous(arguments.repository, None, arguments.candidate.is_none())?;
    let plan = plan_against(
        arguments.root,
        tag.as_deref(),
        &serde_json::from_slice(&bytes)?,
        &read_builds(arguments.builds)?,
        arguments.candidate,
    )?;
    // A stable label whose candidates exist is made from one of them.
    refuse_stable_after_candidates(
        &plan.label,
        &candidate_tags(arguments.repository, &plan.label)?,
    )?;
    Ok(plan)
}

// The tags that start with `prefix`, from the matching-refs list (an empty list
// is an answer, an error is an error; nothing is read from error text).
fn matching_tags(repository: &str, prefix: &str) -> Result<Vec<String>> {
    let refs = release_json(
        repository,
        &format!("git/matching-refs/tags/{prefix}?per_page=100"),
    )?;
    let list = refs.as_array().ok_or("matching refs are not a list")?;
    if list.len() >= 100 {
        return Err(format!("100 or more tags start with {prefix}").into());
    }
    list.iter()
        .map(|entry| {
            entry["ref"]
                .as_str()
                .and_then(|name| name.strip_prefix("refs/tags/"))
                .map(str::to_owned)
                .ok_or_else(|| "a matching ref has no tag name".into())
        })
        .collect()
}

fn tag_exists(repository: &str, tag: &str) -> Result<bool> {
    Ok(matching_tags(repository, tag)?
        .iter()
        .any(|found| found == tag))
}

// The release candidate tags of a stable label: `v0.2.0-rc.1`, `v0.2.0-rc.2`...
fn candidate_tags(repository: &str, stable: &str) -> Result<Vec<String>> {
    if stable.contains('-') {
        return Ok(Vec::new());
    }
    Ok(matching_tags(repository, &format!("{stable}-rc."))?
        .into_iter()
        .filter(|tag| stable_label(tag).is_ok_and(|label| label == stable))
        .collect())
}

// A stable release whose release candidates exist is not prepared, planned or
// published as a new release: the application treats the same version with
// other bytes as a publisher inconsistency. It is the candidate's bytes.
pub(crate) fn refuse_stable_after_candidates(label: &str, candidates: &[String]) -> Result<()> {
    if label.contains('-') || candidates.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{label} has release candidates ({}): make the stable release from one with `promote-candidate`",
        candidates.join(", ")
    )
    .into())
}

// What may be published under `tag`: a candidate or a tag without candidates
// freely, a stable tag that has candidates only from one of them.
pub(crate) fn check_stable_publication(
    tag: &str,
    candidates: &[String],
    candidate_input: Option<&str>,
) -> Result<()> {
    match candidate_input {
        None => refuse_stable_after_candidates(tag, candidates),
        Some(chosen) if candidates.iter().any(|known| known == chosen) => Ok(()),
        Some(chosen) => Err(format!("{chosen} is not a release candidate of {tag}").into()),
    }
}

// `check-stable-release`: the workflow's guard before it prepares or publishes.
pub fn run_check_stable(repository: &str, tag: &str, candidate: Option<&str>) -> Result<()> {
    valid_repository(repository)?;
    if !catalogue::tag_name(tag) {
        return Err(format!("`{tag}` is not a usable tag name").into());
    }
    check_stable_publication(tag, &candidate_tags(repository, tag)?, candidate)
}

// What the commit of a candidate must be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommitRule {
    // The release commit, titled `chore(release): <candidate tag>` (with the
    // pull request number a squash adds). Every release has one, because a
    // release raises versions and that is what the commit does.
    ReleaseCommit,
    // The first release of the repository raises nothing: its manifests already
    // carry the versions to release and no previous release exists, so there is
    // no release commit. Any commit that is an ancestor of the default branch
    // will do (`on_main` says whether this one is).
    FirstRelease { on_main: bool },
}

// The first release is the first label with no other release that has a
// catalogue (`previous_release` is what the lookup of the previous release
// found, this candidate left out) and no other candidate tag. A second
// candidate of the first label is not the first release, and neither is a
// candidate whose sibling's release was deleted: the tag remains.
pub(crate) fn is_first_release(
    stable_label: &str,
    previous_release: Option<&str>,
    candidate_tags: &[String],
    this_candidate: &str,
) -> bool {
    stable_label == FIRST_LABEL
        && previous_release.is_none()
        && candidate_tags.iter().all(|tag| tag == this_candidate)
}

pub(crate) fn commit_rule(first_release: bool, on_main: bool) -> CommitRule {
    if first_release {
        CommitRule::FirstRelease { on_main }
    } else {
        CommitRule::ReleaseCommit
    }
}

// Whether the comparison of `commit` with the default branch has the commit as
// the merge base, which is what being an ancestor of (or being) it means.
fn merge_base_is(comparison: &Value, commit: &str) -> bool {
    comparison["merge_base_commit"]["sha"].as_str() == Some(commit)
}

// The commit of a candidate: titled by `rule`, and checked by a successful push
// run of the repository's checks on the default branch, which verified it.
pub(crate) fn check_candidate_commit(
    message: &str,
    runs: &Value,
    candidate_tag: &str,
    rule: CommitRule,
) -> Result<()> {
    match rule {
        CommitRule::ReleaseCommit => {
            let subject = message.lines().next().unwrap_or_default();
            let subject = subject
                .split_once(" (#")
                .map_or(subject, |(title, _)| title);
            if subject != format!("chore(release): {candidate_tag}") {
                return Err(format!(
                    "the commit of {candidate_tag} is not its release commit (its title is `{subject}`)"
                )
                .into());
            }
        }
        CommitRule::FirstRelease { on_main: false } => {
            return Err(format!(
                "the commit of {candidate_tag} is not an ancestor of the default branch"
            )
            .into());
        }
        CommitRule::FirstRelease { on_main: true } => {}
    }
    let verified = runs["workflow_runs"].as_array().is_some_and(|all| {
        all.iter().any(|run| {
            run["name"] == "Locked Rust source checks"
                && run["event"] == "push"
                && run["head_branch"] == "main"
                && run["conclusion"] == "success"
        })
    });
    if !verified {
        return Err(format!(
            "no successful push run of the checks exists for the commit of {candidate_tag}"
        )
        .into());
    }
    Ok(())
}

// Whether `commit` is an ancestor of (or is) the default branch: the merge base
// of the two is the commit itself.
fn commit_on_main(repository: &str, commit: &str) -> Result<bool> {
    let comparison = release_json(repository, &format!("compare/{commit}...main"))?;
    Ok(merge_base_is(&comparison, commit))
}

fn require_candidate_commit(repository: &str, commit: &str, candidate_tag: &str) -> Result<()> {
    let record = release_json(repository, &format!("commits/{commit}"))?;
    let runs = release_json(
        repository,
        &format!("actions/runs?head_sha={commit}&per_page=100"),
    )?;
    // The first release has no release commit; every other one has.
    let stable = stable_label(candidate_tag)?;
    let previous = fetch_previous(repository, Some(candidate_tag), false)?.0;
    let first = is_first_release(
        &stable,
        previous.as_deref(),
        &candidate_tags(repository, &stable)?,
        candidate_tag,
    );
    let on_main = first && commit_on_main(repository, commit)?;
    let rule = commit_rule(first, on_main);
    check_candidate_commit(
        record["commit"]["message"].as_str().unwrap_or_default(),
        &runs,
        candidate_tag,
        rule,
    )
}

// What the operator does for the first release: the candidate tag goes on a
// commit of the default branch that the checks have passed on.
fn first_release_text(repository: &str, candidate: &str, commit: &str) -> String {
    format!(
        "the first release: no release with a catalogue exists, and {candidate} may go on\n\
         {commit}, a commit of the default branch with a successful push run of the checks.\n\n\
         1. gh api repos/{repository}/git/refs -f ref=refs/tags/{candidate} -f sha={commit}\n\
         2. run `Official plugin candidate` on {candidate} with publish=false, inspect the run,\n\
         \x20  get the owner's approval of the exact tag, commit and bytes, then run it with\n\
         \x20  publish=true, prerelease=true and the approved run, artifact and checksum inputs\n\
         3. later, `promote-candidate {repository} {candidate}` for the stable release\n"
    )
}

// `first-release`: checks that a commit can carry the first candidate and prints
// the steps. It creates nothing; the tag is the owner's separate decision.
pub fn run_first_release(repository: &str, commit: &str) -> Result<()> {
    valid_repository(repository)?;
    if !crate::package::hex(commit, 40) {
        return Err("the commit must be a full 40-digit commit id".into());
    }
    if let (Some(tag), _) = fetch_previous(repository, None, false)? {
        return Err(format!(
            "{tag} already is a release with a catalogue: this is not the first release"
        )
        .into());
    }
    let candidate = format!("{FIRST_LABEL}-rc.1");
    // A candidate tag that is left over from a deleted release counts as well.
    if let Some(tag) = candidate_tags(repository, FIRST_LABEL)?.first() {
        return Err(format!("the tag {tag} already exists").into());
    }
    if tag_exists(repository, FIRST_LABEL)? {
        return Err(format!("the tag {FIRST_LABEL} already exists").into());
    }
    let runs = release_json(
        repository,
        &format!("actions/runs?head_sha={commit}&per_page=100"),
    )?;
    check_candidate_commit(
        "",
        &runs,
        &candidate,
        CommitRule::FirstRelease {
            on_main: commit_on_main(repository, commit)?,
        },
    )?;
    print!("{}", first_release_text(repository, &candidate, commit));
    Ok(())
}

// Writes `catalogue.json` and, when there is a previous release, its record
// `release.json` into the new directory, so that a release can carry unchanged
// plugins over from it. `exclude` leaves one tag out.
pub fn write_previous_release(
    repository: &str,
    exclude: Option<&str>,
    directory: &Path,
) -> Result<()> {
    // The tag being released decides the baseline: a stable tag (no hyphen)
    // takes the last stable release, a candidate the newest of any kind.
    let stable_only = exclude.is_some_and(|tag| !tag.contains('-'));
    let (tag, bytes) = fetch_previous(repository, exclude, stable_only)?;
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
    release_pr_with(arguments, dry_run, run_plan, open_pull_request)
}

// Opens the pull request of a pushed release branch with the operator's `gh`.
fn open_pull_request(root: &Path, branch: &str, title: &str, body: &str) -> Result<()> {
    process::text(Command::new("gh").current_dir(root).args([
        "pr", "create", "--base", "main", "--head", branch, "--title", title, "--body", body,
    ]))?;
    Ok(())
}

// The planner and the pull request opener are parameters so that the guards and
// the clean-up can be tested without GitHub.
fn release_pr_with(
    arguments: &PlanArguments,
    dry_run: bool,
    planner: impl Fn(&PlanArguments) -> Result<Plan>,
    opener: impl Fn(&Path, &str, &str, &str) -> Result<()>,
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
    // A failure before the push has succeeded puts the working tree back on
    // main and removes the branch, so that a retry starts from the state the
    // operator began with.
    if let Err(error) = push_branch(root, &plan, &branch) {
        let _ = git(root, &["checkout", "-q", "--", "."]);
        let _ = git(root, &["switch", "-q", "main"]);
        let _ = git(root, &["branch", "-q", "-D", &branch]);
        return Err(error);
    }
    // From here the branch exists on origin and is kept: deleting the local
    // copy would make a retry fail as a non-fast-forward push.
    let _ = git(root, &["switch", "-q", "main"]);
    let title = format!("chore(release): {}", plan.label);
    opener(root, &branch, &title, &description(&plan)).map_err(|error| {
        format!(
            "the branch {branch} exists locally and on origin, with the release commit, but the \
             pull request was not opened ({error}); open it with `gh pr create --base main \
             --head {branch} --title \"{title}\"`, or delete the branch locally and on origin \
             and run this command again"
        )
        .into()
    })
}

// Raises the manifests, commits and pushes the release branch.
fn push_branch(root: &Path, plan: &Plan, branch: &str) -> Result<()> {
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
    let label = release_commit_label(root)?;
    refuse_stable_after_candidates(&label, &candidate_tags(repository, &label)?)?;
    // A stable release is compared with the last stable one, a candidate with
    // the newest release of any kind.
    let (tag, bytes) = fetch_previous(repository, Some(&label), !label.contains('-'))?;
    verify_release_commit(
        root,
        tag.as_deref(),
        &serde_json::from_slice(&bytes)?,
        &read_builds(builds)?,
    )
}

// The label of the checked out commit if it is a release commit.
fn release_commit_label(root: &Path) -> Result<String> {
    let subject = git(root, &["show", "-s", "--format=%s", "HEAD"])?;
    let files = changed_files(root)?;
    let files: Vec<&str> = files.iter().map(String::as_str).collect();
    snapshot_label(subject.trim_end(), &files)?
        .ok_or_else(|| "the commit is not a release commit".into())
}

fn changed_files(root: &Path) -> Result<Vec<String>> {
    Ok(git(
        root,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "--no-renames",
            "HEAD",
        ],
    )?
    .lines()
    .map(str::to_owned)
    .collect())
}

// `snapshot_verify` without the network: the previous release is a parameter.
fn verify_release_commit(
    root: &Path,
    previous_tag: Option<&str>,
    previous_catalogue: &Value,
    records: &[Value],
) -> Result<String> {
    let label = release_commit_label(root)?;
    only_versions_changed(root, &changed_files(root)?)?;
    require_builds_at_head(root, records)?;
    let report = changed::check(previous_catalogue, records, false)?;
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
    let plan = plan_against(root, previous_tag, previous_catalogue, records, candidate)?;
    if plan.label != label {
        return Err(format!(
            "the release commit is titled {label} but the next label is {}",
            plan.label
        )
        .into());
    }
    Ok(label)
}

// Each manifest the release commit changed must differ from its parent's only
// in `version`, compared as parsed JSON (so formatting and key order do not
// matter and a changed key or value anywhere else does).
fn only_versions_changed(root: &Path, files: &[String]) -> Result<()> {
    if files.is_empty() {
        return Err("the release commit changes no manifest".into());
    }
    for file in files {
        let parse = |revision: &str| -> Result<Value> {
            let text = git(root, &["show", &format!("{revision}:{file}")])
                .map_err(|_| format!("{file} does not exist in {revision}"))?;
            Ok(serde_json::from_str(&text)?)
        };
        let (mut before, mut after) = (parse("HEAD^")?, parse("HEAD")?);
        let versions = (before["version"].clone(), after["version"].clone());
        for manifest in [&mut before, &mut after] {
            manifest
                .as_object_mut()
                .ok_or_else(|| format!("{file} is not a JSON object"))?
                .remove("version");
        }
        if before != after {
            return Err(
                format!("the release commit changes more than the version of {file}").into(),
            );
        }
        if versions.0 == versions.1 {
            return Err(format!("the release commit does not change the version of {file}").into());
        }
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

// A stable release made from a release candidate. The application installs a
// candidate in development mode and offers the stable release of the same
// version as an update only if the manifest digest, the package digest and the
// commit the tag resolves to are the candidate's; anything else is refused as a
// publisher inconsistency. So the stable release is not a new build: it is the
// candidate's approved bytes, published again under the stable tag, which sits
// on the candidate's commit.

// `v0.2.0` for `v0.2.0-rc.1`.
pub(crate) fn stable_label(candidate: &str) -> Result<String> {
    let (stable, number) = candidate
        .split_once("-rc.")
        .ok_or_else(|| format!("`{candidate}` is not a release candidate tag"))?;
    let core = stable
        .strip_prefix('v')
        .ok_or_else(|| format!("`{candidate}` does not start with v"))?;
    if !Version::parse(core)?.pre.is_empty()
        || number.is_empty()
        || number.starts_with('0')
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(format!("`{candidate}` is not a release candidate tag").into());
    }
    Ok(stable.to_owned())
}

fn release_assets(release: &Value) -> Result<BTreeMap<String, (u64, String)>> {
    let mut found = BTreeMap::new();
    for asset in release["assets"]
        .as_array()
        .ok_or("release lists no assets")?
    {
        let name = asset["name"].as_str().ok_or("asset has no name")?;
        let digest = asset["digest"]
            .as_str()
            .and_then(|digest| digest.strip_prefix("sha256:"))
            .ok_or_else(|| format!("asset {name} has no sha256 digest"))?;
        if asset["state"] != "uploaded"
            || found
                .insert(
                    name.to_owned(),
                    (
                        asset["size"].as_u64().ok_or("asset has no size")?,
                        digest.to_owned(),
                    ),
                )
                .is_some()
        {
            return Err(format!("asset {name} is not uploaded exactly once").into());
        }
    }
    Ok(found)
}

// The prepared stable release must equal the published candidate: the same
// repository and commit in its binding, the same asset names, sizes and
// digests (so the same packages, source archive, catalogue and checksums).
pub(crate) fn check_candidate(
    candidate: &Value,
    candidate_tag: &str,
    inventory: &crate::author::Inventory,
) -> Result<()> {
    if candidate["tag_name"].as_str() != Some(candidate_tag)
        || candidate["draft"].as_bool() != Some(false)
        || candidate["prerelease"].as_bool() != Some(true)
        || candidate["immutable"].as_bool() != Some(true)
    {
        return Err(format!("{candidate_tag} is not a published, immutable prerelease").into());
    }
    let binding = crate::author::body_binding(candidate)?;
    if binding.repository_id != inventory.repository_id || binding.commit != inventory.revision {
        return Err(format!(
            "{candidate_tag} was built from {}, not from {}",
            binding.commit, inventory.revision
        )
        .into());
    }
    let wanted: BTreeMap<_, _> = inventory
        .assets
        .iter()
        .map(|asset| (asset.name.clone(), (asset.size, asset.sha256.clone())))
        .collect();
    let published = release_assets(candidate)?;
    if wanted != published {
        let differing: Vec<_> = wanted
            .keys()
            .chain(published.keys())
            .filter(|name| wanted.get(*name) != published.get(*name))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(5)
            .collect();
        return Err(format!(
            "the stable release would differ from {candidate_tag} in: {}",
            differing.join(", ")
        )
        .into());
    }
    Ok(())
}

// `verify-candidate`: the prepared inventory of the stable tag against the
// published candidate, and the candidate's tag still on the same commit.
pub fn run_verify_candidate(
    repository: &str,
    candidate_tag: &str,
    stable_tag: &str,
    inventory: &Path,
) -> Result<()> {
    valid_repository(repository)?;
    if stable_label(candidate_tag)? != stable_tag {
        return Err(format!("{candidate_tag} is not a candidate of {stable_tag}").into());
    }
    let inventory = crate::author::read_inventory(inventory)?;
    let release = release_json(repository, &format!("releases/tags/{candidate_tag}"))?;
    check_candidate(&release, candidate_tag, &inventory)?;
    let resolved = catalogue::binding_in(
        inventory.repository_id,
        candidate_tag,
        std::time::Instant::now() + std::time::Duration::from_secs(120),
    )?;
    if resolved["commit"].as_str() != Some(inventory.revision.as_str()) {
        return Err(format!("{candidate_tag} no longer points at {}", inventory.revision).into());
    }
    require_candidate_commit(repository, &inventory.revision, candidate_tag)?;
    println!("{stable_tag} is {candidate_tag}: same commit, same assets");
    Ok(())
}

// What the owner needs to publish the stable release from a candidate.
fn promotion_text(
    repository: &str,
    candidate: &str,
    stable: &str,
    commit: &str,
    approved_sha256sums: &str,
    preparations: &[(u64, u64)],
) -> String {
    let mut text = format!(
        "candidate {candidate} was built from {commit}\n\
         stable tag {stable} goes on the same commit, with the same bytes:\n\n\
         1. gh api repos/{repository}/git/refs -f ref=refs/tags/{stable} -f sha={commit}\n\
         2. run `Official plugin candidate` on {stable} with publish=true, prerelease=false,\n\
         \x20  candidate-tag={candidate}, prepared-sha256sums={approved_sha256sums} and a preparation:\n"
    );
    for (run, artifact) in preparations {
        text.push_str(&format!(
            "     prepared-run-id={run} prepared-artifact-id={artifact}\n"
        ));
    }
    if preparations.is_empty() {
        text.push_str(
            "     (no unexpired preparation artifact of this commit exists: the stable release\n\
             \x20     cannot be published from the candidate's bytes any more; see the documentation)\n",
        );
    }
    text
}

// `promote-candidate`: checks that a stable release can be made from a
// published candidate and prints exactly how. It creates nothing.
pub fn run_promote(repository: &str, candidate_tag: &str) -> Result<()> {
    valid_repository(repository)?;
    let stable = stable_label(candidate_tag)?;
    if tag_exists(repository, &stable)? {
        return Err(format!("the tag {stable} already exists").into());
    }
    let release = release_json(repository, &format!("releases/tags/{candidate_tag}"))?;
    if release["tag_name"].as_str() != Some(candidate_tag)
        || release["draft"].as_bool() != Some(false)
        || release["prerelease"].as_bool() != Some(true)
        || release["immutable"].as_bool() != Some(true)
    {
        return Err(format!("{candidate_tag} is not a published, immutable prerelease").into());
    }
    let binding = crate::author::body_binding(&release)?;
    let resolved = catalogue::binding_in(
        binding.repository_id,
        candidate_tag,
        std::time::Instant::now() + std::time::Duration::from_secs(120),
    )?;
    if resolved["commit"].as_str() != Some(binding.commit.as_str()) {
        return Err(format!("{candidate_tag} does not point at {}", binding.commit).into());
    }
    require_candidate_commit(repository, &binding.commit, candidate_tag)?;
    let artifacts = release_json(
        repository,
        &format!(
            "actions/artifacts?name=plugin-release-candidate-{}&per_page=100",
            binding.commit
        ),
    )?;
    let preparations: Vec<(u64, u64)> = artifacts["artifacts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|artifact| {
            artifact["expired"] == false && artifact["workflow_run"]["head_sha"] == binding.commit
        })
        .filter_map(|artifact| {
            Some((
                artifact["workflow_run"]["id"].as_u64()?,
                artifact["id"].as_u64()?,
            ))
        })
        .collect();
    print!(
        "{}",
        promotion_text(
            repository,
            candidate_tag,
            &stable,
            &binding.commit,
            &binding.inventory_sha256,
            &preparations
        )
    );
    Ok(())
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

// Every release a catalogue points at, as a set of (id, tag).
fn catalogue_references(catalogue: &Value) -> Result<BTreeSet<(u64, String)>> {
    let mut referenced = BTreeSet::new();
    for plugin in catalogue["plugins"]
        .as_array()
        .ok_or("catalogue has no plugins")?
    {
        for version in plugin["versions"].as_array().into_iter().flatten() {
            if let Some(found) = reference(version)? {
                referenced.insert(found);
            }
        }
    }
    Ok(referenced)
}

// `verify-references <owner/name> <catalogue.json>`: without a previous
// catalogue to compare with (a stable release made from a candidate's bytes),
// the carried entries are not re-derived, but every release they point at must
// still be published and immutable.
pub fn run_verify_catalogue_references(repository: &str, catalogue: &Path) -> Result<()> {
    valid_repository(repository)?;
    let referenced = catalogue_references(&read_json(catalogue)?)?;
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
        // The code under test runs plain `git` in this repository, so the
        // identity and the signing choice must be the repository's own.
        for (key, value) in [
            ("user.name", "t"),
            ("user.email", "t@invalid"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
        ] {
            run_git(&work, &["config", key, value]);
        }
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
        open_release_pr_with(work, dry_run, |_, _, _, _| Ok(()))
    }

    fn open_release_pr_with(
        work: &Path,
        dry_run: bool,
        opener: impl Fn(&Path, &str, &str, &str) -> Result<()>,
    ) -> Result<()> {
        let arguments = PlanArguments {
            root: work,
            repository: "Among-Terminal/among-plugins",
            candidate: None,
            builds: &[],
        };
        release_pr_with(&arguments, dry_run, |_| Ok(fixed_plan()), opener)
    }

    #[test]
    fn a_pull_request_that_cannot_be_opened_keeps_the_pushed_branch() {
        let (_scratch, remote, work) = release_fixture("opener");
        let error = open_release_pr_with(&work, false, |_, _, _, _| Err("gh is down".into()))
            .unwrap_err()
            .to_string();
        // The message says what exists and how to go on.
        assert!(
            error.contains("release/v0.2.1 exists locally and on origin"),
            "{error}"
        );
        assert!(
            error.contains("gh pr create --base main --head release/v0.2.1"),
            "{error}"
        );
        assert!(error.contains("gh is down"), "{error}");
        // The local branch is kept, so a retry is not a non-fast-forward push,
        // and so is the remote one; the working tree is back on main and clean.
        assert!(branches(&work).contains("release/v0.2.1"));
        let remote_heads = git(&remote, &["branch", "--list"]).unwrap();
        assert!(remote_heads.contains("release/v0.2.1"), "{remote_heads}");
        assert_eq!(
            git(&work, &["rev-parse", "--abbrev-ref", "HEAD"])
                .unwrap()
                .trim(),
            "main"
        );
        assert!(git(&work, &["status", "--porcelain"]).unwrap().is_empty());
        // The opener received the branch, the title and the plan as its body.
        let seen = std::cell::RefCell::new(Vec::new());
        let (_s2, _r2, work2) = release_fixture("opener-args");
        open_release_pr_with(&work2, false, |_, branch, title, body| {
            seen.borrow_mut()
                .push((branch.to_owned(), title.to_owned(), body.to_owned()));
            Ok(())
        })
        .unwrap();
        let seen = seen.borrow();
        assert_eq!(seen[0].0, "release/v0.2.1");
        assert_eq!(seen[0].1, "chore(release): v0.2.1");
        assert!(
            seen[0].2.contains("`clock`: 0.2.0 -> 0.2.1"),
            "{}",
            seen[0].2
        );
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
        // The push itself was refused by the remote, not something earlier.
        assert!(error.contains("pre-receive hook declined"), "{error}");
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

    /// A repository with one plugin: a start commit tagged `v0.2.0`, a feature
    /// commit, and a release commit that replaces the manifest by
    /// `release_manifest` under the title `subject`. Build records for it need
    /// the commit, so it is returned too.
    fn release_repo(
        name: &str,
        subject: &str,
        release_manifest: &str,
    ) -> (TempDir, std::path::PathBuf, String) {
        let scratch = TempDir::new(name);
        let work = scratch.0.join("work");
        std::fs::create_dir_all(work.join("plugins/clock")).unwrap();
        run_git(&work, &["init", "-q"]);
        let file = work.join("plugins/clock/among-plugin.json");
        std::fs::write(
            &file,
            "{\n  \"id\": \"clock\",\n  \"version\": \"0.2.0\",\n  \"name\": \"Clock\"\n}\n",
        )
        .unwrap();
        run_git(&work, &["add", "-A"]);
        run_git(&work, &["commit", "-q", "-m", "feat: start"]);
        run_git(&work, &["tag", "v0.2.0"]);
        run_git(
            &work,
            &["commit", "-q", "--allow-empty", "-m", "feat(clock): more"],
        );
        std::fs::write(&file, release_manifest).unwrap();
        run_git(
            &work,
            &["commit", "-q", "-a", "--allow-empty", "-m", subject],
        );
        let head = git(&work, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned();
        (scratch, work, head)
    }

    const RELEASED: &str =
        "{\n  \"id\": \"clock\",\n  \"version\": \"0.3.0\",\n  \"name\": \"Clock\"\n}\n";

    fn release_records(head: &str, version: &str, binary: &str) -> Vec<Value> {
        let mut record = build_of(&[("clock", version, binary)]);
        record["revision"] = json!(head);
        vec![record]
    }

    #[test]
    fn the_release_commit_is_verified_as_a_whole() {
        let previous = catalogue_of(&[("clock", "0.2.0", "a")]);
        let (_scratch, work, head) = release_repo("verify", "chore(release): v0.3.0", RELEASED);
        let verified = verify_release_commit(
            &work,
            Some("v0.2.0"),
            &previous,
            &release_records(&head, "0.3.0", "a2"),
        )
        .unwrap();
        assert_eq!(verified, "v0.3.0");

        // The title names another label than the commits call for.
        let (_s, wrong, head) = release_repo("verify-label", "chore(release): v0.4.0", RELEASED);
        let error = verify_release_commit(
            &wrong,
            Some("v0.2.0"),
            &previous,
            &release_records(&head, "0.3.0", "a2"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("next label is v0.3.0"), "{error}");

        // Build records of another commit.
        let error = verify_release_commit(
            &work,
            Some("v0.2.0"),
            &previous,
            &release_records(&"0".repeat(40), "0.3.0", "a2"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("not for the checked out"), "{error}");

        // The packaged content changed but the build still shows the old version.
        let error = verify_release_commit(
            &work,
            Some("v0.2.0"),
            &previous,
            &release_records(&head_of(&work), "0.2.0", "a2"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("strict version check"), "{error}");

        // A commit that is not a release commit at all.
        run_git(
            &work,
            &["commit", "-q", "--allow-empty", "-m", "fix: later"],
        );
        assert!(verify_release_commit(
            &work,
            Some("v0.2.0"),
            &previous,
            &release_records(&head_of(&work), "0.3.0", "a2"),
        )
        .is_err());
    }

    fn head_of(work: &Path) -> String {
        git(work, &["rev-parse", "HEAD"]).unwrap().trim().to_owned()
    }

    #[test]
    fn a_release_commit_may_change_only_the_version_of_a_manifest() {
        let previous = catalogue_of(&[("clock", "0.2.0", "a")]);
        let check = |name: &str, manifest: &str| {
            let (_scratch, work, head) = release_repo(name, "chore(release): v0.3.0", manifest);
            verify_release_commit(
                &work,
                Some("v0.2.0"),
                &previous,
                &release_records(&head, "0.3.0", "a2"),
            )
        };
        // Another field changed along with the version.
        let renamed =
            "{\n  \"id\": \"clock\",\n  \"version\": \"0.3.0\",\n  \"name\": \"Clocks\"\n}\n";
        let error = check("only-name", renamed).unwrap_err().to_string();
        assert!(error.contains("more than the version"), "{error}");
        // A field added.
        let added = "{\n  \"id\": \"clock\",\n  \"version\": \"0.3.0\",\n  \"name\": \"Clock\",\n  \"x\": 1\n}\n";
        assert!(check("only-added", added).is_err());
        // The version did not change.
        let same = "{\"id\":\"clock\",\"version\":\"0.2.0\",\"name\":\"Clock\"}";
        let error = check("only-same", same).unwrap_err().to_string();
        assert!(error.contains("does not change the version"), "{error}");
        // Another layout and key order is the same manifest.
        let reformatted = "{\"name\":\"Clock\",\"version\":\"0.3.0\",\"id\":\"clock\"}";
        assert!(check("only-layout", reformatted).is_ok());
    }

    fn candidate_release(commit: &str, digest: &str, immutable: bool) -> Value {
        let binding = json!({"repository_id": 7, "workflow_ref": "w", "run_id": 1,
            "tag": "v0.2.0-rc.1", "commit": commit, "inventory_sha256": "e".repeat(64)});
        json!({"id": 5, "tag_name": "v0.2.0-rc.1", "draft": false, "prerelease": true,
               "immutable": immutable,
               "body": format!("{}{binding}", crate::author::BINDING_PREFIX),
               "assets": [
                   {"name": "among-plugins.json", "size": 10, "state": "uploaded", "digest": format!("sha256:{}", "1".repeat(64))},
                   {"name": "clock-0.2.0-linux-amd64.tar.gz", "size": 20, "state": "uploaded", "digest": format!("sha256:{digest}")}]})
    }

    fn stable_inventory(commit: &str, digest: &str) -> crate::author::Inventory {
        let asset = |name: &str, size: u64, sha256: String| crate::author::Asset {
            name: name.to_owned(),
            size,
            sha256,
        };
        crate::author::Inventory {
            repository_id: 7,
            tag: "v0.2.0".into(),
            revision: commit.to_owned(),
            inventory_sha256: "e".repeat(64),
            assets: vec![
                asset("among-plugins.json", 10, "1".repeat(64)),
                asset("clock-0.2.0-linux-amd64.tar.gz", 20, digest.to_owned()),
            ],
        }
    }

    #[test]
    fn a_stable_release_is_the_candidates_bytes_at_the_candidates_commit() {
        let (commit, digest) = ("c".repeat(40), "2".repeat(64));
        let candidate = candidate_release(&commit, &digest, true);
        // 0.2.0-rc.1 -> 0.2.0 with no change at all: accepted.
        assert!(check_candidate(
            &candidate,
            "v0.2.0-rc.1",
            &stable_inventory(&commit, &digest)
        )
        .is_ok());
        // One package built again (another digest): refused, naming the asset.
        let error = check_candidate(
            &candidate,
            "v0.2.0-rc.1",
            &stable_inventory(&commit, &"3".repeat(64)),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("clock-0.2.0-linux-amd64.tar.gz"), "{error}");
        // Another commit than the candidate was built from.
        assert!(check_candidate(
            &candidate,
            "v0.2.0-rc.1",
            &stable_inventory(&"d".repeat(40), &digest)
        )
        .is_err());
        // An asset the stable release lacks or adds.
        let mut fewer = stable_inventory(&commit, &digest);
        fewer.assets.pop();
        assert!(check_candidate(&candidate, "v0.2.0-rc.1", &fewer).is_err());
        let mut more = stable_inventory(&commit, &digest);
        more.assets.push(crate::author::Asset {
            name: "SHA256SUMS".into(),
            size: 1,
            sha256: "4".repeat(64),
        });
        assert!(check_candidate(&candidate, "v0.2.0-rc.1", &more).is_err());
        // Not a published immutable prerelease.
        assert!(check_candidate(
            &candidate_release(&commit, &digest, false),
            "v0.2.0-rc.1",
            &stable_inventory(&commit, &digest)
        )
        .is_err());
        let mut draft = candidate.clone();
        draft["draft"] = json!(true);
        assert!(
            check_candidate(&draft, "v0.2.0-rc.1", &stable_inventory(&commit, &digest)).is_err()
        );
        let mut stable_release = candidate.clone();
        stable_release["prerelease"] = json!(false);
        assert!(check_candidate(
            &stable_release,
            "v0.2.0-rc.1",
            &stable_inventory(&commit, &digest)
        )
        .is_err());
        assert!(check_candidate(
            &candidate,
            "v0.2.0-rc.2",
            &stable_inventory(&commit, &digest)
        )
        .is_err());
    }

    #[test]
    fn a_stable_label_with_release_candidates_is_made_from_one_of_them() {
        let candidates = ["v0.2.0-rc.1".to_owned(), "v0.2.0-rc.2".to_owned()];
        // Planning, preparing or publishing a stable release anew is refused,
        // and the message names the way out.
        let error = refuse_stable_after_candidates("v0.2.0", &candidates)
            .unwrap_err()
            .to_string();
        assert!(error.contains("promote-candidate"), "{error}");
        assert!(error.contains("v0.2.0-rc.1, v0.2.0-rc.2"), "{error}");
        // No candidates, or a candidate label itself: nothing to refuse.
        assert!(refuse_stable_after_candidates("v0.2.0", &[]).is_ok());
        assert!(refuse_stable_after_candidates("v0.2.0-rc.3", &candidates).is_ok());
        // Publishing: a stable tag with candidates needs a candidate of that tag.
        assert!(check_stable_publication("v0.2.0", &candidates, None).is_err());
        assert!(check_stable_publication("v0.2.0", &candidates, Some("v0.2.0-rc.2")).is_ok());
        assert!(check_stable_publication("v0.2.0", &candidates, Some("v0.2.0-rc.9")).is_err());
        assert!(check_stable_publication("v0.2.0", &[], None).is_ok());
        assert!(check_stable_publication("v0.2.0", &[], Some("v0.2.0-rc.1")).is_err());
        assert!(check_stable_publication("v0.2.0-rc.4", &candidates, None).is_ok());
    }

    #[test]
    fn the_commit_of_a_candidate_is_its_verified_release_commit() {
        let release = CommitRule::ReleaseCommit;
        let passed = checks_run("Locked Rust source checks", "push", "main", "success");
        let title = "chore(release): v0.2.0-rc.1";
        assert!(check_candidate_commit(title, &passed, "v0.2.0-rc.1", release).is_ok());
        // A squash commit carries the pull request number and a body.
        assert!(check_candidate_commit(
            "chore(release): v0.2.0-rc.1 (#18)\n\nbody",
            &passed,
            "v0.2.0-rc.1",
            release
        )
        .is_ok());
        // Another title, another candidate's title, an ordinary commit.
        for other in [
            "feat: x",
            "chore(release): v0.2.0-rc.2",
            "chore(release): v0.2.0",
        ] {
            assert!(
                check_candidate_commit(other, &passed, "v0.2.0-rc.1", release).is_err(),
                "{other}"
            );
        }
        for bad in unverified_runs() {
            assert!(check_candidate_commit(title, &bad, "v0.2.0-rc.1", release).is_err());
        }
    }

    fn checks_run(name: &str, event: &str, branch: &str, conclusion: &str) -> Value {
        json!({"workflow_runs": [{"name": name, "event": event,
                                  "head_branch": branch, "conclusion": conclusion}]})
    }

    // The checks must have passed, on a push, to the default branch.
    fn unverified_runs() -> Vec<Value> {
        vec![
            checks_run("Locked Rust source checks", "push", "main", "failure"),
            checks_run(
                "Locked Rust source checks",
                "pull_request",
                "main",
                "success",
            ),
            checks_run("Locked Rust source checks", "push", "topic", "success"),
            checks_run("Other", "push", "main", "success"),
            json!({"workflow_runs": []}),
        ]
    }

    #[test]
    fn the_first_release_may_be_any_verified_commit_of_the_default_branch() {
        let passed = checks_run("Locked Rust source checks", "push", "main", "success");
        let first = CommitRule::FirstRelease { on_main: true };
        // The first release candidate, promoted to stable: no release commit, an
        // ordinary commit that the checks passed on.
        let own = ["v0.2.0-rc.1".to_owned()];
        assert!(is_first_release("v0.2.0", None, &own, "v0.2.0-rc.1"));
        assert!(
            check_candidate_commit("feat: an ordinary commit", &passed, "v0.2.0-rc.1", first)
                .is_ok()
        );
        // A non-first release still needs its release commit: the ordinary
        // commit is refused under the rule that every other release gets.
        assert!(!is_first_release("v0.3.0", None, &[], "v0.3.0-rc.1"));
        // Not the first release: another release with a catalogue exists (the
        // first candidate, for a second one), or another candidate tag remains
        // after its release was deleted, so deleting a release cannot reopen
        // the exemption.
        assert!(
            !is_first_release("v0.2.0", Some("v0.2.0-rc.1"), &own, "v0.2.0-rc.2"),
            "a second candidate of v0.2.0"
        );
        let both = ["v0.2.0-rc.1".to_owned(), "v0.2.0-rc.2".to_owned()];
        assert!(!is_first_release("v0.2.0", None, &both, "v0.2.0-rc.2"));
        assert!(is_first_release("v0.2.0", None, &[], "v0.2.0-rc.1"));
        assert!(check_candidate_commit(
            "feat: an ordinary commit",
            &passed,
            "v0.3.0-rc.1",
            CommitRule::ReleaseCommit
        )
        .is_err());
        // A commit that is not an ancestor of the default branch is refused.
        let off = CommitRule::FirstRelease { on_main: false };
        let error = check_candidate_commit("feat: x", &passed, "v0.2.0-rc.1", off)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not an ancestor"), "{error}");
        // So is one without a successful push run of the checks on main.
        for bad in unverified_runs() {
            assert!(check_candidate_commit("feat: x", &bad, "v0.2.0-rc.1", first).is_err());
        }
    }

    #[test]
    fn the_rule_follows_whether_it_is_the_first_release_and_whether_the_commit_is_on_main() {
        assert_eq!(
            commit_rule(true, true),
            CommitRule::FirstRelease { on_main: true }
        );
        assert_eq!(
            commit_rule(true, false),
            CommitRule::FirstRelease { on_main: false }
        );
        // Not the first release: the release commit, whatever the ancestry says.
        assert_eq!(commit_rule(false, true), CommitRule::ReleaseCommit);
        assert_eq!(commit_rule(false, false), CommitRule::ReleaseCommit);
    }

    #[test]
    fn a_commit_is_on_main_when_it_is_the_merge_base_of_the_comparison() {
        let commit = "a".repeat(40);
        let comparison = |sha: &str| json!({"status": "ahead", "merge_base_commit": {"sha": sha}});
        assert!(merge_base_is(&comparison(&commit), &commit));
        // The merge base is another commit: the commit is off main (diverged).
        assert!(!merge_base_is(&comparison(&"b".repeat(40)), &commit));
        // No merge base, or a malformed answer, is not an answer of "yes".
        assert!(!merge_base_is(&json!({"status": "ahead"}), &commit));
        assert!(!merge_base_is(
            &json!({"merge_base_commit": {"sha": 7}}),
            &commit
        ));
        assert!(!merge_base_is(&json!(null), &commit));
    }

    #[test]
    fn the_first_release_text_names_the_tag_and_the_commit() {
        let text = first_release_text("o/r", "v0.2.0-rc.1", &"a".repeat(40));
        assert!(
            text.contains("-f ref=refs/tags/v0.2.0-rc.1 -f sha=aaaa"),
            "{text}"
        );
        assert!(text.contains("promote-candidate o/r v0.2.0-rc.1"));
        assert!(text.contains("owner's approval"));
    }

    #[test]
    fn a_catalogue_lists_the_releases_it_points_at() {
        let mut catalogue = catalogue_of(&[("clock", "0.2.0", "a"), ("pane-pwd", "0.2.0", "b")]);
        assert!(catalogue_references(&catalogue).unwrap().is_empty());
        catalogue["plugins"][0]["versions"][0]["release"] = json!({"id": 7, "tag": "v0.1.0"});
        catalogue["plugins"][1]["versions"][0]["release"] = json!({"id": 7, "tag": "v0.1.0"});
        assert_eq!(
            catalogue_references(&catalogue).unwrap(),
            BTreeSet::from([(7, "v0.1.0".to_owned())])
        );
        catalogue["plugins"][1]["versions"][0]["release"] = json!({"id": "7"});
        assert!(catalogue_references(&catalogue).is_err());
    }

    #[test]
    fn the_stable_label_of_a_candidate_and_the_instructions_for_it() {
        assert_eq!(stable_label("v0.2.0-rc.1").unwrap(), "v0.2.0");
        assert_eq!(stable_label("v1.10.3-rc.12").unwrap(), "v1.10.3");
        for bad in [
            "v0.2.0",
            "v0.2.0-rc.0",
            "v0.2.0-rc.01",
            "v0.2.0-rc.",
            "v0.2.0-beta.1",
            "0.2.0-rc.1",
            "v0.2-rc.1",
            "v0.2.0-1-rc.1",
        ] {
            assert!(stable_label(bad).is_err(), "{bad}");
        }
        let text = promotion_text(
            "Among-Terminal/among-plugins",
            "v0.2.0-rc.1",
            "v0.2.0",
            &"c".repeat(40),
            &"e".repeat(64),
            &[(9, 99)],
        );
        assert!(
            text.contains("-f ref=refs/tags/v0.2.0 -f sha=cccc"),
            "{text}"
        );
        assert!(text.contains("candidate-tag=v0.2.0-rc.1"));
        assert!(text.contains("prepared-run-id=9 prepared-artifact-id=99"));
        let none = promotion_text(
            "o/r",
            "v0.2.0-rc.1",
            "v0.2.0",
            &"c".repeat(40),
            &"e".repeat(64),
            &[],
        );
        assert!(none.contains("no unexpired preparation artifact"));
    }

    #[test]
    fn a_stable_release_after_a_candidate_with_no_changes_is_planned_from_the_stable_baseline() {
        // Only the candidate 0.2.0-rc.1 is published; the stable release 0.2.0
        // has the same plugin versions and the same content.
        let candidate_catalogue = catalogue_of(&[("clock", "0.2.0", "a")]);
        let current = build_of(&[("clock", "0.2.0", "a")]);
        // Compared with the candidate nothing changed, so nothing could be released.
        let error = plan(
            &from_catalogue(&candidate_catalogue).unwrap(),
            &from_builds(std::slice::from_ref(&current)).unwrap(),
            &[],
            Some("v0.2.0-rc.1"),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("nothing to release"), "{error}");
        // The candidate is not the baseline of a stable release (the release
        // selection tests in `changed` cover that), so there is no previous
        // release and the plugins are new: the first label, v0.2.0.
        let stable = plan(
            &BTreeMap::new(),
            &from_builds(&[current]).unwrap(),
            &[],
            None,
            None,
        )
        .unwrap();
        assert_eq!((stable.label.as_str(), stable.new.len()), ("v0.2.0", 1));
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
