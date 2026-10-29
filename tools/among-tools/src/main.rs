mod archive;
mod author;
mod catalogue;
mod package;
mod process;
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
       among-tools verify-release resume|draft|published <release.json> <inventory.json> <binding> [<latest.json>]
       among-tools resolve-tag <repository-id> <tag>";

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
        [command, name, release, inventory, binding, rest @ ..]
            if command == "verify-release" && rest.len() <= 1 =>
        {
            let missing = author::verify_release(
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
