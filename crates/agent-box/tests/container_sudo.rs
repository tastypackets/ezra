//! Black-box tests of the `AGENT_SUDO` policy. Build the image first, then run: `cargo test -- --ignored`

mod common;

use common::{DockerResource, docker, process_status_field, run_in_image, stderr_of, stdout_of};

fn sudo_succeeds(docker_options: &[&str]) -> bool {
    run_in_image(docker_options, &["sudo", "--non-interactive", "true"])
        .status
        .success()
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn sudo_is_off_by_default() {
    assert!(!sudo_succeeds(&[]));
    assert_eq!(process_status_field(&[], "NoNewPrivs"), "1");
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn empty_value_means_off() {
    let options = ["--env", "AGENT_SUDO="];
    assert!(!sudo_succeeds(&options));
    assert_eq!(process_status_field(&options, "NoNewPrivs"), "1");
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn full_grants_passwordless_sudo() {
    let options = ["--env", "AGENT_SUDO=full"];
    let uid_under_sudo = stdout_of(&run_in_image(
        &options,
        &["sudo", "--non-interactive", "id", "--user"],
    ));
    assert_eq!(uid_under_sudo, "0");
    assert_eq!(process_status_field(&options, "NoNewPrivs"), "0");
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn off_blocks_sudo_in_exec_sessions() {
    let container = DockerResource::start_container(
        "sudo-off",
        &["--env", "AGENT_SUDO=off"],
        &["sleep", "infinity"],
    );
    let exec_sudo = docker(&[
        "exec",
        "--user",
        "dev",
        &container.name,
        "sudo",
        "--non-interactive",
        "true",
    ]);
    assert!(!exec_sudo.status.success());
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn full_allows_sudo_in_exec_sessions() {
    let container = DockerResource::start_container(
        "sudo-full",
        &["--env", "AGENT_SUDO=full"],
        &["sleep", "infinity"],
    );
    let exec_sudo = docker(&[
        "exec",
        "--user",
        "dev",
        &container.name,
        "sudo",
        "--non-interactive",
        "id",
        "--user",
    ]);
    assert_eq!(stdout_of(&exec_sudo), "0");
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn invalid_value_refuses_to_start() {
    let output = run_in_image(&["--env", "AGENT_SUDO=yes"], &["true"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr_of(&output).contains("AGENT_SUDO must be"),
        "{}",
        stderr_of(&output)
    );
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn full_on_read_only_root_filesystem_starts_without_sudo() {
    let options = ["--read-only", "--env", "AGENT_SUDO=full"];
    let output = run_in_image(&options, &["true"]);
    assert!(output.status.success());
    assert!(
        stderr_of(&output).contains("sudo is unavailable"),
        "{}",
        stderr_of(&output)
    );
    assert!(!sudo_succeeds(&options));
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn non_root_start_is_off_by_default() {
    assert_eq!(
        process_status_field(&["--user", "1000:1000"], "NoNewPrivs"),
        "1"
    );
}
