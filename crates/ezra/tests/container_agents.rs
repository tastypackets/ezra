//! Black-box tests of installing the agent CLIs through the manager. They download the real releases.
//! Build the image first, then run: `cargo test -- --ignored`

mod common;

use std::thread;
use std::time::{Duration, Instant};

use common::{DockerResource, docker, stdout_of};

const REINSTALL_TIMEOUT: Duration = Duration::from_secs(300);

#[test]
#[ignore = "needs Docker, network access and a built ezra image"]
fn installed_agents_run_and_offer_sign_in() {
    let container = DockerResource::start_logged_in_manager("agents-install", &[]);
    let saw_download_progress = thread::scope(|scope| {
        let claude_install = scope.spawn(|| container.post("/api/v1/agents/claude/install", ""));
        let mut saw_progress = false;
        while !claude_install.is_finished() {
            saw_progress |= container
                .get("/api/v1/agents")
                .contains(r#""install_progress":{"received_bytes":"#);
            thread::sleep(Duration::from_millis(200));
        }
        let (status, body) = claude_install.join().expect("install thread finishes");
        assert_eq!(status, "200", "{body}");
        assert!(body.contains(r#""configured":true"#), "{body}");
        saw_progress
    });
    assert!(
        saw_download_progress,
        "the Claude download never reported progress"
    );
    let (status, body) = container.post("/api/v1/agents/codex/install", "");
    assert_eq!(status, "200", "{body}");
    assert!(body.contains(r#""install_progress":null"#), "{body}");

    let large_files_on_the_volume = stdout_of(&docker(&[
        "exec",
        &container.name,
        "find",
        "/config/claude",
        "/config/codex",
        "-size",
        "+1M",
    ]));
    assert_eq!(
        large_files_on_the_volume, "",
        "binaries must not be persisted"
    );

    let claude_version = stdout_of(&container.run_as_agent(&["claude", "--version"]));
    assert!(
        claude_version.ends_with("(Claude Code)"),
        "{claude_version}"
    );
    let codex_version = stdout_of(&container.run_as_agent(&["codex", "--version"]));
    assert!(codex_version.starts_with("codex-cli "), "{codex_version}");

    let (status, claude_prompt) = container.post("/api/v1/agents/claude/login", "");
    assert_eq!(status, "200", "{claude_prompt}");
    assert!(
        claude_prompt.contains(r#""url":"https://claude.com/cai/oauth/authorize?"#),
        "{claude_prompt}"
    );
    let (status, codex_prompt) = container.post("/api/v1/agents/codex/login", "");
    assert_eq!(status, "200", "{codex_prompt}");
    assert!(
        codex_prompt.contains(r#""url":"https://auth.openai.com/codex/device""#),
        "{codex_prompt}"
    );
    assert!(!codex_prompt.contains(r#""code":null"#), "{codex_prompt}");
    let listing = container.get("/api/v1/agents");
    assert_eq!(
        listing.matches(r#""login_prompt":{"url""#).count(),
        2,
        "{listing}"
    );

    let (status, body) = container.post(
        "/api/v1/agents/claude/login/code",
        r#"{"code":"not-a-real-code"}"#,
    );
    assert_eq!(status, "502", "{body}");
    for agent in ["claude", "codex"] {
        let (status, body) = container.post(&format!("/api/v1/agents/{agent}/logout"), "");
        assert_eq!(status, "204", "{agent}: {body}");
    }
    let listing = container.get("/api/v1/agents");
    assert_eq!(
        listing
            .matches(
                r#""logged_in":false,"account":null,"sign_in_ends_at":null,"login_prompt":null"#
            )
            .count(),
        2,
        "{listing}"
    );
}

#[test]
#[ignore = "needs Docker, network access and a built ezra image"]
fn configured_agent_is_reinstalled_after_a_recreate() {
    let volume = DockerResource::volume("agents-config");
    let config_mount = format!("{}:/config", volume.name);

    let first =
        DockerResource::start_logged_in_manager("agents-first", &["--volume", &config_mount]);
    let (status, body) = first.post("/api/v1/agents/codex/install", "");
    assert_eq!(status, "200", "{body}");
    drop(first);

    let second =
        DockerResource::start_container("agents-second", &["--volume", &config_mount], &[]);
    let started = Instant::now();
    while !second
        .run_as_agent(&["codex", "--version"])
        .status
        .success()
    {
        assert!(
            started.elapsed() < REINSTALL_TIMEOUT,
            "codex was not reinstalled"
        );
        thread::sleep(Duration::from_secs(2));
    }
    assert!(
        !second
            .run_as_agent(&["bash", "-c", "command -v claude"])
            .status
            .success(),
        "claude was never configured, so it must not be installed"
    );
}
