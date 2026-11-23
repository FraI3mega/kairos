//! Tied to the `/me` markup, which has no JSON for these: a page without the markers counts as unrecognised, not a failure.

#[derive(Debug, Default, PartialEq)]
pub struct MePage {
    pub week: Option<i64>,
    pub all: Option<i64>,
    pub today: Option<i64>,
    pub days: Vec<(String, i64)>,
    pub classes: Vec<(String, i64)>,
    pub projects: Vec<(String, i64)>,
}

fn unescape(s: &str) -> String {
    s.replace("&#x2f;", "/").replace("&#47;", "/").replace("&quot;", "\"").replace("&#x27;", "'").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

pub fn parse_short(s: &str) -> Option<i64> {
    let s = s.trim();
    let (h, m) = s.split_once("h ").unwrap_or(("0", s));
    Some(h.trim().parse::<i64>().ok()? * 60 + m.trim().strip_suffix('m')?.parse::<i64>().ok()?)
}

pub fn parse_human(s: &str) -> Option<i64> {
    if s.trim() == "under 1 min" {
        return Some(0);
    }
    let mut total = 0;
    let mut seen = false;
    let mut it = s.split_whitespace();
    while let Some(n) = it.next() {
        let unit = it.next()?;
        let n: i64 = n.parse().ok()?;
        match unit {
            "hrs" | "hr" => total += n * 60,
            "mins" | "min" => total += n,
            _ => return None,
        }
        seen = true;
    }
    seen.then_some(total)
}

fn between<'a>(s: &'a str, from: &str, to: &str) -> Option<&'a str> {
    let a = s.find(from)? + from.len();
    let b = s[a..].find(to)? + a;
    Some(&s[a..b])
}

fn rows(html: &str, heading: &str) -> Vec<(String, i64)> {
    let Some(at) = html.find(&format!("<h2>{heading}</h2>")) else { return Vec::new() };
    let rest = &html[at..];
    let end = rest.find("</ul>").or_else(|| rest.find("Nothing here yet")).unwrap_or(rest.len());
    let mut s = &rest[..end];
    let mut out = Vec::new();
    const KEY: &str = "<span class=\"n\" title=\"";
    while let Some(i) = s.find(KEY) {
        s = &s[i + KEY.len()..];
        let label = s.split('"').next().unwrap_or("");
        let Some(time) = between(s, "<span class=\"t\">", "</span>") else { break };
        if let Some(m) = parse_human(time) {
            out.push((unescape(label), m));
        }
    }
    out
}

pub fn parse_me(html: &str) -> MePage {
    let mut page = MePage {
        week: between(html, "data-live=\"week\">", "</div>").and_then(parse_short),
        today: between(html, "id=\"dial-b\">", "</b>").and_then(parse_short),
        ..MePage::default()
    };
    if let Some(sub) = between(html, "<p class=\"sub\">", "</p>") {
        page.all = sub.split_once("· ").and_then(|(_, r)| r.strip_suffix(" all time")).and_then(parse_short);
    }
    let mut s = html;
    while let Some(i) = s.find("<div title=\"") {
        s = &s[i + 12..];
        let Some(title) = s.split('"').next() else { break };
        if let Some((day, time)) = title.split_once(" · ") {
            if let Some(m) = parse_human(time) {
                page.days.push((day.to_string(), m));
            }
        }
    }
    page.projects = rows(html, "Projects");
    page.classes = rows(html, "Kind of work");
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_short("12h 05m"), Some(725));
        assert_eq!(parse_short("26m"), Some(26));
        assert_eq!(parse_short("0m"), Some(0));
        assert_eq!(parse_human("1 hr 22 min"), Some(82));
        assert_eq!(parse_human("under 1 min"), Some(0));
        assert_eq!(parse_human("2 hrs 3 mins"), Some(123));
        assert_eq!(parse_human("2 hrs"), Some(120));
        assert_eq!(parse_human("45 mins"), Some(45));
        assert_eq!(parse_human("soon"), None);
    }

    #[test]
    fn page() {
        let html = "<div class=\"big\" data-live=\"week\">3h 20m</div>\n<p class=\"sub\">Focus time this week · 10h 01m all time</p>\n\
            <b id=\"dial-b\">0h 45m</b><h2>Last 30 days</h2><div class=\"days\"><div title=\"Thu Nov 19 · 1 hrs 5 mins\" style=\"height:1%\"></div>\n\
            <div title=\"Fri Nov 20 · 0 mins\"></div></div>\n\
            <h2>Projects</h2><ul class=\"rows\"><li>\n<span class=\"n\" title=\"obaym&#x2f;app\">x</span><span class=\"t\">2 hrs 3 mins</span></li></ul>\n\
            <h2>Kind of work</h2><ul class=\"rows\"><li>\n<span class=\"n\" title=\"Editing\"><i></i>Editing</span><span class=\"bar\"></span><span class=\"t\">1 hrs</span></li></ul>";
        let p = parse_me(html);
        assert_eq!((p.week, p.all, p.today), (Some(200), Some(601), Some(45)));
        assert_eq!(p.days, vec![("Thu Nov 19".to_string(), 65), ("Fri Nov 20".to_string(), 0)]);
        assert_eq!(p.projects, vec![("obaym/app".to_string(), 123)]);
        assert_eq!(p.classes, vec![("Editing".to_string(), 60)]);
    }
}
