mod archive;
mod catalogue;
mod package;
mod process;
mod source;

use std::error::Error;
use std::path::Path;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn run() -> Result<()> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    match arguments.as_slice() {
        [command, root, binaries, vendor, output] if command == "plugin-packages" => {
            package::build(Path::new(root), Path::new(binaries), Path::new(vendor), Path::new(output))
        }
        [command, root, directory] if command == "check-plugin-packages" => {
            package::check(Path::new(root), Path::new(directory))
        }
        [command, root, amd64, arm64, release, output] if command == "plugin-catalogue" => {
            catalogue::generate(
                Path::new(root), Path::new(amd64), Path::new(arm64), release.parse()?, Path::new(output),
            )
        }
        _ => Err("Usage: among-tools plugin-packages <source-root> <native-binary-directory> <vendor-directory> <new-output>\n       among-tools check-plugin-packages <source-root> <package-directory>\n       among-tools plugin-catalogue <source-root> <amd64-package-directory> <arm64-package-directory> <release-id> <new-output>".into()),
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
