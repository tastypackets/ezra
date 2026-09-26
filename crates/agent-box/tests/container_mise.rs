//! Black-box tests of mise on the `/config` volume. Build the image first, then run: `cargo test -- --ignored`

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use common::{DockerResource, run_in_image, stdout_of};

#[test]
#[ignore = "needs Docker, network access and a built agent-box image"]
fn project_pin_overrides_the_image_version() {
    let pin_jq_then_run_it = "mkdir /projects/app && cd /projects/app \
        && printf '[tools]\\njq = \"1.7.1\"\\n' > mise.toml \
        && jq --version";
    let output = run_in_image(&[], &["bash", "-c", pin_jq_then_run_it]);
    assert_eq!(stdout_of(&output).lines().last(), Some("jq-1.7.1"));
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn image_version_is_used_outside_projects() {
    let output = run_in_image(&[], &["node", "--version"]);
    assert!(stdout_of(&output).starts_with("v26."));
}

#[test]
#[ignore = "needs Docker, network access and a built agent-box image"]
fn global_tools_persist_on_the_config_volume() {
    let volume = DockerResource::volume("mise-config");
    let config_mount = format!("{}:/config", volume.name);

    stdout_of(&run_in_image(
        &["--volume", &config_mount],
        &["mise", "use", "--global", "yq@4.44.1"],
    ));
    let output = run_in_image(&["--volume", &config_mount], &["yq", "--version"]);
    assert!(stdout_of(&output).ends_with("v4.44.1"));
}

#[test]
#[ignore = "needs Docker and a built agent-box image"]
fn setup_scripts_using_mise_tools_leave_the_agent_mise_directory_alone() {
    let directory = tempfile::tempdir().expect("temporary directory");
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let script = directory.path().join("10-uses-node");
    fs::write(&script, "#!/bin/sh\nnode --version > /dev/null\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let output = run_in_image(
        &[
            "--volume",
            &format!("{}:/etc/agent-box/setup.d:ro", directory.path().display()),
        ],
        &["find", "/config", "-user", "root"],
    );
    assert_eq!(stdout_of(&output), "");
}
