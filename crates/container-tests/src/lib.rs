use std::fs::{self, Permissions};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

pub fn image_name() -> String {
    std::env::var("EZRA_TEST_IMAGE").unwrap_or_else(|_| "ezra:dev".to_owned())
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

const MANAGER_START_TIMEOUT: Duration = Duration::from_secs(20);
const MANAGER_COOKIES: &str = "/tmp/manager-cookies";
const EVENTS_CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
/// The uid of the container's `dev` user, who runs the manager and the agents.
pub const AGENT_UID: u32 = 1000;

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

    /// Runs curl inside the container with the manager session cookie from `MANAGER_COOKIES`.
    /// Only the request that signs in writes the file, so concurrent requests never read it half
    /// written.
    pub fn curl(&self, arguments: &[&str]) -> Output {
        let mut command = vec![
            "exec",
            &self.name,
            "curl",
            "--silent",
            "--insecure",
            "--cookie",
            MANAGER_COOKIES,
        ];
        command.extend_from_slice(arguments);
        docker(&command)
    }

    /// Waits until the manager answers and returns its session status.
    pub fn wait_for_manager(&self, port: u16) -> String {
        let url = format!("https://localhost:{port}/api/v1/session");
        let started = Instant::now();
        loop {
            let response = self.curl(&["--fail", &url]);
            if response.status.success() {
                return stdout_of(&response);
            }
            assert!(
                started.elapsed() < MANAGER_START_TIMEOUT,
                "manager did not answer: {}",
                stderr_of(&docker(&["logs", &self.name]))
            );
            thread::sleep(Duration::from_millis(200));
        }
    }

    /// Runs `command` in this container as the agent user, `dev`.
    pub fn run_as_agent(&self, command: &[&str]) -> Output {
        let mut arguments = vec!["exec", "--user", "dev", &self.name];
        arguments.extend_from_slice(command);
        docker(&arguments)
    }
}

impl Drop for DockerResource {
    fn drop(&mut self) {
        docker(&[self.kind, "rm", "--force", &self.name]);
    }
}

fn unique_name(purpose: &str) -> String {
    format!("ezra-test-{purpose}-{}", std::process::id())
}

/// A temporary host folder of setup scripts, mounted read-only as /etc/ezra/setup.d.
pub struct SetupDirectory {
    directory: tempfile::TempDir,
}

impl SetupDirectory {
    /// Each script is `(file name, mode, contents)`.
    pub fn with_scripts(scripts: &[(&str, u32, &str)]) -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        directory.path().set_mode(0o755);
        for (name, mode, contents) in scripts {
            directory
                .path()
                .join(name)
                .write_with_mode(contents.as_bytes(), *mode);
        }
        Self { directory }
    }

    pub fn volume_option(&self) -> String {
        format!("{}:/etc/ezra/setup.d:ro", self.directory.path().display())
    }
}

/// A temporary host folder for a test to mount where the container's `dev` user writes.
pub fn agent_directory() -> TempDir {
    let directory = tempfile::tempdir().expect("temporary directory");
    assert_eq!(
        directory.path().owner(),
        AGENT_UID,
        "these tests mount host folders that the container's dev user writes, so run them as uid {AGENT_UID}"
    );
    directory
}

pub trait PathExt {
    /// The permission bits.
    fn mode(&self) -> u32;

    /// Sets the permission bits.
    fn set_mode(&self, mode: u32);

    /// The uid that owns the file.
    fn owner(&self) -> u32;

    /// The inode number, which a rename onto the path changes.
    fn inode(&self) -> u64;

    /// Writes the file with `mode`, creating the folders it is in.
    fn write_with_mode(&self, bytes: &[u8], mode: u32);

    /// Names in this folder that contain `part`.
    fn names_containing(&self, part: &str) -> Vec<String>;
}

impl PathExt for Path {
    fn mode(&self) -> u32 {
        fs::metadata(self).expect("the file exists").mode() & 0o7777
    }

    fn set_mode(&self, mode: u32) {
        fs::set_permissions(self, Permissions::from_mode(mode)).expect("the mode is set");
    }

    fn owner(&self) -> u32 {
        fs::metadata(self).expect("the file exists").uid()
    }

    fn inode(&self) -> u64 {
        fs::metadata(self).expect("the file exists").ino()
    }

    fn write_with_mode(&self, bytes: &[u8], mode: u32) {
        fs::create_dir_all(self.parent().expect("the path has a parent"))
            .expect("the folder is created");
        fs::write(self, bytes).expect("the file is written");
        self.set_mode(mode);
    }

    fn names_containing(&self, part: &str) -> Vec<String> {
        fs::read_dir(self)
            .expect("the folder is listed")
            .map(|entry| {
                entry
                    .expect("the entry is read")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.contains(part))
            .collect()
    }
}

/// The manager in a test container, signed in through curl's cookie jar.
pub struct Manager {
    pub container: DockerResource,
}

impl Manager {
    /// Starts a container and sets the manager's password, which signs curl in.
    pub fn start(purpose: &str, docker_options: &[&str]) -> Self {
        let container = DockerResource::start_container(purpose, docker_options, &[]);
        container.wait_for_manager(8443);
        let setup = container.curl(&[
            "--fail-with-body",
            "--cookie-jar",
            MANAGER_COOKIES,
            "--request",
            "POST",
            "--header",
            "Content-Type: application/json",
            "--data-binary",
            r#"{"password":"x"}"#,
            "https://localhost:8443/api/v1/setup",
        ]);
        assert!(setup.status.success(), "{}", stdout_of(&setup));
        Self { container }
    }

    /// Sends a request to the manager API and returns the status code and the JSON body.
    pub fn request(&self, method: &str, path: &str, body: Option<&Value>) -> (String, Value) {
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
            String::from_utf8(self.container.curl(&arguments).stdout).expect("curl prints text");
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

    /// Starts reading the event stream and returns once it is connected.
    pub fn events(&self) -> EventStream {
        let mut curl = Command::new("docker")
            .args([
                "exec",
                &self.container.name,
                "curl",
                "--silent",
                "--insecure",
                "--no-buffer",
                "--max-time",
                "120",
                "--cookie",
                MANAGER_COOKIES,
                "https://localhost:8443/api/v1/events",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("docker can be executed");
        let stdout = curl.stdout.take().expect("the output is piped");
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        let events = EventStream { curl, lines };
        events.wait_for(
            r#""event":"connected""#,
            "the stream's start",
            EVENTS_CONNECT_TIMEOUT,
        );
        events
    }
}

/// The manager's event stream, read line by line from curl in the container.
pub struct EventStream {
    curl: Child,
    lines: Receiver<String>,
}

impl EventStream {
    /// Waits up to `longest` for a line that contains `needle`.
    pub fn wait_for(&self, needle: &str, what: &str, longest: Duration) {
        let started = Instant::now();
        loop {
            let line = self
                .lines
                .recv_timeout(longest.saturating_sub(started.elapsed()))
                .unwrap_or_else(|_| panic!("{what} was not published within {longest:?}"));
            if line.contains(needle) {
                return;
            }
        }
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        let _ = self.curl.kill();
        let _ = self.curl.wait();
    }
}
