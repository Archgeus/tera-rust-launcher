// autoupdater.exe
// Usage: autoupdater.exe <new_exe_path> <launcher_pid> <target_exe_path> [new_version]
//
// 1. Waits for the launcher process (identified by <launcher_pid>) to exit.
// 2. Copies <new_exe_path> over <target_exe_path> (with retries for locked files).
// 3. If <new_version> is provided, writes it to launcher_version.ini next to the target exe.
// 4. Launches the updated <target_exe_path>.

#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 4 {
        eprintln!("Usage: autoupdater.exe <new_exe_path> <launcher_pid> <target_exe_path> [new_version]");
        std::process::exit(1);
    }

    let new_exe_path   = args[1].clone();
    let launcher_pid   = args[2].parse::<u32>().unwrap_or(0);
    let target_exe     = args[3].clone();
    let new_version    = args.get(4).cloned();

    // 1. Wait for the launcher to exit
    if launcher_pid > 0 {
        wait_for_pid(launcher_pid);
    } else {
        thread::sleep(Duration::from_secs(3));
    }

    // Give OS a moment to fully release the file handle
    thread::sleep(Duration::from_millis(500));

    // 2. Copy new exe over the target (retry up to 10 times, 1 s apart)
    let copied = retry_copy(&new_exe_path, &target_exe, 10);
    if !copied {
        eprintln!(
            "Failed to copy '{}' → '{}' after multiple attempts.",
            new_exe_path, target_exe
        );
        std::process::exit(1);
    }

    // 3. Update launcher_version.ini if a new version was provided
    if let Some(version) = new_version {
        let target_dir = Path::new(&target_exe)
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let ver_ini = target_dir.join("launcher_version.ini");
        // Write a minimal INI: [LAUNCHER]\nversion=X.X.X.X
        let content = format!("[LAUNCHER]\nversion={}\n", version);
        if let Err(e) = fs::write(&ver_ini, content) {
            eprintln!("Warning: could not write launcher_version.ini: {}", e);
        }
    }

    // 4. Launch the updated executable
    match Command::new(&target_exe).spawn() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Failed to launch updated launcher '{}': {}", target_exe, e);
            std::process::exit(1);
        }
    }
}

/// Attempts to copy `src` → `dst` up to `attempts` times, sleeping 1 s between tries.
/// Returns `true` on success.
fn retry_copy(src: &str, dst: &str, attempts: u32) -> bool {
    for i in 0..attempts {
        match fs::copy(src, dst) {
            Ok(_) => return true,
            Err(e) => {
                eprintln!("Copy attempt {}/{} failed: {}", i + 1, attempts, e);
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
    false
}

/// Poll every second (up to 30 s) until the process with `pid` is no longer running.
fn wait_for_pid(pid: u32) {
    for _ in 0..30 {
        let running = is_pid_running(pid);
        if !running {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(windows)]
fn is_pid_running(pid: u32) -> bool {
    let pid_str = pid.to_string();
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&*pid_str))
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn is_pid_running(pid: u32) -> bool {
    // On Linux/macOS: /proc/<pid> exists while the process is alive;
    // fall back to sending signal 0 if /proc is unavailable.
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{}", pid)).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        // kill -0 returns Ok if process exists
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
}
