pub mod admin;
pub mod device;
pub mod export;
pub mod extension;
pub mod leaderboards;
pub mod pages;
pub mod project;
pub mod review;
pub mod settings;

use askama::Template;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderValue};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use kairos_core::brand::MARK;

use crate::assets::url;
use crate::auth::{self, Current, Session, User};
use crate::security::Nonce;
use crate::util::{ErrorMessage, Res};
use crate::St;

pub struct Base {
    pub title: String,
    pub description: String,
    pub canonical: String,
    pub og_type: &'static str,
    pub nonce: String,
    pub test_mode: bool,
    pub public_url: String,
    pub host: String,
    pub user: Option<User>,
    pub csrf: String,
    pub mark: String,
    pub fonts_css: &'static str,
    pub kairos_css: &'static str,
    pub landing_css: &'static str,
    pub landing_js: &'static str,
    pub drip_js: &'static str,
    pub dash_js: &'static str,
    pub settings_js: &'static str,
}

pub fn mark_rects() -> String {
    let mut r = String::new();
    for (y, row) in MARK.iter().enumerate() {
        for (x, c) in row.chars().enumerate() {
            match c {
                '#' => r.push_str(&format!("<rect x='{x}' y='{y}' width='1' height='1'/>")),
                'o' => r.push_str(&format!("<rect x='{x}' y='{y}' width='1' height='1' opacity='.42'/>")),
                _ => {}
            }
        }
    }
    r
}

impl Base {
    pub fn new(st: &St, ctx: &Ctx, title: &str) -> Base {
        Base {
            title: title.to_string(),
            description: String::new(),
            canonical: String::new(),
            og_type: "website",
            nonce: ctx.nonce.clone(),
            test_mode: st.config.test_mode(),
            public_url: st.config.public_url.clone(),
            host: st.config.host().to_string(),
            user: ctx.session.as_ref().map(|s| s.user.clone()),
            csrf: ctx.session.as_ref().map(Session::csrf).unwrap_or_default(),
            mark: mark_rects(),
            fonts_css: url("fonts.css"),
            kairos_css: url("kairos.css"),
            landing_css: url("landing.css"),
            landing_js: url("landing.js"),
            drip_js: url("drip.js"),
            dash_js: url("dash.js"),
            settings_js: url("settings.js"),
        }
    }
}

impl Base {
    pub fn share(mut self, description: &str, path: &str, og_type: &'static str) -> Base {
        self.description = description.to_string();
        self.canonical = format!("{}{path}", self.public_url);
        self.og_type = og_type;
        self
    }
}

const AVATAR_COLORS: [&str; 6] = ["#6ee7c8", "#ffd166", "#ff8fab", "#8fd3ff", "#a297f5", "#b8e986"];

/// Drawn here because profile pictures live on other sites, which the CSP does not allow.
pub fn avatar(username: &str) -> String {
    let h = blake3::hash(username.as_bytes());
    let b = h.as_bytes();
    let color = AVATAR_COLORS[b[0] as usize % AVATAR_COLORS.len()];
    let mut r = String::new();
    for y in 0..5usize {
        for x in 0..3usize {
            let i = y * 3 + x;
            if (b[1 + i / 8] >> (i % 8)) & 1 == 1 {
                for xx in [x, 4 - x] {
                    r.push_str(&format!("<rect x='{xx}' y='{y}' width='1' height='1'/>"));
                }
            }
        }
    }
    format!("<g fill='{color}'>{r}</g>")
}

pub struct Ctx {
    pub nonce: String,
    pub session: Option<Session>,
}

impl FromRequestParts<St> for Ctx {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &St) -> Result<Self, Self::Rejection> {
        Ok(Ctx {
            nonce: parts.extensions.get::<Nonce>().map(|n| n.0.clone()).unwrap_or_default(),
            session: parts.extensions.get::<Current>().and_then(|c| c.0.clone()),
        })
    }
}

pub fn render(t: &impl Template) -> Res<Response> {
    Ok(Html(t.render()?).into_response())
}

#[derive(Template)]
#[template(path = "message.html")]
pub struct Message {
    pub base: Base,
    pub heading: String,
    pub text: String,
}

pub async fn layer(State(st): State<St>, mut req: Request, next: Next) -> Response {
    let (session, refresh) = match auth::load(&st, req.headers()).await {
        Ok(Some((s, r))) => (Some(s), r),
        Ok(None) => (None, false),
        Err(e) => return e.into_response(),
    };
    req.extensions_mut().insert(Current(session.clone()));
    let nonce = req.extensions().get::<Nonce>().map(|n| n.0.clone()).unwrap_or_default();
    let mut res = next.run(req).await;

    let name = auth::session_cookie(&st.config);
    let touched = res.headers().get_all(header::SET_COOKIE).iter().any(|v| v.to_str().is_ok_and(|v| v.starts_with(name)));
    if let (Some(s), true, false) = (&session, refresh, touched) {
        let c = auth::set_cookie(&st.config, name, &s.token, auth::SESSION_DAYS * 86400);
        if let Ok(v) = HeaderValue::from_str(&c) {
            res.headers_mut().append(header::SET_COOKIE, v);
        }
    }

    if let Some(ErrorMessage(text)) = res.extensions().get::<ErrorMessage>().cloned() {
        let status = res.status();
        let retry = res.headers().get(header::RETRY_AFTER).cloned();
        let heading = match status.as_u16() {
            404 => "Not found",
            403 => "Not allowed",
            429 => "Slow down",
            s if s >= 500 => "Something went wrong",
            _ => "That didn't work",
        };
        let text = match status.as_u16() {
            404 => "There's nothing here.".to_string(),
            429 => "Too many requests. Wait a moment and try again.".to_string(),
            s if s >= 500 && text == "internal" => "The server hit an error. Try again in a minute.".to_string(),
            _ => text,
        };
        let page = Message { base: Base::new(&st, &Ctx { nonce, session }, heading), heading: heading.into(), text };
        if let Ok(html) = page.render() {
            let mut out = (status, Html(html)).into_response();
            if let Some(r) = retry {
                out.headers_mut().insert(header::RETRY_AFTER, r);
            }
            return out;
        }
    }
    res
}
