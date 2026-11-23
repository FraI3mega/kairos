use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn files_under(dir: &Path, ext: &[&str], out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            files_under(&path, ext, out);
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| ext.contains(&e)) {
            out.push(path);
        }
    }
}

fn read_all(dir: &str, ext: &[&str]) -> Vec<String> {
    let mut paths = Vec::new();
    files_under(&root().join(dir), ext, &mut paths);
    assert!(!paths.is_empty(), "no {ext:?} files under {dir}");
    paths.iter().map(|p| fs::read_to_string(p).unwrap()).collect()
}

fn clean_css(css: &str) -> String {
    let b = css.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if css[i..].starts_with("/*") {
            i = css[i..].find("*/").map_or(b.len(), |n| i + n + 2);
            out.push(' ');
        } else if b[i] == b'"' || b[i] == b'\'' {
            let quote = b[i];
            i += 1;
            while i < b.len() && b[i] != quote {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
            out.push_str("\"\"");
        } else if css[i..].starts_with("url(") {
            i = css[i..].find(')').map_or(b.len(), |n| i + n + 1);
            out.push_str("url()");
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_'
}

fn ident_at(s: &str, start: usize) -> &str {
    let b = s.as_bytes();
    let mut end = start;
    while end < b.len() && is_ident(b[end]) {
        end += 1;
    }
    &s[start..end]
}

fn selector_classes(css: &str) -> BTreeSet<String> {
    let css = clean_css(css);
    let mut classes = BTreeSet::new();
    let mut start = 0;
    for (i, c) in css.char_indices() {
        match c {
            '{' => {
                let prelude = css[start..i].trim();
                if !prelude.starts_with('@') {
                    let b = prelude.as_bytes();
                    for (k, &ch) in b.iter().enumerate() {
                        let next = b.get(k + 1).copied().unwrap_or(b' ');
                        if ch == b'.' && (next.is_ascii_alphabetic() || next == b'_' || next == b'-') {
                            classes.insert(ident_at(prelude, k + 1).to_string());
                        }
                    }
                }
                start = i + 1;
            }
            '}' | ';' => start = i + 1,
            _ => {}
        }
    }
    classes
}

fn property_definitions(text: &str) -> BTreeSet<String> {
    let b = text.as_bytes();
    let mut names = BTreeSet::new();
    let mut i = 0;
    while let Some(n) = text[i..].find("--") {
        let at = i + n;
        i = at + 2;
        if at > 0 && is_ident(b[at - 1]) {
            continue;
        }
        let name = ident_at(text, at);
        if name.len() > 2 && text[at + name.len()..].trim_start().starts_with(':') {
            names.insert(name.to_string());
        }
    }
    names
}

fn js_property_sets(js: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for (n, _) in js.match_indices("setProperty(") {
        let rest = js[n + "setProperty(".len()..].trim_start();
        if let Some(rest) = rest.strip_prefix(['\'', '"']) {
            if rest.starts_with("--") {
                names.insert(ident_at(rest, 0).to_string());
            }
        }
    }
    names
}

fn tokens(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

fn reads_property(texts: &[String], name: &str) -> bool {
    let needle = format!("var({name}");
    texts.iter().any(|t| {
        t.match_indices(&needle).any(|(n, _)| !t.as_bytes().get(n + needle.len()).is_some_and(|&c| is_ident(c)))
    })
}

#[test]
fn every_css_class_and_variable_is_used() {
    let css = read_all("static", &["css"]);
    let js = read_all("static", &["js"]);
    let templates = read_all("templates", &["html"]);
    let rust = read_all("src", &["rs"]);

    let known: BTreeSet<String> = js.iter().chain(&templates).chain(&rust).flat_map(|t| tokens(t)).collect();
    let mut dead = Vec::new();

    let classes: BTreeSet<String> = css.iter().flat_map(|c| selector_classes(c)).collect();
    for class in classes.iter().filter(|c| !known.contains(*c)) {
        dead.push(format!(".{class} (class in a selector, never named in a template, script or Rust)"));
    }

    let readers: Vec<String> = css.iter().chain(&js).chain(&templates).chain(&rust).cloned().collect();
    let mut properties: BTreeSet<String> = BTreeSet::new();
    for c in &css {
        properties.extend(property_definitions(&clean_css(c)));
    }
    for t in &templates {
        properties.extend(property_definitions(t));
    }
    for j in &js {
        properties.extend(js_property_sets(j));
    }
    for name in properties.iter().filter(|p| !reads_property(&readers, p)) {
        dead.push(format!("{name} (custom property that is set but never read with var())"));
    }

    assert!(dead.is_empty(), "unused css:\n  {}", dead.join("\n  "));
}

#[test]
fn the_scanner_reads_selectors_and_properties() {
    let css = r#"
        /* .in-comment { --in-comment: 1 } */
        :root { --a: 1; --b-c:2 }
        .one, .two:not(.three) > a[href$=".png"] { margin: .5em; background: url(/x/.not-a-class.png) }
        @media (max-width:800px) { .four.five:hover::before { --a: var(--b-c) } }
        @keyframes k { 50% { width: 1.5px } }
        .six { content: ".seven" }
    "#;
    let classes: Vec<_> = selector_classes(css).into_iter().collect();
    assert_eq!(classes, ["five", "four", "one", "six", "three", "two"]);
    let props: Vec<_> = property_definitions(&clean_css(css)).into_iter().collect();
    assert_eq!(props, ["--a", "--b-c"]);
    assert!(reads_property(&["x{y:var(--b-c)}".into()], "--b-c"));
    assert!(!reads_property(&["x{y:var(--b-cd)}".into()], "--b-c"));
    assert_eq!(js_property_sets("el.style.setProperty('--r10', v); a.setProperty(name, 1)").into_iter().collect::<Vec<_>>(), ["--r10"]);
}
