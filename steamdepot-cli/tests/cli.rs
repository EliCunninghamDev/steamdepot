//! Black-box CLI tests. These assert fail-fast behavior, so they never touch
//! the network or require Steam credentials.

use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_steamdepot-cli"))
}

fn run(args: &[&str]) -> (bool, String, String) {
    let output = cli().args(args).output().expect("run steamdepot-cli");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn help_lists_the_main_options() {
    let (ok, stdout, _) = run(&["--help"]);
    assert!(ok);
    for flag in ["--download", "--app-id", "--token", "--logmode"] {
        assert!(stdout.contains(flag), "help is missing {flag}:\n{stdout}");
    }
}

#[test]
fn username_requires_token_before_connecting() {
    let (ok, _, stderr) = run(&["--username", "alice"]);
    assert!(!ok);
    assert!(
        stderr.contains("--username requires --token"),
        "unexpected stderr:\n{stderr}"
    );
}

#[test]
fn token_requires_username_before_connecting() {
    let (ok, _, stderr) = run(&["--token", "secret"]);
    assert!(!ok);
    assert!(
        stderr.contains("--token requires --username"),
        "unexpected stderr:\n{stderr}"
    );
}

#[test]
fn depot_options_must_be_paired() {
    let (ok, _, stderr) = run(&["--depot-id", "123"]);
    assert!(!ok);
    assert!(
        stderr.contains("--depot-id and --depot-app-id must be used together"),
        "unexpected stderr:\n{stderr}"
    );
}

#[test]
fn install_dir_requires_manifests() {
    let (ok, _, stderr) = run(&["--install-dir", "/tmp/steamdepot-test"]);
    assert!(!ok);
    assert!(
        stderr.contains("--install-dir requires --fetch-manifests"),
        "unexpected stderr:\n{stderr}"
    );
}
