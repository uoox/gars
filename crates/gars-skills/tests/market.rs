//! Fixture-based regression test for the Sophub list parser.

use gars_skills::parse_list_html;

const FIXTURE: &str = r##"
<html><body>
<section class="result-meta">
  <span>共 114 条 · 第 1 / 5 页</span>
</section>
<a class="card card--rarity-史诗 card--has-icon" href="/sophub/sops/69f1c0029c0da86eac3ab0b3" title="sophub使用说明">
  <h3 class="card__title"><span class="badge badge--official">官方</span> sophub使用说明</h3>
  <div class="card__meta">
    <span class="badge badge--user">@sophub</span>
    · ⭐ 0.0
    · 💬 0
    · 9 天前
  </div>
  <p class="card__preview"># Sophub  一个 SOP 共享平台.</p>
</a>
<a class="card card--rarity-史诗 card--has-icon" href="/sophub/sops/69f207e9ba77d8b04fb0b9bb" title="hCaptcha 过验 SOP">
  <h3 class="card__title"><span class="badge badge--official">官方</span> hCaptcha 过验 SOP — 物理键鼠策略与避坑指南</h3>
  <div class="card__meta">
    <span class="badge badge--user">@sophub</span>
    · ⭐ 5.0
    · 💬 2
    · 16 天前
  </div>
  <p class="card__preview"># hCaptcha 过验 SOP</p>
</a>
</body></html>
"##;

#[test]
fn parses_two_items_and_pagination() {
    let page = parse_list_html(FIXTURE, "https://fudankw.cn").unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.total, 114);
    assert_eq!(page.page, 1);
    assert_eq!(page.pages, 5);

    let first = &page.items[0];
    assert_eq!(first.id, "69f1c0029c0da86eac3ab0b3");
    assert_eq!(first.source, "official");
    assert_eq!(first.level, "史诗");
    assert_eq!(first.author, "sophub");
    assert_eq!(first.comments, 0);
    assert_eq!(first.posted, "9 天前");
    assert!(first.url.contains("/sophub/sops/"));
    assert!(first.title.starts_with("sophub"));

    let second = &page.items[1];
    assert!((second.stars - 5.0).abs() < 1e-3);
    assert_eq!(second.comments, 2);
    assert!(second.title.contains("hCaptcha"));
}
