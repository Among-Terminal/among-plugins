fn main() -> std::process::ExitCode {
    let start = std::time::Instant::now();
    let mut sampler =
        among_example_plugins::system::Sampler::new(among_example_plugins::system::Linux);
    among_example_plugins::protocol::run(move |scope, _| sampler.status(scope, start.elapsed()))
}
