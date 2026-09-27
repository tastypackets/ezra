//! Black-box tests of saving the agents' settings files through the manager.
//! Build the image first, then run: `cargo test -- --ignored`

mod common;

use std::fs::{self, Permissions};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use common::{DockerResource, curl_in, docker, run_in_image, stdout_of, wait_for_manager};
use serde_json::{Value, json};

/// Sends a request to the manager API and returns the status code and the JSON body.
fn request(
    container: &DockerResource,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (String, Value) {
    let url = format!("https://localhost:8443{path}");
    let body = body.map(Value::to_string);
    let mut arguments = vec![
        "--request",
        method,
        "--header",
        "Content-Type: application/json",
        "--write-out",
        "\n%{http_code}",
    ];
    if let Some(body) = &body {
        arguments.extend(["--data-binary", body]);
    }
    arguments.push(&url);
    let output =
        String::from_utf8(curl_in(container, &arguments).stdout).expect("curl prints text");
    let (response, status) = output
        .rsplit_once('\n')
        .expect("curl prints the status last");
    let response = if response.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(response).expect("the body is JSON")
    };
    (status.to_owned(), response)
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn a_settings_file_mounted_on_its_own_is_saved_in_place() {
    let volume = DockerResource::volume("settings-file");
    let config = format!("{}:/config", volume.name);
    stdout_of(&run_in_image(
        &["--volume", &config],
        &["mkdir", "/config/claude"],
    ));
    let host = tempfile::tempdir().expect("temporary directory");
    let settings = host.path().join("settings.json");
    fs::write(&settings, "{\r\n  \"model\": \"opus\"\r\n}\r\n").expect("the file is written");
    fs::set_permissions(&settings, Permissions::from_mode(0o640)).expect("the mode is set");
    let inode = fs::metadata(&settings).expect("the file exists").ino();
    let mount = format!("{}:/config/claude/settings.json", settings.display());
    let container = DockerResource::start_container(
        "settings-file",
        &["--volume", &config, "--volume", &mount],
        &[],
    );
    wait_for_manager(&container, 8443);
    let (status, _) = request(
        &container,
        "POST",
        "/api/v1/setup",
        Some(&json!({ "password": "x" })),
    );
    assert_eq!(status, "204");

    let (status, opened) = request(
        &container,
        "GET",
        "/api/v1/agents/claude/settings-file",
        None,
    );
    assert_eq!(status, "200", "{opened}");
    assert_eq!(opened["path"], "/config/claude/settings.json");
    assert_eq!(opened["format"], "json");
    assert_eq!(opened["text"], "{\r\n  \"model\": \"opus\"\r\n}\r\n");

    let text = "\u{feff}{\r\n\t\"model\":  \"sonnet\"\r\n}";
    let (status, saved) = request(
        &container,
        "PUT",
        "/api/v1/agents/claude/settings-file",
        Some(&json!({ "text": text, "version": opened["version"] })),
    );
    assert_eq!(status, "200", "{saved}");
    assert_eq!(
        fs::read(&settings).expect("the file is read"),
        text.as_bytes()
    );
    let metadata = fs::metadata(&settings).expect("the file exists");
    assert_eq!(metadata.ino(), inode);
    assert_eq!(metadata.permissions().mode() & 0o7777, 0o640);
    let leftovers = stdout_of(&docker(&[
        "exec",
        &container.name,
        "ls",
        "-A",
        "/config/claude",
    ]));
    assert!(!leftovers.contains(".ezra"), "{leftovers}");

    let (status, conflict) = request(
        &container,
        "PUT",
        "/api/v1/agents/claude/settings-file",
        Some(&json!({ "text": "{}", "version": opened["version"] })),
    );
    assert_eq!(status, "409", "{conflict}");

    let (status, codex) = request(
        &container,
        "GET",
        "/api/v1/agents/codex/settings-file",
        None,
    );
    assert_eq!(status, "200", "{codex}");
    assert_eq!(codex["path"], "/config/codex/config.toml");
    assert_eq!(codex["format"], "toml");
    assert_eq!(codex["text"], "");
    let (status, saved) = request(
        &container,
        "PUT",
        "/api/v1/agents/codex/settings-file",
        Some(&json!({ "text": "model = \"gpt\"\n", "version": codex["version"] })),
    );
    assert_eq!(status, "200", "{saved}");
    let created = stdout_of(&docker(&[
        "exec",
        &container.name,
        "stat",
        "--format",
        "%a %U",
        "/config/codex/config.toml",
    ]));
    assert_eq!(created, "600 dev");
}
