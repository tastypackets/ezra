#![allow(dead_code)]

use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

pub fn image_name() -> String {
    std::env::var("AGENT_BOX_TEST_IMAGE").unwrap_or_else(|_| "agent-box:dev".to_owned())
}

pub fn docker(arguments: &[&str]) -> Output {
    Command::new("docker")
        .args(arguments)
        .output()
        .expect("docker can be executed")
}

pub fn run_in_image(docker_options: &[&str], container_command: &[&str]) -> Output {
    let image = image_name();
    let mut arguments = vec!["run", "--rm"];
    arguments.extend_from_slice(docker_options);
    arguments.push(&image);
    arguments.extend_from_slice(container_command);
    docker(&arguments)
}

pub fn stdout_of(output: &Output) -> String {
    assert!(
        output.status.success(),
        "container failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

pub fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_owned()
}

pub fn process_status_field(docker_options: &[&str], field_name: &str) -> String {
    let status = stdout_of(&run_in_image(docker_options, &["cat", "/proc/self/status"]));
    status
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{field_name}:")))
        .unwrap_or_else(|| panic!("{field_name} missing from /proc/self/status"))
        .trim()
        .to_owned()
}

/// Runs curl inside the container, keeping the manager session cookie in /tmp/manager-cookies.
pub fn curl_in(container: &DockerResource, arguments: &[&str]) -> Output {
    let mut command = vec![
        "exec",
        &container.name,
        "curl",
        "--silent",
        "--insecure",
        "--cookie",
        "/tmp/manager-cookies",
        "--cookie-jar",
        "/tmp/manager-cookies",
    ];
    command.extend_from_slice(arguments);
    docker(&command)
}

/// Waits until the manager answers and returns its session status.
pub fn wait_for_manager(container: &DockerResource, port: u16) -> String {
    let url = format!("https://localhost:{port}/api/v1/session");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let response = curl_in(container, &["--fail", &url]);
        if response.status.success() {
            return stdout_of(&response);
        }
        assert!(
            Instant::now() < deadline,
            "manager did not answer: {}",
            stderr_of(&docker(&["logs", &container.name]))
        );
        thread::sleep(Duration::from_millis(200));
    }
}

/// Removes a Docker container or volume created by a test, even if the test panics.
pub struct DockerResource {
    pub kind: &'static str,
    pub name: String,
}

impl DockerResource {
    pub fn volume(purpose: &str) -> Self {
        Self {
            kind: "volume",
            name: unique_name(purpose),
        }
    }

    pub fn start_container(
        purpose: &str,
        docker_options: &[&str],
        container_command: &[&str],
    ) -> Self {
        let container = Self {
            kind: "container",
            name: unique_name(purpose),
        };
        let image = image_name();
        let mut arguments = vec!["run", "--detach", "--name", &container.name];
        arguments.extend_from_slice(docker_options);
        arguments.push(&image);
        arguments.extend_from_slice(container_command);
        let started = docker(&arguments);
        assert!(started.status.success(), "{}", stderr_of(&started));
        container
    }
}

impl Drop for DockerResource {
    fn drop(&mut self) {
        docker(&[self.kind, "rm", "--force", &self.name]);
    }
}

fn unique_name(purpose: &str) -> String {
    format!("agent-box-test-{purpose}-{}", std::process::id())
}
