use std::io::Write;
use std::thread;
use std::time::Duration;

fn main() {
    // Emit a reasoning delta after a short delay, then a text delta after
    // another delay. This exercises true per-line streaming (RS0).
    thread::sleep(Duration::from_millis(200));
    println!(r#"{{"type":"reasoning","delta":"thinking..."}}"#);
    let _ = std::io::stdout().flush();

    thread::sleep(Duration::from_millis(200));
    println!(r#"{{"type":"text","delta":"answer"}}"#);
    let _ = std::io::stdout().flush();

    thread::sleep(Duration::from_millis(50));
}
