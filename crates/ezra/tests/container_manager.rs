//! Black-box tests of `ezra manager`. Build the image first, then run: `cargo test -- --ignored`

mod common;

use std::time::{Duration, Instant};

use common::{DockerResource, docker, run_in_image, stderr_of, stdout_of};

const UNCLAIMED: &str = r#"{"claimed":false,"authenticated":false}"#;

fn set_password(container: &DockerResource) -> String {
    let response = container.curl(&[
        "--write-out",
        "%{http_code}",
        "--output",
        "/dev/null",
        "--header",
        "Content-Type: application/json",
        "--data",
        r#"{"password":"correct horse"}"#,
        "https://localhost:8443/api/v1/setup",
    ]);
    stdout_of(&response)
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn default_command_serves_the_manager_over_https() {
    let container = DockerResource::start_container("manager", &[], &[]);
    assert_eq!(container.wait_for_manager(8443), UNCLAIMED);
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn the_manager_raises_its_open_file_limit_to_the_hard_limit() {
    let container = DockerResource::start_container(
        "manager-open-files",
        &["--ulimit", "nofile=1024:524288"],
        &[],
    );
    container.wait_for_manager(8443);
    let limits = stdout_of(&docker(&[
        "exec",
        "-u",
        "dev",
        &container.name,
        "sh",
        "-c",
        r#"prlimit --pid "$(pgrep -x ezra)" --nofile --noheadings --output SOFT,HARD --raw"#,
    ]));
    assert_eq!(limits, "524288 524288");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn port_can_be_changed() {
    let container =
        DockerResource::start_container("manager-port", &["--env", "EZRA_PORT=9443"], &[]);
    assert_eq!(container.wait_for_manager(9443), UNCLAIMED);
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn password_and_certificate_survive_a_recreate() {
    let volume = DockerResource::volume("manager-config");
    let config_mount = format!("{}:/config", volume.name);
    let certificate_checksum = |container: &DockerResource| {
        stdout_of(&docker(&[
            "exec",
            &container.name,
            "sha256sum",
            "/config/ezra/tls/certificate.pem",
        ]))
    };

    let first = DockerResource::start_container("manager-first", &["--volume", &config_mount], &[]);
    first.wait_for_manager(8443);
    assert_eq!(set_password(&first), "204");
    let owner = stdout_of(&docker(&[
        "exec",
        &first.name,
        "stat",
        "--format",
        "%U",
        "/config/ezra/settings.toml",
    ]));
    assert_eq!(owner, "dev");
    let first_checksum = certificate_checksum(&first);
    drop(first);

    let second =
        DockerResource::start_container("manager-second", &["--volume", &config_mount], &[]);
    assert_eq!(
        second.wait_for_manager(8443),
        r#"{"claimed":true,"authenticated":false}"#
    );
    assert_eq!(certificate_checksum(&second), first_checksum);
    assert_eq!(set_password(&second), "409");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn docker_stop_shuts_the_manager_down_cleanly() {
    let container = DockerResource::start_container("manager-stop", &[], &[]);
    container.wait_for_manager(8443);

    let started = Instant::now();
    assert!(docker(&["stop", &container.name]).status.success());
    assert!(started.elapsed() < Duration::from_secs(8));
    let exit_code = stdout_of(&docker(&[
        "inspect",
        "--format",
        "{{.State.ExitCode}}",
        &container.name,
    ]));
    assert_eq!(exit_code, "0");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn turning_off_download_certificate_checks_is_logged() {
    let container =
        DockerResource::start_container("manager-tls-off", &["--env", "EZRA_TLS_VERIFY=off"], &[]);
    container.wait_for_manager(8443);
    let logs = stderr_of(&docker(&["logs", &container.name]));
    assert!(
        logs.contains("agent downloads skip certificate verification"),
        "{logs}"
    );
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn unknown_certificate_check_setting_stops_the_manager() {
    let output = run_in_image(&["--env", "EZRA_TLS_VERIFY=maybe"], &["ezra", "manager"]);
    assert!(!output.status.success());
    assert!(
        stderr_of(&output).contains("for key `tls_verify`"),
        "{}",
        stderr_of(&output)
    );
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn repositories_ignore_claude_worktrees() {
    let container = DockerResource::start_container("manager-worktrees", &[], &[]);
    container.wait_for_manager(8443);
    let excludes_file_is_set = || {
        docker(&[
            "exec",
            "-u",
            "dev",
            &container.name,
            "git",
            "config",
            "--global",
            "core.excludesFile",
        ])
        .status
        .success()
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while !excludes_file_is_set() {
        assert!(Instant::now() < deadline, "core.excludesFile is not set");
        std::thread::sleep(Duration::from_millis(200));
    }
    let status = stdout_of(&docker(&[
        "exec",
        "-u",
        "dev",
        "--workdir",
        "/projects",
        &container.name,
        "sh",
        "-ec",
        "git init --quiet app && cd app \
         && git -c user.name=ezra -c user.email=ezra@example.com commit --quiet --allow-empty -m start \
         && git worktree add --quiet .claude/worktrees/feature \
         && git status --porcelain",
    ]));
    assert_eq!(status, "");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn agents_find_the_project_list_in_projects() {
    let container = DockerResource::start_container("manager-folders", &[], &[]);
    container.wait_for_manager(8443);
    docker(&["exec", &container.name, "mkdir", "/projects/app"]);

    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let agents_file = stdout_of(&docker(&[
            "exec",
            &container.name,
            "cat",
            "/projects/AGENTS.md",
        ]));
        if agents_file.contains("Folders:\n- app\n") {
            assert!(
                agents_file.contains("<!-- ezra:folders:start -->"),
                "{agents_file}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no project list in:\n{agents_file}"
        );
        std::thread::sleep(Duration::from_secs(1));
    }
}
