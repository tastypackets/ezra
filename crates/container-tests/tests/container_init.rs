//! Black-box tests of `ezra init` as the image entrypoint.
//! Build the image first, then run: `cargo test -- --ignored`
//! (set `EZRA_TEST_IMAGE` to test an image other than `ezra:dev`).

use std::time::{Duration, Instant};

use ezra_container_tests::{
    DockerResource, docker, process_status_field, run_in_image, stderr_of, stdout_of,
};

const AGENT_DIRECTORIES: [(&str, &str); 4] = [
    ("config", "/config"),
    ("home", "/home/dev"),
    ("projects", "/home/dev/projects"),
    ("cache", "/cache"),
];

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn runs_as_uid_and_gid_1000() {
    assert_eq!(stdout_of(&run_in_image(&[], &["id", "--user"])), "1000");
    assert_eq!(stdout_of(&run_in_image(&[], &["id", "--group"])), "1000");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn every_directory_writable_by_everyone_is_sticky() {
    let output = run_in_image(
        &["--user", "0", "--entrypoint", "find"],
        &[
            "/", "-xdev", "-type", "d", "-perm", "-0002", "!", "-perm", "-1000",
        ],
    );
    assert_eq!(stdout_of(&output), "");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
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
#[ignore = "needs Docker and a built ezra image"]
fn operator_home_is_respected() {
    let home = stdout_of(&run_in_image(
        &["--env", "HOME=/work"],
        &["printenv", "HOME"],
    ));
    assert_eq!(home, "/work");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
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
#[ignore = "needs Docker and a built ezra image"]
fn runtime_added_groups_are_kept_and_root_group_is_not() {
    let groups = stdout_of(&run_in_image(&["--group-add", "4242"], &["id", "--groups"]));
    let group_ids: Vec<&str> = groups.split_whitespace().collect();
    assert!(group_ids.contains(&"4242"), "{groups}");
    assert!(!group_ids.contains(&"0"), "{groups}");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn stderr_pipe_can_be_reopened() {
    assert!(
        run_in_image(&[], &["test", "-w", "/dev/stderr"])
            .status
            .success()
    );
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn tty_can_be_reopened() {
    assert!(
        run_in_image(&["--tty"], &["test", "-w", "/proc/self/fd/0"])
            .status
            .success()
    );
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn normal_start_logs_nothing() {
    assert_eq!(stderr_of(&run_in_image(&[], &["true"])), "");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn exit_code_of_the_command_is_the_containers() {
    let output = run_in_image(&[], &["bash", "-c", "exit 42"]);
    assert_eq!(output.status.code(), Some(42));
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn missing_command_exits_127() {
    let output = run_in_image(&[], &["no-such-command-in-ezra"]);
    assert_eq!(output.status.code(), Some(127));
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
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
#[ignore = "needs Docker and a built ezra image"]
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
#[ignore = "needs Docker and a built ezra image"]
fn unknown_user_gets_a_temporary_home() {
    let output = run_in_image(
        &["--user", "4321:4321"],
        &["bash", "-c", "test -w \"$HOME\" && printenv HOME"],
    );
    assert_eq!(stdout_of(&output), "/tmp/ezra-home-4321");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn unknown_user_writes_to_directories_mounted_for_it() {
    let mounts = AGENT_DIRECTORIES.map(|(_, directory)| format!("{directory}:uid=4321,gid=4321"));
    let mut options = vec!["--user", "4321:4321"];
    for mount in &mounts {
        options.extend(["--tmpfs", mount]);
    }
    let output = run_in_image(&options, &["touch", "/home/dev/projects/app"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(
        !stderr_of(&output).contains("is not writable"),
        "{}",
        stderr_of(&output)
    );
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn docker_init_adds_no_warnings() {
    assert_eq!(stderr_of(&run_in_image(&["--init"], &["true"])), "");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn read_only_root_filesystem_starts() {
    assert!(run_in_image(&["--read-only"], &["true"]).status.success());
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn fresh_named_volumes_belong_to_dev() {
    for (purpose, directory) in AGENT_DIRECTORIES {
        let volume = DockerResource::volume(purpose);
        let mount = format!("{}:{directory}", volume.name);
        let output = run_in_image(
            &["--volume", &mount],
            &["stat", "--format=%u:%g", directory],
        );
        assert_eq!(stdout_of(&output), "1000:1000", "{directory}");
        assert_eq!(stderr_of(&output), "", "{directory}");
    }
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn empty_root_owned_mounts_are_handed_to_dev() {
    for (_, directory) in AGENT_DIRECTORIES {
        let tmpfs = format!("{directory}:mode=0755");
        let output = run_in_image(&["--tmpfs", &tmpfs], &["stat", "--format=%u:%g", directory]);
        assert_eq!(stdout_of(&output), "1000:1000", "{directory}");
        let stderr = stderr_of(&output);
        assert!(
            stderr.contains(&format!(
                "{directory} was an empty mount owned by root, so it now belongs to dev"
            )),
            "{stderr}"
        );
        assert!(!stderr.contains("is not writable"), "{stderr}");
    }
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn root_owned_mounts_init_cannot_hand_over_are_reported() {
    for (_, directory) in AGENT_DIRECTORIES {
        let tmpfs = format!("{directory}:mode=0755");
        for extra_options in [
            ["--cap-drop", "CHOWN"],
            ["--env", "EZRA_CHOWN_EMPTY_MOUNTS=off"],
        ] {
            let mut options = vec!["--tmpfs", &tmpfs];
            options.extend(extra_options);
            let output = run_in_image(&options, &["stat", "--format=%u:%g", directory]);
            assert_eq!(stdout_of(&output), "0:0", "{directory} {extra_options:?}");
            assert!(
                stderr_of(&output).contains(&format!("{directory} is not writable by uid 1000")),
                "{}",
                stderr_of(&output)
            );
        }
    }
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn root_owned_mounts_with_contents_are_left_alone() {
    let volume = DockerResource::volume("filled");
    let filling_mount = format!("{}:/filled", volume.name);
    stdout_of(&run_in_image(
        &[
            "--volume",
            &filling_mount,
            "--user",
            "0:0",
            "--entrypoint",
            "touch",
        ],
        &["/filled/file"],
    ));
    for (_, directory) in AGENT_DIRECTORIES {
        let mount = format!("{}:{directory}", volume.name);
        let output = run_in_image(
            &["--volume", &mount],
            &["stat", "--format=%u:%g", directory],
        );
        assert_eq!(stdout_of(&output), "0:0", "{directory}");
        assert!(
            stderr_of(&output).contains(&format!("{directory} is not writable by uid 1000")),
            "{}",
            stderr_of(&output)
        );
    }
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn a_symlink_in_place_of_projects_is_not_followed_after_a_restart() {
    let container = DockerResource::start_container(
        "symlink",
        &["--tmpfs", "/srv/target:mode=0755"],
        &["sleep", "infinity"],
    );
    let swapped = container.run_as_agent(&[
        "bash",
        "-c",
        "rmdir /home/dev/projects && ln --symbolic /srv/target /home/dev/projects",
    ]);
    assert!(swapped.status.success(), "{}", stderr_of(&swapped));
    let restarted = docker(&["restart", &container.name]);
    assert!(restarted.status.success(), "{}", stderr_of(&restarted));
    let owner = docker(&[
        "exec",
        &container.name,
        "stat",
        "--format=%u:%g",
        "/srv/target",
    ]);
    assert_eq!(stdout_of(&owner), "0:0");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn global_git_settings_live_on_the_config_volume() {
    let output = run_in_image(
        &[],
        &[
            "bash",
            "-c",
            "git config --global user.name Tester && git config --file /config/git/config user.name",
        ],
    );
    assert_eq!(stdout_of(&output), "Tester", "{}", stderr_of(&output));
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn empty_home_mount_creates_projects_as_dev() {
    let output = run_in_image(
        &["--tmpfs", "/home/dev:mode=0755"],
        &[
            "sh",
            "-c",
            "test -w /home/dev/projects && stat --format=%u:%g /home/dev/projects",
        ],
    );
    assert_eq!(stdout_of(&output), "1000:1000");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn home_keeps_projects_and_agent_data_across_recreation() {
    let volume = DockerResource::volume("persistent-home");
    let mount = format!(
        "type=volume,source={},target=/home/dev,volume-nocopy",
        volume.name
    );
    let first = run_in_image(
        &["--mount", &mount],
        &[
            "sh",
            "-ec",
            "mkdir -p /home/dev/projects/app \
          /home/dev/.codex /home/dev/.claude /home/dev/.local/bin; \
          echo repository > /home/dev/projects/app/file; \
          echo '# persistent settings' > /home/dev/.codex/config.toml; \
          echo '{}' > /home/dev/.claude/settings.json; \
          echo '{}' > /home/dev/.claude/.claude.json; \
          echo custom > /home/dev/.bashrc; \
          ln -s /cache/missing-codex /home/dev/.local/bin/codex",
        ],
    );
    stdout_of(&first);
    let second = run_in_image(
        &["--mount", &mount],
        &[
            "sh",
            "-ec",
            "test \"$CODEX_HOME\" = /home/dev/.codex; \
          test \"$CLAUDE_CONFIG_DIR\" = /home/dev/.claude; \
          test -w /home/dev/projects; \
          test -L /home/dev/.local/bin/codex; \
          cat /home/dev/projects/app/file /home/dev/.codex/config.toml /home/dev/.claude/settings.json /home/dev/.claude/.claude.json /home/dev/.bashrc",
        ],
    );
    assert_eq!(
        stdout_of(&second),
        "repository\n# persistent settings\n{}\n{}\ncustom"
    );
}
