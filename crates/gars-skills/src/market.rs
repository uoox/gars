//! Sophub marketplace client.
//!
//! Scrapes the public listing at `https://fudankw.cn/sophub/` (HTML) and uses
//! the public download endpoint `/sophub/api/sops/{id}/download` for raw
//! markdown. The site has no public JSON API, so list/detail go through a
//! light HTML parser keyed off the stable CSS class names (`.card`,
//! `.card__title`, `.card__meta`, `.card__preview`, `.result-meta`).

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MarketQuery {
    pub source: Option<String>, // "official" | "community"
    pub level: Option<String>,  // "普通" | "精良" | "稀有" | "史诗" | "传说"
    pub q: Option<String>,
    pub page: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarketItem {
    pub id: String,
    pub title: String,
    pub url: String,
    pub author: String,
    pub level: String,
    pub source: String, // "official" | "community"
    pub stars: f32,
    pub comments: u32,
    pub posted: String,
    pub preview: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarketListPage {
    pub items: Vec<MarketItem>,
    pub total: u32,
    pub page: u32,
    pub pages: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarketDetail {
    pub id: String,
    pub title: String,
    pub url: String,
    pub download_url: String,
    pub markdown: String,
}

#[derive(Clone, Debug)]
pub struct MarketClient {
    base: String,
    http: Client,
}

impl MarketClient {
    pub fn new(base: &str) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("gars-skills-market/0.5")
            .build()
            .context("build market http client")?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            http,
        })
    }

    pub fn list_url(&self, query: &MarketQuery) -> String {
        let mut url = format!("{}/sophub/", self.base);
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(s) = &query.source
            && !s.is_empty()
        {
            params.push(("source", s.clone()));
        }
        if let Some(l) = &query.level
            && !l.is_empty()
        {
            params.push(("level", l.clone()));
        }
        if let Some(q) = &query.q
            && !q.is_empty()
        {
            params.push(("q", q.clone()));
        }
        if let Some(p) = query.page
            && p > 1
        {
            params.push(("page", p.to_string()));
        }
        if !params.is_empty() {
            url.push('?');
            for (i, (k, v)) in params.iter().enumerate() {
                if i > 0 {
                    url.push('&');
                }
                url.push_str(k);
                url.push('=');
                url.push_str(&urlencode(v));
            }
        }
        url
    }

    pub async fn list(&self, query: &MarketQuery) -> Result<MarketListPage> {
        let url = self.list_url(query);
        let html = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("GET {url}"))?
            .text()
            .await?;
        parse_list_html(&html, &self.base)
    }

    pub async fn detail(&self, id: &str) -> Result<MarketDetail> {
        let id = sanitize_id(id)?;
        let url = format!("{}/sophub/sops/{}", self.base, id);
        let download_url = format!("{}/sophub/api/sops/{}/download", self.base, id);
        // Title is best-effort from the detail page; the actual rendering is
        // done by the markdown body.
        let title = match self
            .http
            .get(&url)
            .send()
            .await
            .ok()
            .and_then(|r| r.error_for_status().ok())
        {
            Some(resp) => match resp.text().await {
                Ok(html) => extract_detail_title(&html).unwrap_or_else(|| id.clone()),
                Err(_) => id.clone(),
            },
            None => id.clone(),
        };
        let markdown = self
            .http
            .get(&download_url)
            .send()
            .await
            .with_context(|| format!("GET {download_url}"))?
            .error_for_status()
            .with_context(|| format!("GET {download_url}"))?
            .text()
            .await?;
        Ok(MarketDetail {
            id,
            title,
            url,
            download_url,
            markdown,
        })
    }

    pub async fn download_markdown(&self, id: &str) -> Result<String> {
        let id = sanitize_id(id)?;
        let url = format!("{}/sophub/api/sops/{}/download", self.base, id);
        let body = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("GET {url}"))?
            .text()
            .await?;
        Ok(body)
    }
}

fn sanitize_id(id: &str) -> Result<String> {
    // Sophub IDs are mongo ObjectId-style 24-char lowercase hex strings.
    // We accept any hex string between 8 and 64 chars to leave room for
    // upstream changes, but reject anything containing a non-hex character
    // (defends against path traversal / URL injection).
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed.len() > 64 || trimmed.len() < 8 {
        return Err(anyhow!("invalid sophub id length: {id}"));
    }
    if !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow!("invalid sophub id: {id}"));
    }
    Ok(trimmed.to_ascii_lowercase())
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

pub fn parse_list_html(html: &str, base: &str) -> Result<MarketListPage> {
    let doc = Html::parse_document(html);
    let card_sel = Selector::parse("a.card").map_err(|e| anyhow!("css: {e}"))?;
    let title_sel = Selector::parse("h3.card__title").map_err(|e| anyhow!("css: {e}"))?;
    let meta_sel = Selector::parse("div.card__meta").map_err(|e| anyhow!("css: {e}"))?;
    let preview_sel = Selector::parse("p.card__preview").map_err(|e| anyhow!("css: {e}"))?;
    let official_sel = Selector::parse(".badge--official").map_err(|e| anyhow!("css: {e}"))?;
    let user_sel = Selector::parse("span.badge--user").map_err(|e| anyhow!("css: {e}"))?;
    let result_meta_sel =
        Selector::parse("section.result-meta span").map_err(|e| anyhow!("css: {e}"))?;

    let base_trim = base.trim_end_matches('/');
    let mut items = Vec::new();
    for card in doc.select(&card_sel) {
        let href = card.value().attr("href").unwrap_or("");
        let Some(id) = href.strip_prefix("/sophub/sops/") else {
            continue;
        };
        let title = card
            .select(&title_sel)
            .next()
            .map(|n| n.text().collect::<Vec<_>>().join("").trim().to_string())
            .unwrap_or_else(|| card.value().attr("title").unwrap_or("").to_string());
        // Drop the "官方"/"用户" prefix that comes from the badge text.
        let title = title
            .trim_start_matches("官方")
            .trim_start_matches("用户")
            .trim()
            .to_string();
        let source = if card.select(&official_sel).next().is_some() {
            "official"
        } else {
            "community"
        }
        .to_string();
        let level = card
            .value()
            .classes()
            .find_map(|c| c.strip_prefix("card--rarity-"))
            .unwrap_or("普通")
            .to_string();
        let meta_text = card
            .select(&meta_sel)
            .next()
            .map(|n| n.text().collect::<Vec<_>>().join(""))
            .unwrap_or_default();
        let author = card
            .select(&user_sel)
            .next()
            .map(|n| n.text().collect::<Vec<_>>().join(""))
            .unwrap_or_default()
            .trim()
            .trim_start_matches('@')
            .to_string();
        let stars = extract_number(&meta_text, '⭐').unwrap_or(0.0);
        let comments = extract_number(&meta_text, '💬').unwrap_or(0.0) as u32;
        let posted = extract_posted(&meta_text);
        let preview = card
            .select(&preview_sel)
            .next()
            .map(|n| n.text().collect::<Vec<_>>().join(""))
            .unwrap_or_default()
            .trim()
            .to_string();
        items.push(MarketItem {
            id: id.trim().trim_end_matches('/').to_string(),
            title,
            url: format!("{base_trim}{href}"),
            author,
            level,
            source,
            stars: stars as f32,
            comments,
            posted,
            preview,
        });
    }

    // Pagination: "共 N 条 · 第 P / T 页"
    let mut total = items.len() as u32;
    let mut page = 1u32;
    let mut pages = 1u32;
    for span in doc.select(&result_meta_sel) {
        let text = span.text().collect::<String>();
        if text.contains('条')
            && let Some((t, p, tp)) = parse_result_meta(&text)
        {
            total = t;
            page = p;
            pages = tp;
            break;
        }
    }

    Ok(MarketListPage {
        items,
        total,
        page,
        pages,
    })
}

fn extract_number(meta: &str, marker: char) -> Option<f64> {
    let pos = meta.find(marker)?;
    let after: String = meta[pos + marker.len_utf8()..]
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    after.parse().ok()
}

fn extract_posted(meta: &str) -> String {
    // The meta line looks like "@sophub · ⭐ 0.0 · 💬 0 · 9 天前". We grab the
    // last `·`-separated segment as a reasonable "posted" string.
    meta.split('·')
        .map(str::trim)
        .rfind(|s| !s.is_empty() && !s.starts_with('@') && !s.contains('⭐') && !s.contains('💬'))
        .unwrap_or("")
        .to_string()
}

fn parse_result_meta(text: &str) -> Option<(u32, u32, u32)> {
    // e.g. "共 114 条 · 第 1 / 5 页"
    let digits: Vec<u32> = text
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|s| s.parse::<u32>().ok())
        .collect();
    if digits.len() >= 3 {
        Some((digits[0], digits[1], digits[2]))
    } else {
        None
    }
}

fn extract_detail_title(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse("title").ok()?;
    let title = doc
        .select(&sel)
        .next()?
        .text()
        .collect::<String>()
        .trim()
        .to_string();
    if title.is_empty() { None } else { Some(title) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pagination_meta() {
        let got = parse_result_meta("共 114 条 · 第 1 / 5 页").unwrap();
        assert_eq!(got, (114, 1, 5));
    }

    #[test]
    fn sanitizes_id_rejects_path() {
        assert!(sanitize_id("../etc/passwd").is_err());
        assert!(sanitize_id("69f1c0029c0da86eac3ab0b3").is_ok());
    }

    #[test]
    fn extracts_meta_numbers() {
        let m = "@sophub · ⭐ 5.0 · 💬 2 · 9 天前";
        assert!((extract_number(m, '⭐').unwrap() - 5.0).abs() < 1e-6);
        assert_eq!(extract_number(m, '💬').unwrap() as u32, 2);
        assert_eq!(extract_posted(m), "9 天前");
    }

    #[test]
    fn list_url_includes_known_params() {
        let c = MarketClient::new("https://fudankw.cn").unwrap();
        let url = c.list_url(&MarketQuery {
            source: Some("official".into()),
            level: Some("史诗".into()),
            q: Some("plan".into()),
            page: Some(2),
        });
        assert!(url.starts_with("https://fudankw.cn/sophub/?"));
        assert!(url.contains("source=official"));
        assert!(url.contains("page=2"));
        assert!(url.contains("q=plan"));
    }
}
