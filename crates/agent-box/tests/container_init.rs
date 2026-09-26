//! Black-box tests of `agent-box init` as the image entrypoint.
//! Build the image first, then run: `cargo test -- --ignored`
//! (set `AGENT_BOX_TEST_IMAGE` to test an image other than `agent-box:dev`).

mod common;

use std::time::{Duration, Instant};

use common::{DockerResource, docker, process_status_field, run_in_image, stderr_of, stdout_of};

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn runs_as_uid_and_gid_1000() {
    assert_eq!(stdout_of(&run_in_image(&[], &["id", "--user"])), "1000");
    assert_eq!(stdout_of(&run_in_image(&[], &["id", "--group"])), "1000");
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn environment_describes_the_agent_user() {
    let environment = stdout_of(&run_in_image(&[], &["env"]));
    for expected in [
        "HOME=/home/dev",
        "USER=dev",
        "LOGNAME=dev",
        "SHELL=/bin/bash",
        "LANG=C.UTF-8",
    ] {
        assert!(
            environment.lines().any(|line| line == expected),
            "{expected} missing from:\n{environment}"
        );
    }
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn operator_home_is_respected() {
    let home = stdout_of(&run_in_image(
        &["--env", "HOME=/projects"],
        &["printenv", "HOME"],
    ));
    assert_eq!(home, "/projects");
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn no_capabilities_remain() {
    for capability_set in ["CapInh", "CapPrm", "CapEff", "CapAmb"] {
        assert_eq!(
            process_status_field(&[], capability_set),
            "0000000000000000",
            "{capability_set}"
        );
    }
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn runtime_added_groups_are_kept_and_root_group_is_not() {
    let groups = stdout_of(&run_in_image(&["--group-add", "4242"], &["id", "--groups"]));
    let group_ids: Vec<&str> = groups.split_whitespace().collect();
    assert!(group_ids.contains(&"4242"), "{groups}");
    assert!(!group_ids.contains(&"0"), "{groups}");
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn stderr_pipe_can_be_reopened() {
    assert!(
        run_in_image(&[], &["test", "-w", "/dev/stderr"])
            .status
            .success()
    );
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn tty_can_be_reopened() {
    assert!(
        run_in_image(&["--tty"], &["test", "-w", "/proc/self/fd/0"])
            .status
            .success()
    );
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn normal_start_logs_nothing() {
    assert_eq!(stderr_of(&run_in_image(&[], &["true"])), "");
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn exit_code_of_the_command_is_the_containers() {
    let output = run_in_image(&[], &["bash", "-c", "exit 42"]);
    assert_eq!(output.status.code(), Some(42));
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn missing_command_exits_127() {
    let output = run_in_image(&[], &["no-such-command-in-agent-box"]);
    assert_eq!(output.status.code(), Some(127));
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn docker_stop_delivers_sigterm_to_the_command() {
    let container = DockerResource::start_container("stop", &[], &["sleep", "infinity"]);

    let stop_started_at = Instant::now();
    assert!(
        docker(&["stop", "--timeout", "20", &container.name])
            .status
            .success()
    );
    let exit_code = stdout_of(&docker(&[
        "inspect",
        "--format",
        "{{.State.ExitCode}}",
        &container.name,
    ]));

    assert_eq!(exit_code, "143");
    assert!(stop_started_at.elapsed() < Duration::from_secs(10));
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn user_1000_gets_the_agent_environment() {
    let home = stdout_of(&run_in_image(
        &["--user", "1000:1000"],
        &["printenv", "HOME"],
    ));
    let user = stdout_of(&run_in_image(
        &["--user", "1000:1000"],
        &["printenv", "USER"],
    ));
    assert_eq!((home.as_str(), user.as_str()), ("/home/dev", "dev"));
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn unknown_user_gets_a_private_home() {
    let output = run_in_image(
        &["--user", "4321:4321"],
        &["bash", "-c", "test -w \"$HOME\" && printenv HOME"],
    );
    assert_eq!(stdout_of(&output), "/tmp/agent-box-home-4321");
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn docker_init_adds_no_warnings() {
    assert_eq!(stderr_of(&run_in_image(&["--init"], &["true"])), "");
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn read_only_root_filesystem_starts() {
    assert!(run_in_image(&["--read-only"], &["true"]).status.success());
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn fresh_named_volume_at_config_is_writable() {
    let volume = DockerResource::volume("config");
    let mount = format!("{}:/config", volume.name);
    assert!(
        run_in_image(&["--volume", &mount], &["test", "-w", "/config"])
            .status
            .success()
    );
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn root_owned_config_is_reported() {
    let output = run_in_image(&["--tmpfs", "/config:mode=0755"], &["true"]);
    assert!(
        stderr_of(&output).contains("/config is not writable"),
        "{}",
        stderr_of(&output)
    );
}
