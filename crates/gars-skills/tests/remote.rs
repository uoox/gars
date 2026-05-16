//! Contract tests for the remote skill-search backend. Uses wiremock to
//! exercise the happy path and the fallback-on-failure path.

use std::time::Duration;

use gars_skills::{RemoteClient, RemoteSearchResult};
use serde_json::json;
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn remote_returns_results_on_happy_path() {
    let server = MockServer::start().await;
    let body = json!({
        "results": [
            {
                "skill": {
                    "key": "plan_sop",
                    "name": "Plan SOP",
                    "one_line_summary": "Break tasks down into a plan",
                    "description": "Use plan mode for multi-step work",
                    "category": "workflow",
                    "tags": ["plan", "subagent"],
                    "form": "markdown",
                    "autonomous_safe": true,
                    "path": "/dev/null",
                    "body_preview": "...",
                    "source": "builtin"
                },
                "relevance": 0.92,
                "quality": 0.81,
                "final_score": 0.87,
                "match_reasons": ["title_hit", "tag_hit"],
                "warnings": []
            }
        ]
    });
    Mock::given(method("POST"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let client = RemoteClient::new(&server.uri(), None, Duration::from_secs(3)).unwrap();
    let results: Vec<RemoteSearchResult> = client.search("plan", None, 10).await.unwrap();
    assert_eq!(results.len(), 1);
    let r = &results[0];
    assert_eq!(r.skill.key, "plan_sop");
    assert!(r.match_reasons.contains(&"title_hit".to_string()));
    assert!((r.final_score - 0.87).abs() < 1e-6);
}

#[tokio::test]
async fn remote_propagates_4xx_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
        .mount(&server)
        .await;

    let client = RemoteClient::new(&server.uri(), None, Duration::from_secs(3)).unwrap();
    let err = client.search("plan", None, 10).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("401") || msg.to_lowercase().contains("unauth"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn remote_forwards_api_key_header_when_set() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/search"))
        .and(header_exists("X-Skill-Search-Key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
        .mount(&server)
        .await;

    let client = RemoteClient::new(
        &server.uri(),
        Some("secret".to_string()),
        Duration::from_secs(3),
    )
    .unwrap();
    let results = client.search("plan", None, 5).await.unwrap();
    assert!(results.is_empty());
}
