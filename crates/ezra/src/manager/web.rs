use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum_extra::extract::cookie::CookieJar;
use serde::Serialize;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

use super::api::session::SessionStatus;
use super::state::AppState;
use super::status::AgentStatus;

const IMMUTABLE: HeaderValue = HeaderValue::from_static("public, max-age=31536000, immutable");
const NO_CACHE: HeaderValue = HeaderValue::from_static("no-cache");

/// What the first screen needs, written into `index.html`.
#[derive(Debug, Serialize)]
struct InitialData {
    session: SessionStatus,
    /// Present only when the request is signed in.
    agents: Option<Vec<AgentStatus>>,
}

impl InitialData {
    async fn gather(app: &AppState, cookies: &CookieJar) -> Self {
        let session = SessionStatus::of(app, cookies).await;
        let agents = if session.authenticated {
            Some(AgentStatus::gather_all(app).await)
        } else {
            None
        };
        Self { session, agents }
    }

    /// `<` is escaped so the data can never close its script tag.
    fn insert_into(&self, page: &str) -> Result<String, serde_json::Error> {
        let json = serde_json::to_string(self)?.replace('<', "\\u003c");
        let script =
            format!(r#"<script id="initial-data" type="application/json">{json}</script>"#);
        Ok(match page.split_once("</head>") {
            Some((head, rest)) => format!("{head}{script}</head>{rest}"),
            None => format!("{page}{script}"),
        })
    }
}

#[derive(Debug, thiserror::Error)]
enum WebError {
    #[error("could not read the web app in {}: {source}", directory.display())]
    Missing {
        directory: PathBuf,
        source: io::Error,
    },
    #[error("could not serialize the initial page data: {0}")]
    Serialize(#[from] serde_json::Error),
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        tracing::error!("{self}");
        match self {
            Self::Missing { .. } => (
                StatusCode::SERVICE_UNAVAILABLE,
                "The manager's web app is missing from this image.",
            )
                .into_response(),
            Self::Serialize(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
}

#[derive(Clone)]
struct WebState {
    app: AppState,
    directory: Arc<PathBuf>,
}

/// Serves the built web app from `directory`: hashed assets as immutable files, client routes
/// as `index.html` with the initial data inserted.
pub fn router(app: AppState, directory: PathBuf) -> Router {
    let state = WebState {
        app,
        directory: Arc::new(directory),
    };
    let assets = ServeDir::new(state.directory.join("assets"));
    let other_files = ServeDir::new(state.directory.as_path())
        .append_index_html_on_directories(false)
        .fallback(get(client_route).with_state(state.clone()));
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .nest(
            "/assets",
            Router::new()
                .fallback_service(assets)
                .layer(SetResponseHeaderLayer::overriding(
                    header::CACHE_CONTROL,
                    |response: &Response| {
                        let status = response.status();
                        (status.is_success() || status == StatusCode::NOT_MODIFIED)
                            .then_some(IMMUTABLE)
                    },
                )),
        )
        .fallback_service(other_files)
        .with_state(state)
}

async fn index(State(state): State<WebState>, cookies: CookieJar) -> Result<Response, WebError> {
    let page = tokio::fs::read_to_string(state.directory.join("index.html"))
        .await
        .map_err(|source| WebError::Missing {
            directory: state.directory.to_path_buf(),
            source,
        })?;
    let page = InitialData::gather(&state.app, &cookies)
        .await
        .insert_into(&page)?;
    Ok(([(header::CACHE_CONTROL, NO_CACHE)], Html(page)).into_response())
}

/// Paths that look like files are missing files, not pages.
async fn client_route(
    state: State<WebState>,
    cookies: CookieJar,
    uri: Uri,
) -> Result<Response, WebError> {
    if Path::new(uri.path()).extension().is_some() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    index(state, cookies).await
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{HeaderMap, Request};
    use axum_extra::extract::cookie::Cookie;

    use super::*;
    use crate::manager::agents::Agent;
    use crate::manager::api::test_support::{ResponseExt, TestManager};
    use crate::manager::auth::SESSION_COOKIE;

    fn cache_control(headers: &HeaderMap) -> Option<&str> {
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
    }

    fn session_cookie(token: &str) -> CookieJar {
        CookieJar::new().add(Cookie::new(SESSION_COOKIE, token.to_owned()))
    }

    #[tokio::test]
    async fn agent_data_needs_a_signed_in_session() {
        let manager = TestManager::new();
        let unclaimed = InitialData::gather(&manager.state, &CookieJar::new()).await;
        assert_eq!(
            unclaimed.session,
            SessionStatus {
                claimed: false,
                authenticated: false
            }
        );
        assert!(unclaimed.agents.is_none());

        let cookie = manager.logged_in().await;
        let made_up = InitialData::gather(&manager.state, &session_cookie("made-up")).await;
        assert!(!made_up.session.authenticated);
        assert!(made_up.agents.is_none());

        let (_, token) = cookie.split_once('=').expect("cookie is name=value");
        let signed_in = InitialData::gather(&manager.state, &session_cookie(token)).await;
        assert!(signed_in.session.authenticated);
        assert_eq!(signed_in.agents.map(|agents| agents.len()), Some(2));
    }

    #[test]
    fn data_cannot_close_its_script_tag() {
        const OPENING_TAG: &str = r#"<script id="initial-data" type="application/json">"#;
        let data = InitialData {
            session: SessionStatus {
                claimed: true,
                authenticated: true,
            },
            agents: Some(vec![AgentStatus {
                agent: Agent::Claude,
                configured: true,
                installed_version: Some("1.0.0".to_owned()),
                logged_in: true,
                account: Some("</script><script>alert(1)</script><!--".to_owned()),
                login_prompt: None,
                session_count: None,
                config_disk_bytes: None,
                install_progress: None,
            }]),
        };
        let page = data
            .insert_into("<!doctype html><html><head></head><body></body></html>")
            .expect("data serializes");
        let (_, script_and_rest) = page.split_once(OPENING_TAG).expect("script is inserted");
        let (json, rest) = script_and_rest
            .split_once("</script>")
            .expect("script is closed");
        assert_eq!(rest, "</head><body></body></html>");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(json).expect("script holds JSON"),
            serde_json::to_value(&data).expect("data converts")
        );
    }

    #[tokio::test]
    async fn client_routes_get_the_page_uncached() {
        let manager = TestManager::new();
        for path in ["/", "/index.html", "/login", "/agents/claude"] {
            let response = manager.get(path, None).await;
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(
                cache_control(response.headers()),
                Some("no-cache"),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn missing_files_are_not_found() {
        let manager = TestManager::new();
        for path in [
            "/favicon.ico",
            "/apple-touch-icon.png",
            "/assets/gone-1a2b.js",
        ] {
            let response = manager.get(path, None).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            assert_eq!(cache_control(response.headers()), None, "{path}");
        }
    }

    #[tokio::test]
    async fn assets_are_cached_for_good() {
        let manager = TestManager::new();
        let response = manager.get("/assets/app-1a2b.js", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(cache_control(response.headers()), IMMUTABLE.to_str().ok());
        let etag = response
            .headers()
            .get(header::ETAG)
            .and_then(|value| value.to_str().ok())
            .expect("assets have an ETag");
        let revalidated = manager
            .send(
                Request::get("/assets/app-1a2b.js")
                    .header(header::IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await;
        assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            cache_control(revalidated.headers()),
            IMMUTABLE.to_str().ok()
        );
    }

    #[tokio::test]
    async fn unknown_api_paths_are_json_not_found() {
        let manager = TestManager::new();
        for path in ["/api", "/api/", "/api/v1/typo", "/api/v1/agents/"] {
            let response = manager.get(path, None).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            let body: serde_json::Value = response.json().await;
            assert!(body["error"].is_string(), "{path}");
        }
    }

    #[tokio::test]
    async fn missing_web_app_is_reported() {
        let manager = TestManager::without_web_app();
        let response = manager.get("/", None).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
