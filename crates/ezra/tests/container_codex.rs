//! Black-box test of Codex remote control against the real Codex release, which it downloads.
//! Build the image first, then run: `cargo test --test container_codex -- --ignored`

mod common;

use std::process::Output;
use std::thread;
use std::time::{Duration, Instant};

use common::{DockerResource, Manager, docker, stderr_of, stdout_of};
use serde_json::Value;

const CODEX_HOME: &str = "/config/codex";
const SERVER_LOG_FILTER: &str = "error,codex_app_server_transport::transport::remote_control=warn";
const SIGNED_OUT: &str = "remote control requires ChatGPT authentication";
const SIGN_IN_NOTICED: Duration = Duration::from_secs(90);
const SERVER_READY: Duration = Duration::from_secs(30);
const RELAY_ERROR_SHOWN: Duration = Duration::from_secs(20);
const SERVER_STOPPED: Duration = Duration::from_secs(5);
const SERVER_PID: &str = "/tmp/codex-server.pid";
const SERVER_STDERR: &str = "/tmp/codex-server.stderr";
const SERVER_EXIT: &str = "/tmp/codex-server.exit";

impl Manager {
    /// Checks that Codex waits with no server running and returns the overview's Codex part.
    fn codex_off(&self) -> Value {
        let (status, overview) = self.request("GET", "/api/v1/remote-control", None);
        assert_eq!(status, "200", "{overview}");
        let codex = overview["codex"].clone();
        assert_eq!(codex["state"], "waiting", "{codex}");
        assert_eq!(self.container.remote_control_servers(), "", "{codex}");
        codex
    }
}

impl DockerResource {
    fn remote_control_servers(&self) -> String {
        let found = docker(&[
            "exec",
            &self.name,
            "pgrep",
            "-u",
            "dev",
            "-a",
            "-f",
            "app-server --remote-control",
        ]);
        assert!(
            matches!(found.status.code(), Some(0 | 1)),
            "{}",
            stderr_of(&found)
        );
        String::from_utf8_lossy(&found.stdout).trim().to_owned()
    }

    /// Runs a Codex command as dev in /projects with Codex's home set, as the manager does.
    fn run_codex(&self, docker_options: &[&str], command: &[&str]) -> Output {
        let codex_home = format!("CODEX_HOME={CODEX_HOME}");
        let mut arguments = vec![
            "exec",
            "--user",
            "dev",
            "--workdir",
            "/projects",
            "--env",
            &codex_home,
        ];
        arguments.extend_from_slice(docker_options);
        arguments.push(&self.name);
        arguments.extend_from_slice(command);
        docker(&arguments)
    }

    fn read_until<T>(&self, file: &str, within: Duration, parse: impl Fn(&str) -> Option<T>) -> T {
        let started = Instant::now();
        loop {
            let read = self.run_as_agent(&["cat", file]);
            if let Some(value) = parse(String::from_utf8_lossy(&read.stdout).trim()) {
                return value;
            }
            assert!(
                started.elapsed() < within,
                "{file} after {within:?}: {}{}",
                String::from_utf8_lossy(&read.stdout),
                String::from_utf8_lossy(&read.stderr)
            );
            thread::sleep(Duration::from_millis(200));
        }
    }
}

#[test]
#[ignore = "needs Docker, network access and a built ezra image"]
fn real_codex_stays_off_until_chatgpt_and_takes_ezras_launch() {
    let manager = Manager::start("codex", &[]);
    let container = &manager.container;
    let (status, body) = manager.request("POST", "/api/v1/agents/codex/install", None);
    assert_eq!(status, "200", "{body}");

    manager.codex_off();

    let features = stdout_of(&container.run_codex(&[], &["codex", "features", "list"]));
    let daemon_auto_start = features
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .find(|columns| columns.first() == Some(&"daemon_auto_start"))
        .expect("codex lists daemon_auto_start");
    assert_eq!(daemon_auto_start.last(), Some(&"false"), "{features}");
    let config = stdout_of(&container.run_as_agent(&[
        "stat",
        "--format",
        "%U:%G %a",
        "/etc/codex/config.toml",
    ]));
    assert_eq!(config, "root:root 644");

    stdout_of(&container.run_codex(
        &[],
        &[
            "sh",
            "-c",
            "printf sk-ezra-test | codex login --with-api-key",
        ],
    ));
    let started = Instant::now();
    loop {
        let codex = manager.codex_off();
        if codex["problem"] == "not_chatgpt" {
            break;
        }
        assert!(started.elapsed() < SIGN_IN_NOTICED, "{codex}");
        thread::sleep(Duration::from_secs(1));
    }
    stdout_of(&container.run_codex(&[], &["codex", "logout"]));

    let codex =
        stdout_of(&container.run_as_agent(&["sh", "-c", r#"readlink -f "$(command -v codex)""#]));
    let takes = |flag| {
        container
            .run_codex(&[], &[&codex, "app-server", flag, "--help"])
            .status
            .success()
    };
    assert!(
        takes("--remote-control"),
        "{codex} refuses --remote-control"
    );
    let mut launch = vec![codex.as_str(), "app-server", "--remote-control"];
    if takes("--managed-daemon") {
        launch.push("--managed-daemon");
    }
    launch.extend([
        "--listen",
        "unix://",
        "-c",
        r#"sandbox_mode="danger-full-access""#,
        "-c",
        r#"approval_policy="on-request""#,
    ]);
    let script = format!(
        r#""$@" </dev/null >/dev/null 2>{SERVER_STDERR} & echo $! >{SERVER_PID}; wait $!; echo $? >{SERVER_EXIT}"#
    );
    let log_filter = format!("RUST_LOG={SERVER_LOG_FILTER}");
    let mut command = vec!["sh", "-c", script.as_str(), "sh"];
    command.extend(launch);
    let launched = container.run_codex(&["--detach", "--env", &log_filter], &command);
    assert!(launched.status.success(), "{launched:?}");
    let pid = container.read_until(SERVER_PID, SERVER_READY, |pid| {
        pid.parse::<u32>().ok().map(|pid| pid.to_string())
    });

    let started = Instant::now();
    let version = loop {
        let version = container.run_codex(&[], &["codex", "app-server", "daemon", "version"]);
        if version.status.success() {
            break version;
        }
        let exit = container.run_as_agent(&["cat", SERVER_EXIT]);
        assert!(
            !exit.status.success() && started.elapsed() < SERVER_READY,
            "server exit {}, daemon version: {}, server stderr: {}",
            String::from_utf8_lossy(&exit.stdout),
            stderr_of(&version),
            String::from_utf8_lossy(&container.run_as_agent(&["cat", SERVER_STDERR]).stdout)
        );
        thread::sleep(Duration::from_millis(250));
    };
    let version: Value =
        serde_json::from_slice(&version.stdout).expect("daemon version prints JSON");
    let installed = stdout_of(&container.run_as_agent(&["codex", "--version"]));
    assert_eq!(version["status"], "running", "{version}");
    assert_eq!(
        version["appServerVersion"].as_str(),
        installed.strip_prefix("codex-cli "),
        "{version}"
    );

    let stderr = container.read_until(SERVER_STDERR, RELAY_ERROR_SHOWN, |stderr| {
        stderr.contains(SIGNED_OUT).then(|| stderr.to_owned())
    });
    assert!(
        !stderr.contains("API key auth is not supported"),
        "{stderr}"
    );
    eprintln!("The server's stderr:\n{stderr}");

    stdout_of(&container.run_as_agent(&["kill", "-TERM", &pid]));
    let exit = container.read_until(SERVER_EXIT, SERVER_STOPPED, |exit| {
        (!exit.is_empty()).then(|| exit.to_owned())
    });
    assert_eq!(exit, "0");

    let codex_home = stdout_of(&container.run_as_agent(&["ls", "-A", CODEX_HOME]));
    assert!(
        !codex_home.lines().any(|entry| entry == "packages"),
        "{codex_home}"
    );
}
