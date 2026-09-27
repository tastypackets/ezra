//! Black-box tests of saving the agents' settings files through the manager.
//! Build the image first, then run: `cargo test -- --ignored`

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::time::Duration;

use ezra_container_tests::{
    DockerResource, Manager, PathExt, agent_directory, docker, run_in_image, stderr_of, stdout_of,
};
use serde_json::{Value, json};

const WATCHED_CHANGE_TIMEOUT: Duration = Duration::from_secs(2);
const REREAD_TIMEOUT: Duration = Duration::from_secs(12);
const SETTINGS_FILE_CHANGED: &str = r#""topic":"settings_file""#;
const CLAUDE_TEXT: &str = "{\r\n  \"model\": \"opus\"\r\n}\r\n";
const CODEX_TEXT: &str = "\u{feff}# ezra\r\n[projects]\r\n\"/home/dev/projects/a\" = {\r\n  trust_level = \"trusted\",\r\n}\r\n";

trait ManagerExt {
    fn open(&self, agent: &str) -> Value;
    fn save(&self, agent: &str, text: &str, version: &Value) -> (String, Value);
    fn codex_feature(&self, feature: &str) -> Option<String>;
}

impl ManagerExt for Manager {
    /// The agent's settings file as the manager reads it now.
    fn open(&self, agent: &str) -> Value {
        let (status, file) = self.request(
            "GET",
            &format!("/api/v1/agents/{agent}/settings-file"),
            None,
        );
        assert_eq!(status, "200", "{file}");
        file
    }

    /// Saves `text` over `version` and returns the status code and the JSON body.
    fn save(&self, agent: &str, text: &str, version: &Value) -> (String, Value) {
        self.request(
            "PUT",
            &format!("/api/v1/agents/{agent}/settings-file"),
            Some(&json!({ "text": text, "version": version })),
        )
    }

    /// The state `codex features list` gives `feature`.
    fn codex_feature(&self, feature: &str) -> Option<String> {
        stdout_of(&self.container.run_as_agent(&["codex", "features", "list"]))
            .lines()
            .find(|line| line.split_whitespace().next() == Some(feature))
            .and_then(|line| line.split_whitespace().last())
            .map(str::to_owned)
    }
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn files_mounted_on_their_own_are_saved_in_place() {
    let volume = DockerResource::volume("settings-file-own");
    let config = format!("{}:/config", volume.name);
    stdout_of(&run_in_image(
        &["--volume", &config],
        &["mkdir", "/config/claude", "/config/codex"],
    ));
    let host = agent_directory();
    let settings = host.path().join("settings.json");
    let codex_config = host.path().join("config.toml");
    settings.write_with_mode(CLAUDE_TEXT.as_bytes(), 0o640);
    codex_config.write_with_mode(CODEX_TEXT.as_bytes(), 0o644);
    let (settings_inode, codex_inode) = (settings.inode(), codex_config.inode());
    let settings_mount = format!("{}:/config/claude/settings.json", settings.display());
    let codex_mount = format!("{}:/config/codex/config.toml", codex_config.display());
    let manager = Manager::start(
        "settings-file-own",
        &[
            "--volume",
            &config,
            "--volume",
            &settings_mount,
            "--volume",
            &codex_mount,
        ],
    );

    let claude = manager.open("claude");
    assert_eq!(claude["path"], "/config/claude/settings.json");
    assert_eq!(claude["format"], "json");
    assert_eq!(claude["text"], CLAUDE_TEXT);
    let codex = manager.open("codex");
    assert_eq!(codex["path"], "/config/codex/config.toml");
    assert_eq!(codex["format"], "toml");
    assert_eq!(codex["text"], CODEX_TEXT);

    let renamed = manager.container.run_as_agent(&[
        "sh",
        "-c",
        "printf 'a = 1\\n' > /config/codex/.tmpAbC123 && mv /config/codex/.tmpAbC123 /config/codex/config.toml",
    ]);
    assert!(!renamed.status.success());
    let refused = stderr_of(&renamed);
    assert!(refused.contains("Device or resource busy"), "{refused}");
    stdout_of(
        &manager
            .container
            .run_as_agent(&["rm", "/config/codex/.tmpAbC123"]),
    );

    let events = manager.events();
    let edited = "{\r\n  \"model\": \"haiku\"\r\n}\r\n";
    fs::write(&settings, edited).expect("the file is edited in place");
    events.wait_for(SETTINGS_FILE_CHANGED, "an edit on the host", REREAD_TIMEOUT);
    let (status, conflict) = manager.save("claude", "{}", &claude["version"]);
    assert_eq!(status, "409", "{conflict}");
    let claude = manager.open("claude");
    assert_eq!(claude["text"], edited);

    let (status, problem) = manager.save("claude", "{\"a\":1,}", &claude["version"]);
    assert_eq!(status, "400", "{problem}");
    assert_eq!(
        problem,
        json!({ "error": "trailing comma", "line": 1, "column": 8 })
    );
    let text = "\u{feff}{\r\n\t\"model\":  \"sonnet\"\r\n}";
    let (status, saved) = manager.save("claude", text, &claude["version"]);
    assert_eq!(status, "200", "{saved}");
    assert_eq!(saved["text"], text);
    assert_eq!(
        fs::read(&settings).expect("the file is read"),
        text.as_bytes()
    );
    assert_eq!(settings.inode(), settings_inode);
    assert_eq!(settings.mode(), 0o640);

    let (status, problem) = manager.save(
        "codex",
        "\u{feff}model = \"a\"\r\nmodel = \"b\"\r\n",
        &codex["version"],
    );
    assert_eq!(status, "400", "{problem}");
    assert_eq!(
        problem,
        json!({ "error": "duplicate key", "line": 2, "column": 1 })
    );
    let text = "\u{feff}model =\t\"gpt-5\"\r\nescape = \"\\e\\x41\"\r\nstart = 07:32\r\n[projects]\r\n\"/home/dev/projects/a\" = {\r\n  trust_level = \"trusted\",\r\n}";
    let (status, saved) = manager.save("codex", text, &codex["version"]);
    assert_eq!(status, "200", "{saved}");
    assert_eq!(
        fs::read(&codex_config).expect("the file is read"),
        text.as_bytes()
    );
    assert_eq!(codex_config.inode(), codex_inode);
    assert_eq!(codex_config.mode(), 0o644);
    let (status, conflict) = manager.save("codex", "", &codex["version"]);
    assert_eq!(status, "409", "{conflict}");

    for directory in ["/config/claude", "/config/codex"] {
        let names = stdout_of(&docker(&[
            "exec",
            &manager.container.name,
            "ls",
            "-A",
            directory,
        ]));
        assert!(!names.contains(".ezra"), "{names}");
    }
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn files_in_mounted_directories_are_replaced_and_keep_their_mode() {
    let host = agent_directory();
    let claude_directory = host.path().join("claude");
    let codex_directory = host.path().join("codex");
    let settings = claude_directory.join("settings.json");
    let target = claude_directory.join("dotfiles/settings.json");
    let codex_config = codex_directory.join("config.toml");
    target.write_with_mode(CLAUDE_TEXT.as_bytes(), 0o660);
    symlink("dotfiles/settings.json", &settings).expect("the link is created");
    fs::create_dir(&codex_directory).expect("the directory is created");
    let claude_mount = format!("{}:/config/claude", claude_directory.display());
    let codex_mount = format!("{}:/config/codex", codex_directory.display());
    let manager = Manager::start(
        "settings-file-directories",
        &["--volume", &claude_mount, "--volume", &codex_mount],
    );

    let claude = manager.open("claude");
    assert_eq!(claude["text"], CLAUDE_TEXT);
    let codex = manager.open("codex");
    assert_eq!(codex["text"], "");

    let events = manager.events();
    let by_claude = "{\n  \"model\": \"haiku\"\n}\n";
    let claude_staging = claude_directory.join("dotfiles/.cc-writes/.tmp.4242.9f3a");
    claude_staging.write_with_mode(by_claude.as_bytes(), 0o660);
    fs::rename(&claude_staging, &target).expect("Claude's save lands");
    events.wait_for(
        SETTINGS_FILE_CHANGED,
        "Claude's save through the link",
        WATCHED_CHANGE_TIMEOUT,
    );
    let by_codex = "[projects.\"/home/dev/projects/a\"]\ntrust_level = \"trusted\"\n";
    let codex_staging = codex_directory.join(".tmpAbC123");
    codex_staging.write_with_mode(by_codex.as_bytes(), 0o600);
    fs::rename(&codex_staging, &codex_config).expect("Codex's save lands");
    events.wait_for(
        SETTINGS_FILE_CHANGED,
        "Codex's save of a new file",
        WATCHED_CHANGE_TIMEOUT,
    );

    let (status, conflict) = manager.save("claude", "{}", &claude["version"]);
    assert_eq!(status, "409", "{conflict}");
    let (status, conflict) = manager.save("codex", "", &codex["version"]);
    assert_eq!(status, "409", "{conflict}");
    assert_eq!(
        fs::read(&target).expect("the file is read"),
        by_claude.as_bytes()
    );
    assert_eq!(
        fs::read(&codex_config).expect("the file is read"),
        by_codex.as_bytes()
    );

    let claude = manager.open("claude");
    assert_eq!(claude["text"], by_claude);
    let target_inode = target.inode();
    let text = "\u{feff}{\r\n\t\"model\":  \"sonnet\",\r\n  \"unknown\": [1, {}]\r\n}\r\n\r\n";
    let (status, saved) = manager.save("claude", text, &claude["version"]);
    assert_eq!(status, "200", "{saved}");
    assert_eq!(
        fs::read_link(&settings).expect("the link stays"),
        Path::new("dotfiles/settings.json")
    );
    assert_eq!(
        fs::read(&target).expect("the file is read"),
        text.as_bytes()
    );
    assert_ne!(target.inode(), target_inode);
    assert_eq!(target.mode(), 0o660);

    let codex = manager.open("codex");
    assert_eq!(codex["text"], by_codex);
    codex_config.set_mode(0o664);
    let codex_inode = codex_config.inode();
    let (status, saved) = manager.save("codex", CODEX_TEXT, &codex["version"]);
    assert_eq!(status, "200", "{saved}");
    assert_eq!(
        fs::read(&codex_config).expect("the file is read"),
        CODEX_TEXT.as_bytes()
    );
    assert_ne!(codex_config.inode(), codex_inode);
    assert_eq!(codex_config.mode(), 0o664);

    fs::remove_file(&codex_config).expect("the file is removed");
    let (status, conflict) = manager.save("codex", "a = 1\n", &saved["version"]);
    assert_eq!(status, "409", "{conflict}");
    assert!(!codex_config.exists());
    let codex = manager.open("codex");
    assert_eq!(codex["text"], "");
    let (status, saved) = manager.save("codex", "a = 1\n", &codex["version"]);
    assert_eq!(status, "200", "{saved}");
    assert_eq!(codex_config.mode(), 0o600);

    for directory in [
        &claude_directory,
        &claude_directory.join("dotfiles"),
        &codex_directory,
    ] {
        assert_eq!(directory.names_containing(".ezra"), Vec::<String>::new());
    }
}

#[test]
#[ignore = "needs Docker, network access and a built ezra image"]
fn codex_reads_what_ezra_saves_and_its_own_saves_are_noticed() {
    let host = agent_directory();
    let codex_config = host.path().join("config.toml");
    let target = host.path().join("dotfiles/config.toml");
    let codex_mount = format!("{}:/config/codex", host.path().display());
    let manager = Manager::start("settings-file-codex", &["--volume", &codex_mount]);
    let (status, installed) = manager.request("POST", "/api/v1/agents/codex/install", None);
    assert_eq!(status, "200", "{installed}");
    let codex_version = installed["installed_version"]
        .as_str()
        .expect("Codex is installed")
        .to_owned();
    let with_codex = format!("with Codex {codex_version}");

    let events = manager.events();
    let codex = manager.open("codex");
    let text = format!("{CODEX_TEXT}[features]\r\n\tdaemon_auto_start =  false");
    let (status, saved) = manager.save("codex", &text, &codex["version"]);
    assert_eq!(status, "200", "{saved}");
    assert_eq!(
        fs::read(&codex_config).expect("the file is read"),
        text.as_bytes()
    );
    events.wait_for(SETTINGS_FILE_CHANGED, "ezra's save", WATCHED_CHANGE_TIMEOUT);
    events.wait_for(
        SETTINGS_FILE_CHANGED,
        "ezra's save seen on disk",
        WATCHED_CHANGE_TIMEOUT,
    );
    assert_eq!(
        manager.codex_feature("daemon_auto_start").as_deref(),
        Some("false"),
        "{with_codex}"
    );

    stdout_of(&manager.container.run_as_agent(&[
        "codex",
        "features",
        "enable",
        "daemon_auto_start",
    ]));
    events.wait_for(
        SETTINGS_FILE_CHANGED,
        &format!("Codex's own save {with_codex}"),
        WATCHED_CHANGE_TIMEOUT,
    );
    let (status, conflict) = manager.save("codex", &text, &saved["version"]);
    assert_eq!(status, "409", "{with_codex}: {conflict}");
    let codex = manager.open("codex");
    let by_codex = fs::read_to_string(&codex_config).expect("the file is read");
    assert_eq!(codex["text"], by_codex);
    assert_eq!(
        manager.codex_feature("daemon_auto_start").as_deref(),
        Some("true"),
        "{with_codex}"
    );

    target.write_with_mode(by_codex.as_bytes(), 0o640);
    let link = host.path().join("config.toml.link");
    symlink("dotfiles/config.toml", &link).expect("the link is created");
    fs::rename(&link, &codex_config).expect("the link replaces the file");
    stdout_of(&manager.container.run_as_agent(&[
        "codex",
        "features",
        "disable",
        "daemon_auto_start",
    ]));
    events.wait_for(
        SETTINGS_FILE_CHANGED,
        &format!("Codex's own save through a link {with_codex}"),
        WATCHED_CHANGE_TIMEOUT,
    );
    let (status, conflict) = manager.save("codex", &text, &codex["version"]);
    assert_eq!(status, "409", "{with_codex}: {conflict}");
    let codex = manager.open("codex");
    assert_eq!(
        codex["text"],
        fs::read_to_string(&target).expect("the file is read"),
        "{with_codex}"
    );
    assert_eq!(
        manager.codex_feature("daemon_auto_start").as_deref(),
        Some("false"),
        "{with_codex}"
    );

    target.set_mode(0o660);
    let text = text.replace("false", "true");
    let (status, saved) = manager.save("codex", &text, &codex["version"]);
    assert_eq!(status, "200", "{saved}");
    assert_eq!(
        fs::read_link(&codex_config).expect("the link stays"),
        Path::new("dotfiles/config.toml")
    );
    assert_eq!(
        fs::read(&target).expect("the file is read"),
        text.as_bytes()
    );
    assert_eq!(target.mode(), 0o660);
    assert_eq!(
        manager.codex_feature("daemon_auto_start").as_deref(),
        Some("true"),
        "{with_codex}"
    );
}

#[test]
#[ignore = "needs Docker and a built ezra image"]
fn missing_files_and_their_folders_are_created_for_the_agent_user() {
    let manager = Manager::start("settings-file-missing", &[]);
    for (agent, directory, text) in [
        ("claude", "/config/claude", "{}"),
        ("codex", "/config/codex", "a = 1\n"),
    ] {
        assert!(
            !docker(&["exec", &manager.container.name, "test", "-e", directory])
                .status
                .success(),
            "{directory} exists before the save"
        );
        let missing = manager.open(agent);
        assert_eq!(missing["text"], "");
        let (status, saved) = manager.save(agent, text, &missing["version"]);
        assert_eq!(status, "200", "{saved}");
        let created = stdout_of(&docker(&[
            "exec",
            &manager.container.name,
            "stat",
            "--format",
            "%n %a %U",
            directory,
            saved["path"].as_str().expect("the path is text"),
        ]));
        assert_eq!(
            created,
            format!(
                "{directory} 755 dev\n{} 600 dev",
                saved["path"].as_str().expect("the path is text")
            )
        );
    }
}
