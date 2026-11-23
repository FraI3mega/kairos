use std::collections::HashMap;
use std::sync::OnceLock;

use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

// Fonts come before the stylesheets that point at them: a stylesheet's `/static/<name>`
// references are rewritten to the hashed URLs of files listed earlier.
const FILES: &[(&str, &[u8])] = &[
    ("fonts/funnel-display.woff2", include_bytes!("../static/fonts/funnel-display.woff2")),
    ("fonts/funnel-sans.woff2", include_bytes!("../static/fonts/funnel-sans.woff2")),
    ("fonts/plex-mono-400.woff2", include_bytes!("../static/fonts/plex-mono-400.woff2")),
    ("fonts/plex-mono-500.woff2", include_bytes!("../static/fonts/plex-mono-500.woff2")),
    ("fonts.css", include_bytes!("../static/fonts.css")),
    ("kairos.css", include_bytes!("../static/kairos.css")),
    ("landing.css", include_bytes!("../static/landing.css")),
    ("drip.js", include_bytes!("../static/drip.js")),
    ("dash.js", include_bytes!("../static/dash.js")),
    ("landing.js", include_bytes!("../static/landing.js")),
    ("settings.js", include_bytes!("../static/settings.js")),
];

struct File {
    bytes: Vec<u8>,
    mime: &'static str,
}

pub struct Assets {
    urls: HashMap<&'static str, String>,
    files: HashMap<String, File>,
}

fn mime(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, e)| e) {
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn build() -> Assets {
    let mut urls: HashMap<&'static str, String> = HashMap::new();
    let mut files = HashMap::new();
    for &(name, raw) in FILES {
        let mut bytes = raw.to_vec();
        if name.ends_with(".css") {
            let mut text = String::from_utf8_lossy(raw).into_owned();
            for (dep, url) in &urls {
                text = text.replace(&format!("/static/{dep}"), url);
            }
            bytes = text.into_bytes();
        }
        let hash = blake3::hash(&bytes).to_hex();
        let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
        let url = format!("/static/{stem}.{}.{ext}", &hash[..12]);
        files.insert(url.clone(), File { bytes, mime: mime(name) });
        urls.insert(name, url);
    }
    Assets { urls, files }
}

pub fn assets() -> &'static Assets {
    static ASSETS: OnceLock<Assets> = OnceLock::new();
    ASSETS.get_or_init(build)
}

pub fn url(name: &str) -> &'static str {
    assets().urls.get(name).map_or("/static/missing", String::as_str)
}

pub async fn serve(Path(path): Path<String>) -> Response {
    match assets().files.get(&format!("/static/{path}")) {
        Some(f) => (
            [(header::CONTENT_TYPE, f.mime), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")],
            f.bytes.as_slice(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stylesheets_point_at_hashed_fonts() {
        let css = &assets().files[url("fonts.css")].bytes;
        let css = String::from_utf8_lossy(css);
        assert!(css.contains(url("fonts/funnel-sans.woff2")), "{css}");
        assert!(!css.contains("/static/fonts/funnel-sans.woff2)"));
        assert!(url("kairos.css").starts_with("/static/kairos."));
    }
}
