mod support;
use mad_super_bot::auth::{AuthManager, Credentials, Endpoints};
use support::*;

#[tokio::test]
async fn validates_client_scopes_and_saved_bot_identity() {
    for body in [
        validated("wrong", "bot-id"),
        validated("client", "wrong"),
        serde_json::json!({"client_id":"client","user_id":"bot-id","scopes":[],"expires_in":3600})
            .to_string(),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = token_file(dir.path());
        let (url, _) = server(vec![reply(200, body)]).await;
        let result = AuthManager::load(
            Credentials::new("client", "secret"),
            path,
            Endpoints::local(&url),
        )
        .await;
        assert!(result.is_err());
        let error = result.err().unwrap().to_string();
        assert!(!error.contains("old-access") && !error.contains("secret"));
    }
}

#[tokio::test]
async fn simultaneous_unauthorized_requests_refresh_once_and_persist_rotated_token() {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = token_file(dir.path());
    let (url,requests)=server(vec![reply(200,validated("client","bot-id")), reply(200,r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600,"scope":["user:read:chat","user:write:chat"]}"#),reply(200,validated("client","bot-id"))]).await;
    let auth = AuthManager::load(
        Credentials::new("client", "secret"),
        &path,
        Endpoints::local(&url),
    )
    .await
    .unwrap();
    let (a, b) = tokio::join!(
        auth.refresh_after_rejection("old-access"),
        auth.refresh_after_rejection("old-access")
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(auth.access_token().await.unwrap(), "new-access");
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("new-refresh")
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.starts_with("POST /token"))
            .count(),
        1
    );
}

#[tokio::test]
async fn rotated_refresh_survives_validation_failure_but_session_stays_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let path = token_file(dir.path());
    let (url, requests) = server(vec![
        reply(200, validated("client", "bot-id")),
        reply(
            200,
            r#"{"access_token":"rotated-access","refresh_token":"rotated-refresh"}"#,
        ),
        reply(503, "{}"),
    ])
    .await;
    let auth = AuthManager::load(
        Credentials::new("client", "secret"),
        &path,
        Endpoints::local(&url),
    )
    .await
    .unwrap();
    assert!(auth.refresh_after_rejection("old-access").await.is_err());
    assert!(auth.access_token().await.is_err());
    assert!(
        std::fs::read_to_string(path)
            .unwrap()
            .contains("rotated-refresh")
    );
    assert_eq!(requests.lock().unwrap().len(), 3);
}

fn device(interval: u64, expires: u64) -> String {
    serde_json::json!({"device_code":"private-device-code","verification_uri":"https://twitch.tv/activate","user_code":"ABC","interval":interval,"expires_in":expires}).to_string()
}

#[tokio::test]
async fn device_polling_obeys_pending_and_slowdown_then_saves_validated_grant() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tokens.json");
    let (url, requests) = server(vec![
        reply(200, device(1, 30)),
        reply(400, r#"{"message":"authorization_pending"}"#),
        reply(400, r#"{"error":"slow_down"}"#),
        reply(
            200,
            r#"{"access_token":"authorized","refresh_token":"refresh"}"#,
        ),
        reply(200, validated("client", "new-bot")),
        reply(200, validated("client", "new-bot")),
    ])
    .await;
    let started = std::time::Instant::now();
    let auth = AuthManager::authorize(
        Credentials::new("client", "secret"),
        &path,
        Endpoints::local(&url),
        |uri, code| {
            assert_eq!(uri, "https://twitch.tv/activate");
            assert_eq!(code, "ABC");
        },
    )
    .await
    .unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_secs(8));
    assert_eq!(auth.user_id().await, "new-bot");
    assert_eq!(auth.access_token().await.unwrap(), "authorized");
    assert_eq!(requests.lock().unwrap().len(), 6);
}

#[tokio::test]
async fn device_denial_expiry_and_missing_scopes_do_not_create_a_token_file() {
    let denied = vec![
        reply(200, device(1, 20)),
        reply(400, r#"{"message":"access_denied"}"#),
    ];
    let expired = vec![reply(200, device(10, 1))];
    let bad_scope = vec![
        reply(200, device(1, 20)),
        reply(
            200,
            r#"{"access_token":"unauthorized","refresh_token":"refresh"}"#,
        ),
        reply(
            200,
            r#"{"client_id":"client","user_id":"bot","scopes":[],"expires_in":3600}"#,
        ),
    ];
    let server_expired = vec![
        reply(200, device(1, 20)),
        reply(400, r#"{"message":"expired_token"}"#),
    ];
    for replies in [denied, expired, bad_scope, server_expired] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let (url, _) = server(replies).await;
        let result = AuthManager::authorize(
            Credentials::new("client", "secret"),
            &path,
            Endpoints::local(&url),
            |_, _| {},
        )
        .await;
        assert!(result.is_err());
        assert!(!path.exists());
    }
}
