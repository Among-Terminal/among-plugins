use among_example_plugins::{clock, managed};

fn main() -> std::process::ExitCode {
    let profile = managed::Profile {
        identity: "github:1431055733/clock",
        required: &["status"],
        optional: &[],
    };
    managed::run(&profile, clock::managed_status)
}
