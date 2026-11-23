use std::sync::{Arc, RwLock};

use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Signin {
    #[default]
    Idle,
    Waiting { code: String },
    Failed { reason: String },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub version: &'static str,
    pub focus: &'static str,
    pub idle: &'static str,
    pub today_secs: f64,
    pub paused_until: Option<f64>,
    pub username: Option<String>,
    pub server: Option<String>,
    pub last_upload: Option<f64>,
    pub upload_error: Option<String>,
    pub token_rejected: bool,
    pub update: Option<String>,
    pub signin: Signin,
}

pub type Shared = Arc<RwLock<Status>>;

pub fn shared() -> Shared {
    Arc::new(RwLock::new(Status { version: env!("CARGO_PKG_VERSION"), ..Status::default() }))
}

pub fn set(s: &Shared, f: impl FnOnce(&mut Status)) {
    if let Ok(mut g) = s.write() {
        f(&mut g);
    }
}
