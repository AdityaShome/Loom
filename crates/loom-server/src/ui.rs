//! Serves the embedded web UI (built from `ui/` into `ui/dist`).

use axum::{
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../ui/dist"]
struct Assets;

/// Exact asset match, else `index.html` so client-side routes work.
pub fn serve(uri: &Uri) -> Option<Response> {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    // Don't mask missing API/asset calls with the app shell.
    let file = Assets::get(path).or_else(|| {
        (!path.starts_with("api/") && !path.starts_with("v1/") && !path.contains('.'))
            .then(|| Assets::get("index.html"))
            .flatten()
    })?;
    let mime = file.metadata.mimetype().to_string();
    Some(([(header::CONTENT_TYPE, mime)], file.data.into_owned()).into_response())
}

pub fn not_built() -> Response {
    (StatusCode::NOT_FOUND, "loom: web UI not built. Run `npm install && npm run build` in ui/ and rebuild.")
        .into_response()
}
