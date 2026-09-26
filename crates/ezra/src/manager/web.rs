use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum_extra::extract::cookie::CookieJar;
use serde::Serialize;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

use super::api::session::SessionStatus;
use super::state::AppState;
use super::status::AgentStatus;

const IMMUTABLE: HeaderValue = HeaderValue::from_static("public, max-age=31536000, immutable");

/// What the first screen needs, written into `index.html` so the page never shows a loading state.
#[derive(Debug, Serialize)]
struct InitialData {
    session: SessionStatus,
    /// Present only when the request is signed in.
    agents: Option<Vec<AgentStatus>>,
}

#[derive(Clone)]
struct WebState {
    app: AppState,
    directory: Arc<PathBuf>,
}

/// Serves the built web app from `directory`: hashed assets as immutable files, every other
/// path as `index.html` with the initial data inserted.
pub fn router(app: AppState, directory: PathBuf) -> Router {
    let state = WebState {
        app,
        directory: Arc::new(directory),
    };
    let assets = ServeDir::new(state.directory.join("assets"));
    let other_files = ServeDir::new(state.directory.as_path())
        .append_index_html_on_directories(false)
        .fallback(get(index).with_state(state.clone()));
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .nest(
            "/assets",
            Router::new()
                .fallback_service(assets)
                .layer(SetResponseHeaderLayer::overriding(
                    header::CACHE_CONTROL,
                    IMMUTABLE,
                )),
        )
        .fallback_service(other_files)
        .with_state(state)
}

async fn index(State(state): State<WebState>, cookies: CookieJar) -> Response {
    let page = match tokio::fs::read_to_string(state.directory.join("index.html")).await {
        Ok(page) => page,
        Err(error) => return web_app_missing(&state.directory, &error),
    };
    let session = SessionStatus::of(&state.app, &cookies).await;
    let agents = if session.authenticated {
        Some(AgentStatus::gather_all(&state.app).await)
    } else {
        None
    };
    let initial_data = InitialData { session, agents };
    match serde_json::to_string(&initial_data) {
        Ok(json) => (
            [(header::CACHE_CONTROL, "no-cache")],
            axum::response::Html(with_initial_data(&page, &json)),
        )
            .into_response(),
        Err(error) => {
            tracing::error!("could not serialize the initial page data: {error}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Inserts the data before `</head>`. `<` is escaped so the data can never close the script tag.
fn with_initial_data(page: &str, json: &str) -> String {
    let script = format!(
        r#"<script id="initial-data" type="application/json">{}</script>"#,
        json.replace('<', "\\u003c")
    );
    match page.split_once("</head>") {
        Some((head, rest)) => format!("{head}{script}</head>{rest}"),
        None => format!("{script}{page}"),
    }
}

fn web_app_missing(directory: &Path, error: &io::Error) -> Response {
    tracing::error!(
        "could not read the web app in {} ({error})",
        directory.display()
    );
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "The manager's web app is missing from this image.",
    )
        .into_response()
}
