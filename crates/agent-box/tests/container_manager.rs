//! Black-box tests of `agent-box manager`. Build the image first, then run: `cargo test -- --ignored`

mod common;

use std::time::{Duration, Instant};

use common::{DockerResource, curl_in, docker, stdout_of, wait_for_manager};

const UNCLAIMED: &str = r#"{"claimed":false,"authenticated":false}"#;

fn set_password(container: &DockerResource) -> String {
    let response = curl_in(
        container,
        &[
            "--write-out",
            "%{http_code}",
            "--output",
            "/dev/null",
            "--header",
            "Content-Type: application/json",
            "--data",
            r#"{"password":"correct horse"}"#,
            "https://localhost:8443/api/v1/setup",
        ],
    );
    stdout_of(&response)
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn default_command_serves_the_manager_over_https() {
    let container = DockerResource::start_container("manager", &[], &[]);
    assert_eq!(wait_for_manager(&container, 8443), UNCLAIMED);
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn port_can_be_changed() {
    let container =
        DockerResource::start_container("manager-port", &["--env", "MANAGER_PORT=9443"], &[]);
    assert_eq!(wait_for_manager(&container, 9443), UNCLAIMED);
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn password_and_certificate_survive_a_recreate() {
    let volume = DockerResource::volume("manager-config");
    let config_mount = format!("{}:/config", volume.name);
    let certificate_checksum = |container: &DockerResource| {
        stdout_of(&docker(&[
            "exec",
            &container.name,
            "sha256sum",
            "/config/agent-box/tls/certificate.pem",
        ]))
    };

    let first = DockerResource::start_container("manager-first", &["--volume", &config_mount], &[]);
    wait_for_manager(&first, 8443);
    assert_eq!(set_password(&first), "204");
    let owner = stdout_of(&docker(&[
        "exec",
        &first.name,
        "stat",
        "--format",
        "%U",
        "/config/agent-box/settings.toml",
    ]));
    assert_eq!(owner, "dev");
    let first_checksum = certificate_checksum(&first);
    drop(first);

    let second =
        DockerResource::start_container("manager-second", &["--volume", &config_mount], &[]);
    assert_eq!(
        wait_for_manager(&second, 8443),
        r#"{"claimed":true,"authenticated":false}"#
    );
    assert_eq!(certificate_checksum(&second), first_checksum);
    assert_eq!(set_password(&second), "409");
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn docker_stop_shuts_the_manager_down_cleanly() {
    let container = DockerResource::start_container("manager-stop", &[], &[]);
    wait_for_manager(&container, 8443);

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
#[ignore = "needs Docker and a built agent-box image"]
fn page_walks_from_password_to_dashboard() {
    let container = DockerResource::start_container("manager-page", &[], &[]);
    wait_for_manager(&container, 8443);
    let page = || stdout_of(&curl_in(&container, &["--fail", "https://localhost:8443/"]));

    assert!(page().contains("Set a password"));
    for asset in ["app.js", "app.css"] {
        let url = format!("https://localhost:8443/assets/{asset}");
        assert!(!stdout_of(&curl_in(&container, &["--fail", &url])).is_empty());
    }

    assert_eq!(set_password(&container), "204");
    let dashboard = page();
    assert!(dashboard.contains("Claude Code"), "{dashboard}");
    assert!(dashboard.contains("Codex"), "{dashboard}");
    assert!(dashboard.contains("Not installed"), "{dashboard}");
}
