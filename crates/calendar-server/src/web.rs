//! Only build-time allowlisted public files are reachable; no runtime filesystem access.
use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

pub fn router() -> Router {
    Router::new()
        .route("/admin/", get(index))
        .route("/admin/index.html", get(index))
        .route("/admin/assets/{name}", get(asset))
}
async fn index() -> Response {
    file("index.html")
}
async fn asset(Path(name): Path<String>) -> Response {
    file(&format!("assets/{name}"))
}
fn file(name: &str) -> Response {
    let Some((_, mime, bytes)) = FILES.iter().find(|(path, _, _)| *path == name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            ("content-type", *mime),
            ("cache-control", if name == "index.html" { "no-store" } else { "public, max-age=31536000, immutable" }),
            ("x-content-type-options", "nosniff"),
            ("referrer-policy", "no-referrer"),
            ("x-frame-options", "DENY"),
            ("content-security-policy", "default-src 'self'; script-src 'self' https://challenges.cloudflare.com; style-src 'self'; connect-src 'self'; frame-src https://challenges.cloudflare.com; img-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"),
        ],
        *bytes,
    ).into_response()
}
