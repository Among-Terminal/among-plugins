use among_example_plugins::{managed, system};

fn main() -> std::process::ExitCode {
    let start = std::time::Instant::now();
    let mut sampler = system::Sampler::new(system::Linux);
    let profile = managed::Profile {
        identity: "github:1431055733/system-status",
        required: &["status", "system.metrics"],
        optional: &[],
    };
    managed::run(&profile, move |scope, _| {
        sampler.managed_status(scope, start.elapsed())
    })
}
