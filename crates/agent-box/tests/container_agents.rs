//! Black-box tests of installing the agent CLIs through the manager. They download the real releases.
//! Build the image first, then run: `cargo test -- --ignored`

mod common;

use std::thread;
use std::time::{Duration, Instant};

use common::{DockerResource, curl_in, docker, stdout_of, wait_for_manager};

/// Posts to the manager API and returns the status code and body.
fn post(container: &DockerResource, path: &str, body: &str) -> (String, String) {
    let url = format!("https://localhost:8443{path}");
    let output = stdout_of(&curl_in(
        container,
        &[
            "--header",
            "Content-Type: application/json",
            "--data",
            body,
            "--write-out",
            "\n%{http_code}",
            &url,
        ],
    ));
    let (response_body, status) = output.rsplit_once('\n').unwrap_or(("", &output));
    (status.to_owned(), response_body.to_owned())
}

fn start_logged_in_manager(purpose: &str, docker_options: &[&str]) -> DockerResource {
    let container = DockerResource::start_container(purpose, docker_options, &[]);
    wait_for_manager(&container, 8443);
    let (status, body) = post(&container, "/api/v1/setup", r#"{"password":"x"}"#);
    assert_eq!(status, "204", "{body}");
    container
}

fn run_as_agent(container: &DockerResource, command: &[&str]) -> std::process::Output {
    let mut arguments = vec!["exec", "--user", "dev", &container.name];
    arguments.extend_from_slice(command);
    docker(&arguments)
}

#[test]
#[ignore = "needs Docker, network access and a built agent-box image"]
fn installed_agents_run_and_leave_their_config_directories_empty() {
    let container = start_logged_in_manager("agents-install", &[]);
    for agent in ["claude", "codex"] {
        let (status, body) = post(&container, &format!("/api/v1/agents/{agent}/install"), "");
        assert_eq!(status, "200", "{body}");
        assert!(body.contains(r#""configured":true"#), "{body}");
    }

    let config_contents = stdout_of(&docker(&[
        "exec",
        &container.name,
        "find",
        "/config/claude",
        "/config/codex",
        "-mindepth",
        "1",
    ]));
    assert_eq!(config_contents, "");

    let claude_version = stdout_of(&run_as_agent(&container, &["claude", "--version"]));
    assert!(
        claude_version.ends_with("(Claude Code)"),
        "{claude_version}"
    );
    let codex_version = stdout_of(&run_as_agent(&container, &["codex", "--version"]));
    assert!(codex_version.starts_with("codex-cli "), "{codex_version}");
}

#[test]
#[ignore = "needs Docker, network access and a built agent-box image"]
fn configured_agent_is_reinstalled_after_a_recreate() {
    let volume = DockerResource::volume("agents-config");
    let config_mount = format!("{}:/config", volume.name);

    let first = start_logged_in_manager("agents-first", &["--volume", &config_mount]);
    let (status, body) = post(&first, "/api/v1/agents/codex/install", "");
    assert_eq!(status, "200", "{body}");
    drop(first);

    let second =
        DockerResource::start_container("agents-second", &["--volume", &config_mount], &[]);
    let deadline = Instant::now() + Duration::from_secs(300);
    while !run_as_agent(&second, &["codex", "--version"])
        .status
        .success()
    {
        assert!(Instant::now() < deadline, "codex was not reinstalled");
        thread::sleep(Duration::from_secs(2));
    }
    assert!(
        !run_as_agent(&second, &["bash", "-c", "command -v claude"])
            .status
            .success(),
        "claude was never configured, so it must not be installed"
    );
}
