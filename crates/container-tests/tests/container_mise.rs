//! Black-box tests of mise on the `/config` volume. Build the image first, then run: `cargo test -- --ignored`

use ezra_container_tests::{DockerResource, SetupDirectory, run_in_image, stdout_of};

/// Passes CI's token on, since unauthenticated GitHub API calls from shared runners hit the rate limit.
const MISE_GITHUB_TOKEN: [&str; 2] = ["--env", "MISE_GITHUB_TOKEN"];

#[test]
#[ignore = "needs Docker, network access and a built ezra image"]
fn project_pin_overrides_the_image_version() {
    let pin_jq_then_run_it = "mkdir /home/dev/projects/app && cd /home/dev/projects/app \
        && printf '[tools]\\njq = \"1.7.1\"\\n' > mise.toml \
        && jq --version";
    let output = run_in_image(&MISE_GITHUB_TOKEN, &["bash", "-c", pin_jq_then_run_it]);
    assert_eq!(stdout_of(&output).lines().last(), Some("jq-1.7.1"));
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn image_version_is_used_outside_projects() {
    let output = run_in_image(&[], &["node", "--version"]);
    assert!(stdout_of(&output).starts_with("v26."));
}

#[test]
#[ignore = "needs Docker, network access and a built ezra image"]
fn global_tools_persist_on_the_config_volume() {
    let volume = DockerResource::volume("mise-config");
    let config_mount = format!("{}:/config", volume.name);

    stdout_of(&run_in_image(
        &["--env", "MISE_GITHUB_TOKEN", "--volume", &config_mount],
        &["mise", "use", "--global", "yq@4.44.1"],
    ));
    let output = run_in_image(&["--volume", &config_mount], &["yq", "--version"]);
    assert!(stdout_of(&output).ends_with("v4.44.1"));
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn setup_scripts_using_mise_tools_leave_the_agent_mise_directory_alone() {
    let directory = SetupDirectory::with_scripts(&[(
        "10-uses-node",
        0o755,
        "#!/bin/sh\nnode --version > /dev/null\n",
    )]);

    let output = run_in_image(
        &["--volume", &directory.volume_option()],
        &["find", "/config", "-user", "root"],
    );
    assert_eq!(stdout_of(&output), "");
}
