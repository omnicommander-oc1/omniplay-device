use std::process::Command;

fn main() {
    // Get git version information
    let git_version = Command::new("git")
        .args(&["describe", "--tags", "--always", "--dirty"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    // Get git commit hash
    let git_hash = Command::new("git")
        .args(&["rev-parse", "--short", "HEAD"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    // Get build timestamp
    let build_time = chrono::Utc::now().to_rfc3339();

    // Make these available to the application
    println!("cargo:rustc-env=GIT_VERSION={}", git_version);
    println!("cargo:rustc-env=GIT_HASH={}", git_hash);
    println!("cargo:rustc-env=BUILD_TIME={}", build_time);

    // Re-run if git changes
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
} 