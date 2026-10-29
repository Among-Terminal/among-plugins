// Packaging starts the entrypoint once with the manifest's required features
// and expects the admission back. This fixture has no SDK dependency, so it
// echoes the admission line as a real plugin would accept it, then stops.
use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { return };
        if line.contains("\"type\":\"managed_hello\"") {
            let _ = writeln!(stdout, "{line}");
            let _ = stdout.flush();
        } else if line.contains("\"type\":\"shutdown\"") {
            return;
        }
    }
}
