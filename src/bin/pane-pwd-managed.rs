use among_example_plugins::{managed, pwd};

fn main() -> std::process::ExitCode {
    let profile = managed::Profile {
        identity: "github:1431055733/pane-pwd",
        required: &["status", "context.terminals", "context.cwd"],
        optional: &[],
    };
    managed::run(&profile, pwd::managed_status)
}
