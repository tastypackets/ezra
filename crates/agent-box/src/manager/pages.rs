use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum_extra::extract::cookie::CookieJar;
use maud::{DOCTYPE, Markup, html};

use super::agents::Agent;
use super::auth::SESSION_COOKIE;
use super::login::LoginPrompt;
use super::state::AppState;
use super::status::{self, AgentStatus};

const SCRIPT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../ui/dist/app.js"));
const STYLESHEET: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../ui/dist/app.css"
));

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/assets/app.js", get(script))
        .route("/assets/app.css", get(stylesheet))
        .with_state(state)
}

async fn script() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        SCRIPT,
    )
}

async fn stylesheet() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        STYLESHEET,
    )
}

/// One page with three states: choose a password, sign in, or the dashboard.
async fn home(State(state): State<AppState>, cookies: CookieJar) -> Markup {
    let is_claimed = state.settings.lock().await.manager.password_hash.is_some();
    let is_signed_in = state.is_session(cookies.get(SESSION_COOKIE).map(|cookie| cookie.value()));
    let content = match (is_claimed, is_signed_in) {
        (false, _) => choose_password(),
        (true, false) => sign_in(),
        (true, true) => dashboard(&status::all_agent_statuses(&state).await),
    };
    page(is_signed_in && is_claimed, content)
}

fn page(show_sign_out: bool, content: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "agent-box" }
                link rel="stylesheet" href="assets/app.css";
                script type="module" src="assets/app.js" {}
            }
            body {
                div.shell {
                    header.topbar {
                        div {
                            h1 { "agent-box" }
                            p.muted { "Agent manager" }
                        }
                        @if show_sign_out {
                            form data-api="logout" {
                                button.button.secondary type="submit" { "Sign out" }
                            }
                        }
                    }
                    main { (content) }
                }
            }
        }
    }
}

fn choose_password() -> Markup {
    password_card(
        "Set a password",
        "No password is set yet. Whoever sets it first controls this manager.",
        "setup",
        "new-password",
        ("Set password", "Saving…"),
    )
}

fn sign_in() -> Markup {
    password_card(
        "Sign in",
        "Enter the manager password.",
        "login",
        "current-password",
        ("Sign in", "Signing in…"),
    )
}

fn password_card(
    title: &str,
    description: &str,
    api_path: &str,
    autocomplete: &str,
    (label, busy_label): (&str, &str),
) -> Markup {
    html! {
        div.card.narrow {
            h2 { (title) }
            p.muted { (description) }
            form.form data-api=(api_path) data-busy=(busy_label) {
                label.field {
                    span { "Password" }
                    input type="password" name="password" autocomplete=(autocomplete) required autofocus;
                }
                button.button.primary type="submit" { (label) }
                p.error data-error hidden {}
            }
        }
    }
}

fn dashboard(statuses: &[AgentStatus]) -> Markup {
    html! {
        section.card.flush {
            div.card-header {
                h2 { "Agents" }
                p.muted { "Installed command-line agents and their sign-in state." }
            }
            div.table-scroll {
                table {
                    thead {
                        tr {
                            th { "Agent" }
                            th { "Status" }
                            th { "Version" }
                            th { "Account" }
                            th.number { "Sessions" }
                            th.number title="Sign-in, settings and sessions the CLI keeps on /config. The CLI itself is not counted." { "Saved data" }
                            th.number { span.visually-hidden { "Actions" } }
                        }
                    }
                    tbody {
                        @for status in statuses {
                            (agent_row(status))
                        }
                    }
                }
            }
        }
        @for status in statuses {
            @if let Some(prompt) = &status.login_prompt {
                (sign_in_panel(status.agent, prompt))
            }
        }
    }
}

fn agent_row(status: &AgentStatus) -> Markup {
    let agent = status.agent;
    let is_installed = status.installed_version.is_some();
    let (state_label, badge) = match (
        is_installed,
        status.logged_in,
        status.login_prompt.is_some(),
    ) {
        (false, _, _) => ("Not installed", "neutral"),
        (true, true, _) => ("Signed in", "good"),
        (true, false, true) => ("Signing in", "pending"),
        (true, false, false) => ("Signed out", "neutral"),
    };
    html! {
        tr {
            td.strong { (display_name(agent)) }
            td { span.badge.(badge) { (state_label) } }
            td.mono[is_installed] { (status.installed_version.as_deref().unwrap_or("—")) }
            td { (status.account.as_deref().unwrap_or(if status.logged_in { "unavailable" } else { "—" })) }
            td.number { (status.session_count.map_or("unavailable".to_owned(), |count| count.to_string())) }
            td.number { (status.config_disk_bytes.map_or("unavailable".to_owned(), human_bytes)) }
            td.number {
                div.row-actions {
                    @if is_installed {
                        (action(agent, "install", "Update", "Updating…", false))
                    } @else {
                        (action(agent, "install", "Install", "Installing…", true))
                    }
                    @if is_installed && status.logged_in {
                        (action(agent, "logout", "Sign out", "Signing out…", false))
                    } @else if is_installed && status.login_prompt.is_none() {
                        (action(agent, "login", "Sign in", "Starting…", true))
                    }
                }
            }
        }
    }
}

fn action(agent: Agent, name: &str, label: &str, busy_label: &str, primary: bool) -> Markup {
    html! {
        form data-api={ "agents/" (agent) "/" (name) } data-busy=(busy_label) {
            button.button.small.primary[primary].secondary[!primary] type="submit" { (label) }
            p.error data-error hidden {}
        }
    }
}

fn sign_in_panel(agent: Agent, prompt: &LoginPrompt) -> Markup {
    let site = prompt
        .url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("the sign-in page");
    html! {
        section.card.flush {
            div.card-header {
                h2 { "Sign in to " (display_name(agent)) }
                p.muted { "Finish these steps in any browser." }
            }
            ol.steps {
                li {
                    span.step-number { "1" }
                    div {
                        p.step-title { "Open the sign-in page" }
                        a.button.secondary.small href=(prompt.url) target="_blank" rel="noopener" { "Open " (site) " ↗" }
                    }
                }
                @match (agent, &prompt.code) {
                    (Agent::Codex, Some(code)) => {
                        li {
                            span.step-number { "2" }
                            div {
                                p.step-title { "Enter this code" }
                                div.code-row {
                                    span.code { (code) }
                                    button.button.secondary.small type="button" data-copy=(code) { "Copy" }
                                }
                            }
                        }
                    }
                    _ => {
                        li {
                            span.step-number { "2" }
                            div {
                                p.step-title { "Paste the code shown after you approve" }
                                form.inline-form data-api={ "agents/" (agent) "/login/code" } data-busy="Checking…" {
                                    input type="text" name="code" autocomplete="off" spellcheck="false" required aria-label="Code" placeholder="Code";
                                    button.button.primary.small type="submit" { "Finish sign-in" }
                                    p.error data-error hidden {}
                                }
                            }
                        }
                    }
                }
            }
            div.card-footer {
                @if agent == Agent::Codex {
                    p.waiting data-waiting-for=(agent) {
                        span.spinner aria-hidden="true" {}
                        "Waiting for you to finish on the website…"
                    }
                } @else {
                    span {}
                }
                form data-api={ "agents/" (agent) "/login" } data-busy="Restarting…" {
                    button.button.secondary.small type="submit" { "Start over" }
                    p.error data-error hidden {}
                }
            }
        }
    }
}

fn display_name(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "Claude Code",
        Agent::Codex => "Codex",
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = UNITS[0];
    for candidate in UNITS {
        value /= 1000.0;
        unit = candidate;
        if value < 1000.0 {
            break;
        }
    }
    format!("{value:.1} {unit}")
}

#[cfg(test)]
mod tests {
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::manager::agents::InstallPaths;
    use crate::manager::settings::Settings;

    #[test]
    fn byte_counts_read_naturally() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1_500), "1.5 KB");
        assert_eq!(human_bytes(231_000_000), "231.0 MB");
        assert_eq!(human_bytes(4_200_000_000), "4.2 GB");
    }

    async fn home_page(settings: Settings, cookie: Option<&str>) -> String {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::new(
            directory.path().join("settings.toml"),
            settings,
            InstallPaths::under_home(directory.path()),
        );
        let mut request = Request::get("/");
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        let response = router(state)
            .oneshot(request.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(body.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn unclaimed_manager_asks_for_a_password() {
        let page = home_page(Settings::default(), None).await;
        assert!(page.contains("Set a password"));
        assert!(page.contains(r#"data-api="setup""#));
    }

    #[tokio::test]
    async fn claimed_manager_asks_to_sign_in() {
        let mut settings = Settings::default();
        settings.manager.password_hash = Some("$argon2id$example".to_owned());
        let page = home_page(settings, Some("session=made-up")).await;
        assert!(page.contains(r#"data-api="login""#));
        assert!(!page.contains("Claude Code"));
    }

    #[tokio::test]
    async fn assets_are_served() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::new(
            directory.path().join("settings.toml"),
            Settings::default(),
            InstallPaths::under_home(directory.path()),
        );
        for (path, content_type) in [
            ("/assets/app.js", "text/javascript; charset=utf-8"),
            ("/assets/app.css", "text/css; charset=utf-8"),
        ] {
            let response = router(state.clone())
                .oneshot(Request::get(path).body(axum::body::Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);
        }
    }
}
