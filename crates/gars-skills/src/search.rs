use std::{collections::HashMap, path::Path};

use serde::{Deserialize, Serialize};

use crate::skill::{SkillIndex, scan_skills_dir};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillHit {
    pub skill: SkillIndex,
    pub score: f64,
}

#[derive(Clone, Debug, Default)]
pub struct SearchOptions {
    pub top_k: usize,
    pub category: Option<String>,
    pub autonomous_only: bool,
}

pub fn search_local(query: &str, root: &Path, opts: SearchOptions) -> Vec<SkillHit> {
    let skills = scan_skills_dir(root);
    rank(skills, query, opts)
}

pub fn rank(skills: Vec<SkillIndex>, query: &str, opts: SearchOptions) -> Vec<SkillHit> {
    let terms = tokenize(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let filtered: Vec<SkillIndex> = skills
        .into_iter()
        .filter(|s| {
            if let Some(cat) = &opts.category
                && !s.category.eq_ignore_ascii_case(cat)
            {
                return false;
            }
            if opts.autonomous_only && !s.autonomous_safe {
                return false;
            }
            true
        })
        .collect();

    let n = filtered.len().max(1) as f64;
    let mut df: HashMap<String, usize> = HashMap::new();
    let docs: Vec<(String, usize)> = filtered
        .iter()
        .map(|s| {
            let doc = build_doc(s);
            let toks = tokenize(&doc);
            let len = toks.len();
            for term in unique(toks) {
                *df.entry(term).or_insert(0) += 1;
            }
            (doc, len)
        })
        .collect();
    let avgdl = docs.iter().map(|(_, l)| *l as f64).sum::<f64>() / n;
    let k1 = 1.2;
    let b = 0.75;

    let mut hits: Vec<SkillHit> = filtered
        .into_iter()
        .enumerate()
        .map(|(i, skill)| {
            let (doc, len) = &docs[i];
            let mut tf: HashMap<&str, usize> = HashMap::new();
            for t in tokenize(doc) {
                *tf.entry(boxed_leak(t)).or_insert(0) += 1;
            }
            let dl = *len as f64;
            let mut score = 0.0;
            for term in &terms {
                let f = *tf.get(term.as_str()).unwrap_or(&0) as f64;
                if f == 0.0 {
                    continue;
                }
                let dft = *df.get(term).unwrap_or(&0) as f64;
                let idf = ((n - dft + 0.5) / (dft + 0.5) + 1.0).ln();
                let denom = f + k1 * (1.0 - b + b * dl / avgdl.max(1.0));
                score += idf * f * (k1 + 1.0) / denom;
                if skill.key.eq_ignore_ascii_case(term) {
                    score += 5.0;
                }
                if skill.tags.iter().any(|t| t.eq_ignore_ascii_case(term)) {
                    score += 2.0;
                }
            }
            SkillHit { skill, score }
        })
        .filter(|h| h.score > 0.0)
        .collect();

    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if opts.top_k > 0 {
        hits.truncate(opts.top_k);
    }
    hits
}

fn build_doc(skill: &SkillIndex) -> String {
    let mut s = String::new();
    s.push_str(&skill.key);
    s.push(' ');
    s.push_str(&skill.name);
    s.push(' ');
    s.push_str(&skill.one_line_summary);
    s.push(' ');
    s.push_str(&skill.description);
    s.push(' ');
    s.push_str(&skill.category);
    s.push(' ');
    for t in &skill.tags {
        s.push_str(t);
        s.push(' ');
    }
    s.push_str(&skill.body_preview);
    s
}

fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                buf.push(lower);
            }
        } else if !buf.is_empty() {
            out.push(std::mem::take(&mut buf));
        }
        if !ch.is_ascii() && !ch.is_alphanumeric() {
            // already handled (non-alphanumeric resets)
        }
        if !ch.is_ascii() && ch.is_alphanumeric() {
            // CJK: each char as its own token
            let ch_str = ch.to_string();
            if !buf.is_empty() && buf != ch_str {
                out.push(std::mem::take(&mut buf));
            }
            if !out.last().map(|s| s.as_str() == ch_str).unwrap_or(false) {
                out.push(ch_str);
            }
            buf.clear();
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

fn unique(tokens: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for t in tokens {
        if seen.insert(t.clone()) {
            out.push(t);
        }
    }
    out
}

fn boxed_leak(s: String) -> &'static str {
    // Used only for short-lived BM25 calculation within a single function scope.
    // Each invocation leaks O(unique_terms) bytes; acceptable for an offline search.
    Box::leak(s.into_boxed_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_keyword_in_key_first() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("plan.md");
        let p2 = dir.path().join("misc.md");
        std::fs::write(
            &p1,
            "---\nkey: plan\nname: Plan SOP\ntags: [plan]\n---\nplan body\n",
        )
        .unwrap();
        std::fs::write(
            &p2,
            "---\nkey: misc\nname: Misc\n---\nthis mentions plan once\n",
        )
        .unwrap();
        let hits = search_local("plan", dir.path(), SearchOptions::default());
        assert!(!hits.is_empty());
        assert_eq!(hits[0].skill.key, "plan");
    }
}
