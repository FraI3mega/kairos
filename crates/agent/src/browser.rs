use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kairos_core::event::Class;

const FRESH: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    pub class: Class,
    pub project: Option<String>,
}

#[derive(Default)]
pub struct BrowserState {
    tabs: Mutex<HashMap<&'static str, (Tab, Instant)>>,
}

pub fn family(app: &str) -> Option<&'static str> {
    let app = app.to_ascii_lowercase();
    let is = |prefix: &str| app == prefix || app.starts_with(&format!("{prefix}-"));
    Some(match () {
        _ if is("msedge") || is("microsoft-edge") => "edge",
        _ if is("brave") || is("brave-browser") => "brave",
        _ if is("vivaldi") || is("vivaldi-bin") => "vivaldi",
        _ if is("opera") => "opera",
        _ if is("chrome") || is("google-chrome") || is("chromium") || is("chromium-browser") => "chrome",
        _ if is("firefox") || is("firefox-esr") || is("librewolf") || is("zen") || is("waterfox") => "firefox",
        _ if is("arc") => "arc",
        _ => return None,
    })
}

impl BrowserState {
    pub fn report(&self, family: &'static str, tab: Tab) {
        if let Ok(mut tabs) = self.tabs.lock() {
            tabs.insert(family, (tab, Instant::now()));
        }
    }

    pub fn current(&self, app: &str) -> Option<Tab> {
        let family = family(app)?;
        let tabs = self.tabs.lock().ok()?;
        let fresh = |f: &str| tabs.get(f).filter(|(_, at)| at.elapsed() < FRESH).map(|(tab, _)| tab.clone());
        // Brave, Vivaldi and Arc can't be told apart from Chrome by the extension, so it reports them as chrome.
        fresh(family).or_else(|| matches!(family, "brave" | "vivaldi" | "arc").then(|| fresh("chrome")).flatten())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_match_process_names() {
        assert_eq!(family("chrome"), Some("chrome"));
        assert_eq!(family("google-chrome-stable"), Some("chrome"));
        assert_eq!(family("msedge"), Some("edge"));
        assert_eq!(family("firefox"), Some("firefox"));
        assert_eq!(family("code"), None);
    }

    #[test]
    fn reports_go_stale() {
        let s = BrowserState::default();
        assert_eq!(s.current("chrome"), None);
        s.report("chrome", Tab { class: Class::Testing, project: Some("o/app".into()) });
        assert_eq!(s.current("chrome").map(|t| t.class), Some(Class::Testing));
        assert_eq!(s.current("firefox"), None);
    }

    #[test]
    fn chromium_forks_fall_back_to_chrome_reports() {
        let s = BrowserState::default();
        s.report("chrome", Tab { class: Class::Testing, project: Some("o/app".into()) });
        for app in ["brave", "vivaldi", "arc"] {
            assert_eq!(s.current(app).map(|t| t.class), Some(Class::Testing), "{app}");
        }
        assert_eq!(s.current("msedge"), None);
        s.report("brave", Tab { class: Class::Other, project: None });
        assert_eq!(s.current("brave").map(|t| t.class), Some(Class::Other));
    }
}
