use std::fmt;

use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum_extra::extract::cookie::CookieJar;
use maud::{DOCTYPE, Markup, html};

use super::agents::{Agent, DownloadProgress};
use super::auth::SESSION_COOKIE;
use super::login::LoginPrompt;
use super::state::AppState;
use super::status::AgentStatus;

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
        (true, true) => dashboard(&AgentStatus::gather_all(&state).await),
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
                title { "EZ Remote Agent" }
                link rel="stylesheet" href="assets/app.css";
                script type="module" src="assets/app.js" {}
            }
            body {
                div.shell {
                    header.topbar {
                        div {
                            h1 { "EZ Remote Agent" }
                            p.muted { "Agent manager" }
                        }
                        @if show_sign_out {
                            form data-api="logout" {
                                (ButtonStyle::Secondary.submit("Sign out", ButtonSize::Regular))
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
        "Set password",
    )
}

fn sign_in() -> Markup {
    password_card(
        "Sign in",
        "Enter the manager password.",
        "login",
        "current-password",
        "Sign in",
    )
}

fn password_card(
    title: &str,
    description: &str,
    api_path: &str,
    autocomplete: &str,
    label: &str,
) -> Markup {
    html! {
        div.card.narrow {
            h2 { (title) }
            p.muted { (description) }
            form.form data-api=(api_path) {
                label.field {
                    span { "Password" }
                    input type="password" name="password" autocomplete=(autocomplete) required autofocus;
                }
                (ButtonStyle::Primary.submit(label, ButtonSize::Regular))
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
                            th.number title="Sign-in, settings and sessions the CLI keeps on /config, not the CLI itself" { "Saved data" }
                            th.number { span.visually-hidden { "Actions" } }
                        }
                    }
                    tbody {
                        @for status in statuses {
                            (status.table_row())
                        }
                    }
                }
            }
        }
        @for status in statuses {
            @if let Some(prompt) = &status.login_prompt {
                (prompt.sign_in_panel(status.agent))
            }
        }
    }
}

impl AgentStatus {
    fn state_badge(&self) -> (&'static str, &'static str) {
        match (
            self.installed_version.is_some(),
            self.logged_in,
            self.login_prompt.is_some(),
        ) {
            (false, _, _) => ("Not installed", "neutral"),
            (true, true, _) => ("Signed in", "good"),
            (true, false, true) => ("Signing in", "pending"),
            (true, false, false) => ("Signed out", "neutral"),
        }
    }

    fn table_row(&self) -> Markup {
        let agent = self.agent;
        let is_installed = self.installed_version.is_some();
        let (state_label, badge) = self.state_badge();
        html! {
            tr {
                td.strong { (agent.display_name()) }
                td { span.badge.(badge) { (state_label) } }
                td.mono[is_installed] { (self.installed_version.as_deref().unwrap_or("—")) }
                td { (self.account.as_deref().unwrap_or(if self.logged_in { "unavailable" } else { "—" })) }
                td.number { (self.session_count.map_or("unavailable".to_owned(), |count| count.to_string())) }
                td.number { (self.config_disk_bytes.map_or("unavailable".to_owned(), |bytes| ByteSize(bytes).to_string())) }
                td.number {
                    div.row-actions {
                        @if let Some(progress) = self.install_progress {
                            (agent.install_in_progress(progress))
                        } @else if is_installed {
                            (agent.action_form("install", "Update", ButtonStyle::Secondary))
                        } @else {
                            (agent.action_form("install", "Install", ButtonStyle::Primary))
                        }
                        @if is_installed && self.logged_in {
                            (agent.action_form("logout", "Sign out", ButtonStyle::Secondary))
                        } @else if is_installed && self.login_prompt.is_none() {
                            (agent.action_form("login", "Sign in", ButtonStyle::Primary))
                        }
                    }
                }
            }
        }
    }
}

impl Agent {
    fn action_form(self, action: &str, label: &str, style: ButtonStyle) -> Markup {
        html! {
            form data-api={ "agents/" (self) "/" (action) } data-progress-for=[(action == "install").then_some(self)] {
                (style.submit(label, ButtonSize::Small))
                p.error data-error hidden {}
            }
        }
    }

    /// An install running when the page was rendered, for example a reinstall at startup.
    fn install_in_progress(self, progress: DownloadProgress) -> Markup {
        let label = progress
            .percent()
            .map_or_else(|| "Installing".to_owned(), |percent| format!("{percent}%"));
        html! {
            button.button.small.secondary type="button" disabled aria-busy="true" data-install-running=(self) {
                span.button-spinner aria-hidden="true" {}
                span.button-label { (label) }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ButtonStyle {
    Primary,
    Secondary,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ButtonSize {
    Regular,
    Small,
}

impl ButtonStyle {
    /// A submit button that shows a spinner while its form is busy.
    fn submit(self, label: &str, size: ButtonSize) -> Markup {
        let primary = matches!(self, Self::Primary);
        html! {
            button.button.primary[primary].secondary[!primary].small[size == ButtonSize::Small] type="submit" {
                span.button-spinner aria-hidden="true" {}
                span.button-label { (label) }
            }
        }
    }
}

impl LoginPrompt {
    /// The host the sign-in link points at, for the button label.
    fn site(&self) -> &str {
        self.url
            .split_once("://")
            .and_then(|(_scheme, rest)| rest.split('/').next())
            .unwrap_or("the sign-in page")
    }

    fn sign_in_panel(&self, agent: Agent) -> Markup {
        html! {
            section.card.flush {
                div.card-header {
                    h2 { "Sign in to " (agent.display_name()) }
                    p.muted { "Finish these steps in any browser." }
                }
                ol.steps {
                    li {
                        span.step-number { "1" }
                        div {
                            p.step-title { "Open the sign-in page" }
                            a.button.secondary.small href=(self.url) target="_blank" rel="noopener" { "Open " (self.site()) " ↗" }
                        }
                    }
                    @match (agent, &self.code) {
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
                                    form.inline-form data-api={ "agents/" (agent) "/login/code" } {
                                        input type="text" name="code" autocomplete="off" spellcheck="false" required aria-label="Code" placeholder="Code";
                                        (ButtonStyle::Primary.submit("Finish sign-in", ButtonSize::Small))
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
                            "Waiting for you to finish on the website"
                        }
                    } @else {
                        span {}
                    }
                    form data-api={ "agents/" (agent) "/login" } {
                        (ButtonStyle::Secondary.submit("Start over", ButtonSize::Small))
                        p.error data-error hidden {}
                    }
                }
            }
        }
    }
}

/// A byte count shown in decimal units, e.g. `231.0 MB`.
struct ByteSize(u64);

impl fmt::Display for ByteSize {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
        if self.0 < 1000 {
            return write!(formatter, "{} B", self.0);
        }
        let mut value = self.0 as f64;
        let mut unit = "B";
        for candidate in UNITS {
            value /= 1000.0;
            unit = candidate;
            if value < 1000.0 {
                break;
            }
        }
        write!(formatter, "{value:.1} {unit}")
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::manager::agents::{InstallPaths, TlsVerification};
    use crate::manager::auth::HashedPassword;
    use crate::manager::settings::Settings;

    #[test]
    fn byte_counts_read_naturally() {
        for (bytes, shown) in [
            (0, "0 B"),
            (999, "999 B"),
            (1_500, "1.5 KB"),
            (231_000_000, "231.0 MB"),
            (4_200_000_000, "4.2 GB"),
        ] {
            assert_eq!(ByteSize(bytes).to_string(), shown);
        }
    }

    async fn home_page(settings: Settings, cookie: Option<&str>) -> String {
        let directory = tempfile::tempdir().expect("temporary directory");
        let state = AppState::new(
            directory.path().join("settings.toml"),
            settings,
            InstallPaths::under_home(directory.path()),
            TlsVerification::default(),
        );
        let mut request = Request::get("/");
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        let response = router(state)
            .oneshot(request.body(Body::empty()).expect("request builds"))
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body is readable")
            .to_bytes();
        String::from_utf8(body.to_vec()).expect("page is UTF-8")
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
        settings.manager.password_hash =
            Some(HashedPassword::from_password("correct horse").expect("password hashes"));
        let page = home_page(settings, Some("session=made-up")).await;
        assert!(page.contains(r#"data-api="login""#));
        assert!(!page.contains("Claude Code"));
    }

    #[tokio::test]
    async fn assets_are_served() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let state = AppState::new(
            directory.path().join("settings.toml"),
            Settings::default(),
            InstallPaths::under_home(directory.path()),
            TlsVerification::default(),
        );
        for (path, content_type) in [
            ("/assets/app.js", "text/javascript; charset=utf-8"),
            ("/assets/app.css", "text/css; charset=utf-8"),
        ] {
            let response = router(state.clone())
                .oneshot(
                    Request::get(path)
                        .body(Body::empty())
                        .expect("request builds"),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);
        }
    }
}
