// Jackson Coxson
// Runs the Python shims until it's written in Rust

use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
};

use log::{info, warn};

pub fn run(path: &str, count: u32) {
    info!("Running {}...", path);
    for _ in 0..count {
        let path = path.to_string();
        std::thread::spawn(move || {
            loop {
                // Run the Python shim
                let mut child = Command::new("python3")
                    .args(["-u", &path])
                    .stdout(Stdio::piped())
                    .spawn()
                    .unwrap();
                let stdout = child.stdout.take().unwrap();

                // Stream output.
                let lines = BufReader::new(stdout).lines();
                for line in lines {
                    println!("{}", line.unwrap());
                }

                warn!("Python shim stopped!");
            }
        });
    }
}
