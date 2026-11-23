use askama::Template;
use axum::extract::State;
use axum::response::Response;

use super::{render, Base, Ctx};
use crate::util::Res;
use crate::St;

#[derive(Template)]
#[template(path = "extension.html")]
struct Page {
    base: Base,
    chrome: Option<String>,
    firefox: Option<String>,
}

pub async fn page(State(st): State<St>, ctx: Ctx) -> Res<Response> {
    let base = Base::new(&st, &ctx, "Browser extension · Kairos").share(
        "The Kairos browser extension tells Kairos when you're testing your app on localhost, so that time counts too.",
        "/extension",
        "website",
    );
    render(&Page { base, chrome: st.config.chrome_extension_url.clone(), firefox: st.config.firefox_extension_url.clone() })
}
