// Jackson Coxson
// Runs the Python shims until it's written in Rust

use log::{info, warn};

pub fn run(path: &str, count: u32) {
    info!("Running {}...", path);
    for _ in 0..count {
        let path = path.to_string();
        std::thread::spawn(move || {
            loop {
                // Run the Python shim
                let output = std::process::Command::new("python3")
                    .arg(&path)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .output()
                    .expect("Failed to run Python shim");

                warn!("Python shim stopped: {:?}", output);
            }
        });
    }
}
