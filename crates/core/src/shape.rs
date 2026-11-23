use serde::{Deserialize, Serialize};

use crate::parse::{logish_hits, strip_reminders};

/// How a prompt is laid out, as counts only (never any of its text). Covers the part the harness didn't record as pasted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shape {
    pub chars: u32,
    pub lines: u32,
    pub blank: u32,
    pub indented: u32,
    pub trailing: u32,
    pub markdown: u32,
    pub rules: u32,
    pub logish: u32,
    pub symbols: u32,
    pub digits: u32,
    pub typographic: u32,
    pub pictographs: u32,
    pub sentences: u32,
    pub capitalized: u32,
    pub ended: u32,
    pub casual: u32,
    /// Set when the harness keeps its own record of what was pasted into the prompt (Claude Code's prompt history).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marked: Option<Marked>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Marked {
    pub pastes: u32,
    pub words: u32,
}

const TYPOGRAPHIC: [char; 8] =
    ['\u{2014}', '\u{2013}', '\u{201c}', '\u{201d}', '\u{2018}', '\u{2019}', '\u{2026}', '\u{a0}'];
const SYMBOLS: &str = "{}[]()<>;=|\\$`";
const RULE_CHARS: &str = "-=_*#~";
const RULE_RUN: usize = 4;
const CASUAL: [&str; 38] = [
    "pls", "plz", "u", "ur", "lol", "bro", "idk", "lmk", "ngl", "tbh", "rn", "imo", "btw", "ya", "yea", "nah", "gonna",
    "wanna", "gotta", "kinda", "dunno", "lowk", "lowkey", "fr", "dont", "cant", "im", "thats", "isnt", "doesnt",
    "wont", "ive", "didnt", "whats", "tho", "cuz", "lmao", "wdym",
];

fn n(x: usize) -> u32 {
    u32::try_from(x).unwrap_or(u32::MAX)
}

fn markdown_line(line: &str) -> bool {
    let t = line.trim_start();
    let heading = t.starts_with('#')
        && t.trim_start_matches('#').starts_with(' ')
        && t.len() - t.trim_start_matches('#').len() <= 6;
    let bullet = ["- ", "* ", "+ ", "\u{2022} ", "> "].iter().any(|p| t.starts_with(p));
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    let numbered = (1..=3).contains(&digits)
        && matches!(t[digits..].chars().next(), Some('.' | ')'))
        && t[digits + 1..].starts_with(' ');
    heading || bullet || numbered || t.starts_with("```") || t.starts_with('|') || t.contains("**")
}

fn rule_runs(text: &str) -> usize {
    let mut runs = 0;
    let mut prev = None;
    let mut len = 0;
    for c in text.chars() {
        let ruled = RULE_CHARS.contains(c) || ('\u{2500}'..='\u{257f}').contains(&c);
        if ruled && prev == Some(c) {
            len += 1;
            if len == RULE_RUN {
                runs += 1;
            }
        } else {
            len = 1;
        }
        prev = Some(c);
    }
    runs
}

fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut start = 0;
        let b = line.as_bytes();
        for (i, &c) in b.iter().enumerate() {
            if matches!(c, b'.' | b'!' | b'?') && b.get(i + 1).is_none_or(|n| n.is_ascii_whitespace()) {
                out.push(&line[start..=i]);
                start = i + 1;
            }
        }
        out.push(&line[start..]);
    }
    out.retain(|s| s.chars().any(char::is_alphabetic));
    out
}

pub fn prompt_shape(text: &str) -> Shape {
    let text = strip_reminders(text);
    let text = text.trim();
    let lines: Vec<&str> = if text.is_empty() { Vec::new() } else { text.lines().collect() };
    let filled = || lines.iter().filter(|l| !l.trim().is_empty());
    let sentences = sentences(text);
    let words = text.split(|c: char| !(c.is_alphanumeric() || c == '\''));
    let casual = words.filter(|w| *w == "i" || CASUAL.contains(&w.to_lowercase().as_str())).count();
    Shape {
        chars: n(text.chars().count()),
        lines: n(lines.len()),
        blank: n(lines.len() - filled().count()),
        indented: n(filled().filter(|l| l.starts_with("  ") || l.starts_with('\t')).count()),
        trailing: n(filled().filter(|l| l.ends_with([' ', '\t'])).count()),
        markdown: n(filled().filter(|l| markdown_line(l)).count()),
        rules: n(rule_runs(text)),
        logish: n(logish_hits(text)),
        symbols: n(text.chars().filter(|c| SYMBOLS.contains(*c)).count()),
        digits: n(text.chars().filter(char::is_ascii_digit).count()),
        typographic: n(text.chars().filter(|c| TYPOGRAPHIC.contains(c)).count()),
        pictographs: n(text
            .chars()
            .filter(|&c| {
                c >= '\u{2000}'
                    && !c.is_alphanumeric()
                    && !TYPOGRAPHIC.contains(&c)
                    && !('\u{2500}'..='\u{257f}').contains(&c)
            })
            .count()),
        sentences: n(sentences.len()),
        capitalized: n(sentences
            .iter()
            .filter(|s| s.chars().find(|c| c.is_alphabetic()).is_some_and(char::is_uppercase))
            .count()),
        ended: n(filled().filter(|l| l.trim_end().ends_with(['.', '!', '?', ':'])).count()),
        casual: n(casual),
        marked: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_line_is_one_lowercase_sentence_with_casual_words() {
        let s = prompt_shape("ok so fix the login bug pls, i think its the token and don't cache it");
        assert_eq!((s.lines, s.blank, s.indented, s.markdown, s.typographic), (1, 0, 0, 0, 0));
        assert_eq!((s.sentences, s.capitalized, s.ended), (1, 0, 0));
        assert_eq!(s.casual, 2);
    }

    #[test]
    fn a_written_up_request_has_structure_and_typography() {
        let text = "## Goal\n\nBuild the export \u{2014} it should \u{201c}just work\u{201d}.\n\n- Add a **CSV** option.\n- Keep it fast.\n\n1. Read the rows.\n2) Write them out.";
        let s = prompt_shape(text);
        assert_eq!(s.lines, 9);
        assert_eq!(s.blank, 3);
        assert_eq!(s.markdown, 5);
        assert_eq!(s.typographic, 3);
        assert_eq!((s.sentences, s.capitalized), (6, 6));
        assert_eq!(s.ended, 5);
        assert_eq!(s.casual, 0);
    }

    #[test]
    fn a_pasted_log_has_indentation_rules_and_log_lines() {
        let text = "Traceback (most recent call last):\n    File \"app.py\", line 3\n  \n=====  FAILED  =====\nError: boom   \n12:01:02 retry [x] -> \u{2713}";
        let s = prompt_shape(text);
        assert_eq!(s.indented, 1);
        assert_eq!(s.blank, 1);
        assert_eq!(s.trailing, 1);
        assert_eq!(s.rules, 2);
        assert!(s.logish >= 3);
        assert_eq!(s.pictographs, 1);
        assert!(s.symbols >= 4 && s.digits >= 7);
    }

    #[test]
    fn reminders_and_surrounding_space_are_left_out() {
        let s = prompt_shape("  <system-reminder>\n\n# x\n</system-reminder>hi there  \n");
        assert_eq!(prompt_shape("hi there"), s);
        assert_eq!(prompt_shape("   "), Shape::default());
    }

    #[test]
    fn marked_is_left_out_of_the_json_until_set() {
        let s = prompt_shape("hi");
        assert!(!serde_json::to_string(&s).unwrap().contains("marked"));
        let back: Shape = serde_json::from_str(r#"{"lines":2,"marked":{"pastes":1,"words":40}}"#).unwrap();
        assert_eq!((back.lines, back.marked), (2, Some(Marked { pastes: 1, words: 40 })));
    }
}
