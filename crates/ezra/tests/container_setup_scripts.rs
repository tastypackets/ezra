//! Black-box tests of `/etc/ezra/setup.d`. Build the image first, then run: `cargo test -- --ignored`

mod common;

use common::{DockerResource, SetupDirectory, docker, run_in_image, stderr_of, stdout_of};

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn scripts_run_as_root_in_name_order_before_the_agent_starts() {
    let directory = SetupDirectory::with_scripts(&[
        (
            "20-second",
            0o755,
            "#!/bin/sh\necho \"20:$(id -u)\" >> /tmp/setup-order\n",
        ),
        (
            "10-first.sh",
            0o755,
            "#!/bin/bash\necho \"10:$(id -u)\" >> /tmp/setup-order\n",
        ),
    ]);
    let output = run_in_image(
        &["--volume", &directory.volume_option()],
        &["cat", "/tmp/setup-order"],
    );
    assert_eq!(stdout_of(&output), "10:0\n20:0");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn script_output_goes_to_the_container_log() {
    let directory =
        SetupDirectory::with_scripts(&[("10-hello", 0o755, "#!/bin/sh\necho hello from setup\n")]);
    let output = run_in_image(&["--volume", &directory.volume_option()], &["true"]);
    assert_eq!(stdout_of(&output), "hello from setup");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn failing_script_warns_and_the_rest_still_run() {
    let directory = SetupDirectory::with_scripts(&[
        ("10-fails", 0o755, "#!/bin/sh\nexit 3\n"),
        ("20-runs", 0o755, "#!/bin/sh\ntouch /tmp/second-ran\n"),
    ]);
    let output = run_in_image(
        &["--volume", &directory.volume_option()],
        &["test", "-e", "/tmp/second-ran"],
    );
    stdout_of(&output);
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("/etc/ezra/setup.d/10-fails failed with exit status: 3"),
        "{stderr}"
    );
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn file_that_is_not_executable_is_skipped_with_a_warning() {
    let directory = SetupDirectory::with_scripts(&[
        ("10-forgot-chmod.sh", 0o644, "#!/bin/sh\ntouch /tmp/ran\n"),
        (".gitkeep", 0o644, ""),
    ]);
    let output = run_in_image(
        &["--volume", &directory.volume_option()],
        &["test", "!", "-e", "/tmp/ran"],
    );
    stdout_of(&output);
    let stderr = stderr_of(&output);
    assert!(
        stderr
            .contains("skipping /etc/ezra/setup.d/10-forgot-chmod.sh because it is not executable"),
        "{stderr}"
    );
    assert!(!stderr.contains(".gitkeep"), "{stderr}");
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn script_without_an_interpreter_line_warns() {
    let directory = SetupDirectory::with_scripts(&[("10-no-shebang", 0o755, "touch /tmp/ran\n")]);
    let output = run_in_image(&["--volume", &directory.volume_option()], &["true"]);
    stdout_of(&output);
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("could not run /etc/ezra/setup.d/10-no-shebang"),
        "{stderr}"
    );
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn non_root_start_ignores_the_scripts() {
    let directory =
        SetupDirectory::with_scripts(&[("10-root-only", 0o755, "#!/bin/sh\ntouch /tmp/ran\n")]);
    let output = run_in_image(
        &[
            "--user",
            "1000:1000",
            "--volume",
            &directory.volume_option(),
        ],
        &["test", "!", "-e", "/tmp/ran"],
    );
    stdout_of(&output);
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("/etc/ezra/setup.d is ignored when the container starts as uid 1000"),
        "{stderr}"
    );
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn root_never_runs_what_the_agent_puts_on_its_path() {
    let directory = SetupDirectory::with_scripts(&[(
        "10-tools",
        0o755,
        "#!/bin/sh\ndpkg-query --version >/dev/null\njq --version >/dev/null && echo \"jq works in $PWD\"\nstat -c 'temporary files in %n, %U %a' \"$TMPDIR\"\n",
    )]);
    let plant = "for directory in /home/dev/.local/bin /config/mise/shims; do \
        mkdir -p $directory \
        && printf '#!/bin/sh\\ntouch /tmp/planted-ran\\n' > $directory/dpkg-query \
        && chmod +x $directory/dpkg-query; done; \
        if [ -e /tmp/planted-ran ]; then echo planted ran; fi; \
        command -v dpkg-query";
    let container = DockerResource::start_container(
        "root-path",
        &[
            "--volume",
            &directory.volume_option(),
            "--env",
            "EZRA_APT_PACKAGES=bash",
            "--workdir",
            "/home/dev/projects",
        ],
        &["sh", "-c", plant],
    );
    assert_eq!(stdout_of(&docker(&["wait", &container.name])), "0");

    let restart = docker(&["start", "--attach", &container.name]);
    let stdout = stdout_of(&restart);
    assert!(!stdout.contains("planted ran"), "{stdout}");
    let lines: Vec<&str> = stdout.lines().collect();
    for expected in [
        "/home/dev/.local/bin/dpkg-query",
        "jq works in /",
        "temporary files in /root/tmp, root 700",
    ] {
        assert!(lines.contains(&expected), "{stdout}");
    }
}
