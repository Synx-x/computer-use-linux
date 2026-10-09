//! Serves the PWA from `web/`.
//!
//! Debug builds read `web/` from disk, so the PWA reloads without a rebuild.
//! Release builds embed it when `web/index.html` existed at build time. With
//! no PWA at all, `/` answers with a short placeholder page.

use axum::body::Body;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use std::path::{Path, PathBuf};

#[cfg(all(not(debug_assertions), cul_web))]
static EMBEDDED: include_dir::Dir<'static> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/web");

const PLACEHOLDER: &str = "<!doctype html><meta charset=utf-8><title>Agent desktops</title>\
<p>The agent desktops daemon is running. The PWA is not built into this binary. \
The API lives under <code>/api/desktops</code>.</p>";

#[derive(Clone, Debug)]
pub enum Web {
    Dir(PathBuf),
    Embedded,
    Placeholder,
}

impl Web {
    /// Pick the PWA source. `override_dir` comes from `--web-dir`.
    pub fn pick(override_dir: Option<PathBuf>) -> Web {
        if let Some(dir) = override_dir {
            return Web::Dir(dir);
        }
        if cfg!(debug_assertions) {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
            if dir.join("index.html").is_file() {
                return Web::Dir(dir);
            }
        }
        if cfg!(all(not(debug_assertions), cul_web)) {
            return Web::Embedded;
        }
        Web::Placeholder
    }

    pub fn describe(&self) -> String {
        match self {
            Web::Dir(dir) => format!("web/ from {}", dir.display()),
            Web::Embedded => "embedded web/".to_string(),
            Web::Placeholder => "placeholder page (no web/ found)".to_string(),
        }
    }

    pub async fn serve(&self, uri: Uri) -> Response {
        let path = uri.path().trim_start_matches('/');
        let path = if path.is_empty() { "index.html" } else { path };
        if path
            .split('/')
            .any(|part| part == ".." || part.starts_with('.'))
        {
            return StatusCode::NOT_FOUND.into_response();
        }
        match self {
            Web::Dir(dir) => match tokio::fs::read(dir.join(path)).await {
                Ok(bytes) => file_response(path, bytes),
                Err(_) => {
                    not_found(path, || async {
                        tokio::fs::read(dir.join("index.html")).await.ok()
                    })
                    .await
                }
            },
            Web::Embedded => embedded(path),
            Web::Placeholder => {
                if path == "index.html" {
                    file_response(path, PLACEHOLDER.as_bytes().to_vec())
                } else {
                    StatusCode::NOT_FOUND.into_response()
                }
            }
        }
    }
}

#[cfg(all(not(debug_assertions), cul_web))]
fn embedded(path: &str) -> Response {
    match EMBEDDED.get_file(path).or_else(|| {
        // Unknown page paths fall back to the app shell. Asset paths do not.
        (!path.contains('.'))
            .then(|| EMBEDDED.get_file("index.html"))
            .flatten()
    }) {
        Some(file) => file_response(
            file.path().to_str().unwrap_or(path),
            file.contents().to_vec(),
        ),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(not(all(not(debug_assertions), cul_web)))]
fn embedded(_path: &str) -> Response {
    StatusCode::NOT_FOUND.into_response()
}

/// Unknown page paths fall back to the app shell. Asset paths do not.
async fn not_found<F, Fut>(path: &str, index: F) -> Response
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<Vec<u8>>>,
{
    if !path.contains('.') {
        if let Some(bytes) = index().await {
            return file_response("index.html", bytes);
        }
    }
    StatusCode::NOT_FOUND.into_response()
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn file_response(path: &str, bytes: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        mime(path).parse().expect("static mime"),
    );
    // The service worker caches the shell. The daemon itself never should.
    headers.insert(
        header::CACHE_CONTROL,
        "no-cache".parse().expect("static header"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifests_and_modules_get_their_mime_types() {
        assert_eq!(mime("manifest.webmanifest"), "application/manifest+json");
        assert_eq!(
            mime("vendor/novnc/core/rfb.js"),
            "text/javascript; charset=utf-8"
        );
    }

    #[tokio::test]
    async fn dotted_and_parent_paths_are_refused() {
        let web = Web::Placeholder;
        let response = web.serve("/../etc/passwd".parse().unwrap()).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let response = web.serve("/".parse().unwrap()).await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}
