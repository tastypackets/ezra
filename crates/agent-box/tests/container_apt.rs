//! Black-box tests of `APT_PACKAGES`. Build the image first, then run: `cargo test -- --ignored`

mod common;

use std::process::Output;

use common::{DockerResource, docker, run_in_image, stderr_of, stdout_of};

fn last_line_of_stdout(output: &Output) -> String {
    stdout_of(output)
        .lines()
        .last()
        .unwrap_or_default()
        .to_owned()
}

#[test]
#[ignore = "needs Docker, network access and a built agent-box image"]
fn listed_package_is_installed_for_the_agent() {
    let output = run_in_image(&["--env", "APT_PACKAGES=hello"], &["hello"]);
    assert_eq!(last_line_of_stdout(&output), "Hello, world!");
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn installed_or_provided_packages_need_no_apt_run() {
    let output = run_in_image(
        &[
            "--network",
            "none",
            "--env",
            "APT_PACKAGES=bash,awk coreutils",
        ],
        &["true"],
    );
    stdout_of(&output);
    assert_eq!(stderr_of(&output), "");
}

#[test]
#[ignore = "needs Docker, network access and a built agent-box image"]
fn unknown_package_does_not_block_the_others() {
    let output = run_in_image(
        &[
            "--env",
            "APT_PACKAGES=agent-box-no-such-package hello/resolute",
        ],
        &["hello"],
    );
    assert_eq!(last_line_of_stdout(&output), "Hello, world!");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("could not install agent-box-no-such-package from APT_PACKAGES"),
        "{stderr}"
    );
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn offline_start_warns_and_continues() {
    let output = run_in_image(
        &["--network", "none", "--env", "APT_PACKAGES=hello"],
        &["true"],
    );
    stdout_of(&output);
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("could not install hello from APT_PACKAGES"),
        "{stderr}"
    );
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn read_only_root_filesystem_warns_and_continues() {
    let output = run_in_image(&["--read-only", "--env", "APT_PACKAGES=hello"], &["true"]);
    stdout_of(&output);
    let stderr = stderr_of(&output);
    assert!(stderr.contains("not installed: hello"), "{stderr}");
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn non_root_start_ignores_the_list() {
    let output = run_in_image(
        &["--user", "1000:1000", "--env", "APT_PACKAGES=hello"],
        &["true"],
    );
    stdout_of(&output);
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("APT_PACKAGES is ignored when the container starts as uid 1000"),
        "{stderr}"
    );
}

#[test]
#[ignore = "needs Docker, network access and a built agent-box image"]
fn restart_keeps_packages_without_running_apt_again() {
    let container = DockerResource::start_container(
        "apt-restart",
        &["--env", "APT_PACKAGES=hello"],
        &["hello"],
    );
    docker(&["wait", &container.name]);
    let first_start = stderr_of(&docker(&["logs", &container.name]));
    assert!(
        first_start.contains("installing hello from APT_PACKAGES"),
        "{first_start}"
    );

    let restart = docker(&["start", "--attach", &container.name]);
    assert_eq!(stdout_of(&restart), "Hello, world!");
    assert_eq!(stderr_of(&restart), "");
}

#[test]
#[ignore = "needs Docker, network access and a built agent-box image"]
fn interrupted_installation_is_finished_without_apt() {
    let leave_hello_unpacked_then_init = "apt-get -qq update \
        && cd /tmp && apt-get -qq download hello \
        && dpkg --unpack hello_*.deb >/dev/null \
        && exec /usr/local/bin/agent-box init -- hello";
    let output = run_in_image(
        &[
            "--user",
            "0",
            "--entrypoint",
            "bash",
            "--env",
            "APT_PACKAGES=hello",
        ],
        &["-c", leave_hello_unpacked_then_init],
    );
    assert_eq!(last_line_of_stdout(&output), "Hello, world!");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("finishing an interrupted package installation"),
        "{stderr}"
    );
    assert!(!stderr.contains("installing hello"), "{stderr}");
}
