mod archive;
mod author;
mod catalogue;
mod changed;
mod package;
mod process;
mod release;
mod source;
mod strict;
mod toolchain;

use std::error::Error;
use std::path::Path;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const USAGE: &str = "Usage: among-tools plugin-packages <source-root> <native-binary-directory> <vendor-directory> <new-output>
       among-tools check-plugin-packages <source-root> <package-directory>
       among-tools plugin-catalogue <source-root> <amd64-package-directory> <arm64-package-directory> <release-id> <new-output>
       among-tools author-packages <source-root> <native-binary-directory> <vendor-directory> <repository-id> <new-output>
       among-tools author-catalogue <amd64-build> <arm64-build> <repository-id> <tag> <revision> <new-output>
       among-tools release-binding <inventory.json> <workflow-ref> <run-id> <new-output>
       among-tools prepared-run <run.json> <artifact.json> <repository-id> <source> <run-id> <artifact-id> <workflow-ref>
       among-tools prepared-inventory <prepared-directory> <repository-id> <revision> <tag> <approved-sha256sums-digest> <new-inventory>
       among-tools verify-release stable|prerelease resume|draft|published <release.json> <inventory.json> <binding> [<latest.json>]
       among-tools resolve-tag <repository-id> <tag>
       among-tools previous-catalogue <owner/name> <new-output>
       among-tools changed-plugins [--pending] <previous-catalogue.json> <build.json>...
       among-tools plugin-release-plan <repo-root> <previous-catalogue.json> <previous-tag>|- [--rc N] [--apply] <build.json>...
       among-tools plugin-release-pr <repo-root> <previous-catalogue.json> <previous-tag>|- [--rc N] [--dry-run] <build.json>...
       among-tools snapshot-label <commit-subject> [<changed-file>...]
       among-tools carry-over <owner/name> <previous-catalogue.json> <previous-tag> <fresh-catalogue.json> <new-output> <build.json>...";

// `[--rc N] [--apply] [--dry-run] <build.json>...` after the three fixed
// arguments of the release planning commands.
fn release_options<'a>(
    arguments: &'a [String],
    paths: &'a mut Vec<&'a Path>,
) -> Result<(release::Options<'a>, bool)> {
    let [root, previous, tag, rest @ ..] = arguments else {
        return Err(USAGE.into());
    };
    let (mut candidate, mut apply, mut dry_run) = (None, false, false);
    let mut rest = rest.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--rc" => candidate = Some(rest.next().ok_or(USAGE)?.parse()?),
            "--apply" => apply = true,
            "--dry-run" => dry_run = true,
            path => paths.push(Path::new(path)),
        }
    }
    if paths.is_empty() {
        return Err(USAGE.into());
    }
    Ok((
        release::Options {
            root: Path::new(root),
            previous: Path::new(previous),
            previous_tag: (tag != "-").then_some(tag.as_str()),
            candidate,
            apply,
            builds: paths,
        },
        dry_run,
    ))
}

fn channel(name: &str) -> Result<author::Channel> {
    match name {
        "stable" => Ok(author::Channel::Stable),
        "prerelease" => Ok(author::Channel::Prerelease),
        _ => Err(USAGE.into()),
    }
}

fn stage(name: &str) -> Result<author::Stage> {
    match name {
        "resume" => Ok(author::Stage::Resume),
        "draft" => Ok(author::Stage::Draft),
        "published" => Ok(author::Stage::Published),
        _ => Err(USAGE.into()),
    }
}

fn run() -> Result<()> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    match arguments.as_slice() {
        [command, root, binaries, vendor, output] if command == "plugin-packages" => {
            package::build(
                Path::new(root),
                Path::new(binaries),
                Path::new(vendor),
                Path::new(output),
            )
        }
        [command, root, directory] if command == "check-plugin-packages" => {
            package::check(Path::new(root), Path::new(directory))
        }
        [command, root, amd64, arm64, release, output] if command == "plugin-catalogue" => {
            catalogue::generate(
                Path::new(root),
                Path::new(amd64),
                Path::new(arm64),
                release.parse()?,
                Path::new(output),
            )
        }
        [command, root, binaries, vendor, repository, output] if command == "author-packages" => {
            author::packages(
                Path::new(root),
                Path::new(binaries),
                Path::new(vendor),
                repository.parse()?,
                Path::new(output),
            )
        }
        [command, amd64, arm64, repository, tag, revision, output]
            if command == "author-catalogue" =>
        {
            author::catalogue(
                Path::new(amd64),
                Path::new(arm64),
                repository.parse()?,
                tag,
                revision,
                Path::new(output),
            )
        }
        [command, inventory, workflow, run, output] if command == "release-binding" => {
            author::binding(
                Path::new(inventory),
                workflow,
                run.parse()?,
                Path::new(output),
            )
        }
        [command, run, artifact, repository, source, run_id, artifact_id, workflow]
            if command == "prepared-run" =>
        {
            author::prepared_run(
                Path::new(run),
                Path::new(artifact),
                repository.parse()?,
                source,
                run_id.parse()?,
                artifact_id.parse()?,
                workflow,
            )
        }
        [command, prepared, repository, revision, tag, digest, output]
            if command == "prepared-inventory" =>
        {
            author::prepared_inventory(
                Path::new(prepared),
                repository.parse()?,
                revision,
                tag,
                digest,
                Path::new(output),
            )
        }
        [command, kind, name, release, inventory, binding, rest @ ..]
            if command == "verify-release" && rest.len() <= 1 =>
        {
            let missing = author::verify_release(
                channel(kind)?,
                stage(name)?,
                Path::new(release),
                Path::new(inventory),
                Path::new(binding),
                rest.first().map(Path::new),
            )?;
            for name in missing {
                println!("{name}");
            }
            Ok(())
        }
        [command, repository, tag] if command == "resolve-tag" => {
            let resolved = catalogue::binding_in(
                repository.parse()?,
                tag,
                Instant::now() + Duration::from_secs(120),
            )?;
            println!(
                "{}",
                resolved["commit"]
                    .as_str()
                    .ok_or("tag resolved to no commit")?
            );
            Ok(())
        }
        [command, repository, output] if command == "previous-catalogue" => {
            changed::previous_catalogue(repository, Path::new(output))
        }
        [command, flag, previous, builds @ ..]
            if command == "changed-plugins" && flag == "--pending" && !builds.is_empty() =>
        {
            let builds: Vec<_> = builds.iter().map(Path::new).collect();
            changed::run(Path::new(previous), &builds, true)
        }
        [command, previous, builds @ ..] if command == "changed-plugins" && !builds.is_empty() => {
            let builds: Vec<_> = builds.iter().map(Path::new).collect();
            changed::run(Path::new(previous), &builds, false)
        }
        [command, rest @ ..] if command == "plugin-release-plan" => {
            let mut paths = Vec::new();
            let (options, _) = release_options(rest, &mut paths)?;
            let plan = release::run_plan(&options)?;
            println!("{}", serde_json::to_string_pretty(&plan.to_json())?);
            Ok(())
        }
        [command, rest @ ..] if command == "plugin-release-pr" => {
            let mut paths = Vec::new();
            let (options, dry_run) = release_options(rest, &mut paths)?;
            release::run_release_pr(&options, dry_run)
        }
        [command, subject, files @ ..] if command == "snapshot-label" => {
            let files: Vec<_> = files.iter().map(String::as_str).collect();
            if let Some(label) = release::snapshot_label(subject, &files)? {
                println!("{label}");
            }
            Ok(())
        }
        [command, repository, previous, tag, fresh, output, builds @ ..]
            if command == "carry-over" && !builds.is_empty() =>
        {
            let builds: Vec<_> = builds.iter().map(Path::new).collect();
            release::run_carry_over(
                repository,
                Path::new(previous),
                tag,
                Path::new(fresh),
                Path::new(output),
                &builds,
            )
        }
        _ => Err(USAGE.into()),
    }
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
