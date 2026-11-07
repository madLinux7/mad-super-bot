mod support;
use mad_super_bot::{
    auth::{AuthManager, Credentials, Endpoints},
    twitch::{Api, SendOutcome},
};
use support::*;

#[tokio::test]
async fn http_success_can_still_be_a_rejected_message_and_user_token_omits_source_flag() {
    let dir = tempfile::tempdir().unwrap();
    let path = token_file(dir.path());
    let (url,requests)=server(vec![reply(200,validated("client","bot-id")),reply(200,r#"{"data":[{"message_id":"id","is_sent":false,"drop_reason":{"code":"msg_banned","message":"private response"}}]}"#)]).await;
    let auth = AuthManager::load(
        Credentials::new("client", "secret"),
        path,
        Endpoints::local(&url),
    )
    .await
    .unwrap();
    let api = Api::new(auth, "channel").unwrap();
    assert!(matches!(
        api.send_chat("hello").await,
        SendOutcome::Rejected { .. }
    ));
    let requests = requests.lock().unwrap();
    assert!(requests[1].starts_with("POST /chat/messages"));
    assert!(!requests[1].contains("for_source_only"));
}

#[tokio::test]
async fn rate_and_unauthorized_responses_are_returned_without_blind_retries() {
    for (status, expected) in [(429, "rate"), (401, "auth"), (503, "uncertain")] {
        let dir = tempfile::tempdir().unwrap();
        let path = token_file(dir.path());
        let (url, requests) = server(vec![
            reply(200, validated("client", "bot-id")),
            (status, "Retry-After: 2\r\n".into(), "{}".into()),
        ])
        .await;
        let auth = AuthManager::load(
            Credentials::new("client", "secret"),
            path,
            Endpoints::local(&url),
        )
        .await
        .unwrap();
        let api = Api::new(auth, "channel").unwrap();
        match (expected, api.send_chat("hello").await) {
            ("rate", SendOutcome::RateLimited { retry_after }) => {
                assert!(retry_after.as_secs() >= 2)
            }
            ("auth", SendOutcome::Unauthorized { rejected_token }) => {
                assert_eq!(rejected_token, "old-access")
            }
            ("uncertain", SendOutcome::Uncertain) => {}
            other => panic!("unexpected result: {other:?}"),
        }
        assert_eq!(requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn successful_http_rate_headers_pause_the_sender_and_timeouts_are_not_retried() {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio::{io::AsyncReadExt, net::TcpListener};
    let dir = tempfile::tempdir().unwrap();
    let path = token_file(dir.path());
    let (url, _) = server(vec![
        reply(200, validated("client", "bot-id")),
        (
            200,
            "Ratelimit-Remaining: 0\r\nRetry-After: 3\r\n".into(),
            r#"{"data":[{"message_id":"id","is_sent":true}]}"#.into(),
        ),
    ])
    .await;
    let auth = AuthManager::load(
        Credentials::new("client", "secret"),
        path,
        Endpoints::local(&url),
    )
    .await
    .unwrap();
    let api = Api::new(auth, "channel").unwrap();
    assert!(matches!(
        api.send_chat("hello").await,
        SendOutcome::Sent { .. }
    ));
    assert!(api.rate_limit_delay().await >= Duration::from_secs(2));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let hits = count.clone();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut b = [0; 4096];
        let _ = stream.read(&mut b).await;
        hits.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(1)).await;
    });
    let dir = tempfile::tempdir().unwrap();
    let path = token_file(dir.path());
    let (oauth, _) = server(vec![reply(200, validated("client", "bot-id"))]).await;
    let mut endpoints = Endpoints::local(&oauth);
    endpoints.helix = format!("http://{address}");
    let auth = AuthManager::load(Credentials::new("client", "secret"), path, endpoints)
        .await
        .unwrap();
    let api = Api::with_http(
        auth,
        "channel",
        reqwest::Client::builder()
            .timeout(Duration::from_millis(30))
            .build()
            .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        api.send_chat("hello").await,
        SendOutcome::Uncertain
    ));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

fn welcome(id: &str, keepalive: u64) -> String {
    serde_json::json!({"metadata":{"message_type":"session_welcome","message_id":format!("welcome-{id}")},"payload":{"session":{"id":id,"keepalive_timeout_seconds":keepalive}}}).to_string()
}
fn chat(id: &str, text: &str) -> String {
    serde_json::json!({"metadata":{"message_type":"notification","message_id":id,"subscription_type":"channel.chat.message"},"payload":{"event":{"chatter_user_id":"viewer","chatter_user_login":"viewer","message":{"text":text}}}}).to_string()
}
async fn next_event(
    rx: &mut tokio::sync::mpsc::Receiver<mad_super_bot::twitch::TwitchEvent>,
) -> mad_super_bot::twitch::TwitchEvent {
    tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn directed_reconnect_keeps_old_socket_until_welcome_without_resubscribing_and_deduplicates()
{
    use futures_util::{SinkExt, StreamExt};
    use mad_super_bot::twitch::{EventSub, TwitchEvent};
    use tokio::{
        net::TcpListener,
        sync::{mpsc, watch},
    };
    use tokio_tungstenite::{accept_async, tungstenite::Message};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_url = format!("ws://{}", listener.local_addr().unwrap());
    let reconnect = ws_url.clone();
    let ws_server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut old = accept_async(socket).await.unwrap();
        old.send(Message::Text(welcome("old", 10).into()))
            .await
            .unwrap();
        old.send(Message::Text(chat("one", "!project").into()))
            .await
            .unwrap();
        old.send(Message::Text(chat("one", "!project").into()))
            .await
            .unwrap();
        old.send(Message::Text(serde_json::json!({"metadata":{"message_type":"notification","message_id":"unknown","subscription_type":"future.event"},"payload":{"event":{}}}).to_string().into())).await.unwrap();
        old.send(Message::Text(serde_json::json!({"metadata":{"message_type":"session_reconnect"},"payload":{"session":{"reconnect_url":reconnect}}}).to_string().into())).await.unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let mut new = accept_async(socket).await.unwrap();
        // This message must still reach the receiver while the replacement waits for Welcome.
        old.send(Message::Text(chat("two", "!socials").into()))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        new.send(Message::Text(welcome("new", 10).into()))
            .await
            .unwrap();
        new.send(Message::Text(chat("one", "!project").into()))
            .await
            .unwrap();
        new.send(Message::Text(chat("three", "!discord").into()))
            .await
            .unwrap();
        while let Some(Ok(_)) = new.next().await {}
    });
    let dir = tempfile::tempdir().unwrap();
    let path = token_file(dir.path());
    let (url, requests) = server(vec![
        reply(200, validated("client", "bot-id")),
        reply(202, "{}"),
        reply(202, "{}"),
        reply(202, "{}"),
    ])
    .await;
    let mut endpoints = Endpoints::local(&url);
    endpoints.websocket = ws_url;
    let auth = AuthManager::load(Credentials::new("client", "secret"), path, endpoints)
        .await
        .unwrap();
    let (tx, mut rx) = mpsc::channel(32);
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(EventSub::new(Api::new(auth, "channel").unwrap()).run(tx, shutdown));
    assert!(matches!(next_event(&mut rx).await, TwitchEvent::Ready));
    for expected in ["!project", "!socials", "!discord"] {
        match next_event(&mut rx).await {
            TwitchEvent::Chat { text, .. } => assert_eq!(text, expected),
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(80), rx.recv())
            .await
            .is_err()
    );
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.starts_with("POST /eventsub/subscriptions"))
            .count(),
        3
    );
    stop.send(true).unwrap();
    task.await.unwrap();
    ws_server.abort();
}

#[tokio::test]
async fn keepalives_extend_session_hard_disconnect_resubscribes_and_silence_expires() {
    use futures_util::SinkExt;
    use mad_super_bot::twitch::{EventSub, TwitchEvent};
    use std::time::Duration;
    use tokio::{
        net::TcpListener,
        sync::{mpsc, watch},
    };
    use tokio_tungstenite::{accept_async, tungstenite::Message};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_url = format!("ws://{}", listener.local_addr().unwrap());
    let ws_server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut old = accept_async(socket).await.unwrap();
        old.send(Message::Text(welcome("first", 1).into()))
            .await
            .unwrap();
        for _ in 0..2 {
            tokio::time::sleep(Duration::from_millis(600)).await;
            old.send(Message::Text(
                serde_json::json!({"metadata":{"message_type":"session_keepalive"},"payload":{}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        }
        old.send(Message::Text(chat("retained", "!first").into()))
            .await
            .unwrap();
        old.close(None).await.unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let mut new = accept_async(socket).await.unwrap();
        new.send(Message::Text(welcome("second", 1).into()))
            .await
            .unwrap();
        new.send(Message::Text(chat("retained", "!first").into()))
            .await
            .unwrap();
        new.send(Message::Text(chat("fresh", "!second").into()))
            .await
            .unwrap();
        // Keep the TCP socket open without Twitch keepalives.
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let dir = tempfile::tempdir().unwrap();
    let path = token_file(dir.path());
    let mut replies = vec![reply(200, validated("client", "bot-id"))];
    replies.extend((0..6).map(|_| reply(202, "{}")));
    let (url, requests) = server(replies).await;
    let mut endpoints = Endpoints::local(&url);
    endpoints.websocket = ws_url;
    let auth = AuthManager::load(Credentials::new("client", "secret"), path, endpoints)
        .await
        .unwrap();
    let (tx, mut rx) = mpsc::channel(32);
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(EventSub::new(Api::new(auth, "channel").unwrap()).run(tx, shutdown));
    assert!(matches!(next_event(&mut rx).await, TwitchEvent::Ready));
    assert!(matches!(next_event(&mut rx).await,TwitchEvent::Chat{text,..} if text=="!first"));
    assert!(matches!(
        next_event(&mut rx).await,
        TwitchEvent::Disconnected
    ));
    assert!(matches!(next_event(&mut rx).await, TwitchEvent::Ready));
    assert!(matches!(next_event(&mut rx).await,TwitchEvent::Chat{text,..} if text=="!second"));
    assert!(matches!(
        next_event(&mut rx).await,
        TwitchEvent::Disconnected
    ));
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.starts_with("POST /eventsub/subscriptions"))
            .count(),
        6
    );
    stop.send(true).unwrap();
    task.await.unwrap();
    ws_server.abort();
}

#[tokio::test]
async fn revoked_authorization_stops_eventsub_without_reconnecting() {
    use futures_util::SinkExt;
    use mad_super_bot::twitch::{EventSub, TwitchEvent};
    use tokio::{
        net::TcpListener,
        sync::{mpsc, watch},
    };
    use tokio_tungstenite::{accept_async, tungstenite::Message};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_url = format!("ws://{}", listener.local_addr().unwrap());
    let ws_server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(socket).await.unwrap();
        socket
            .send(Message::Text(welcome("first", 10).into()))
            .await
            .unwrap();
        socket.send(Message::Text(serde_json::json!({"metadata":{"message_type":"revocation"},"payload":{"subscription":{"status":"authorization_revoked","type":"channel.chat.message"}}}).to_string().into())).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    });
    let dir = tempfile::tempdir().unwrap();
    let path = token_file(dir.path());
    let (url, _) = server(vec![
        reply(200, validated("client", "bot-id")),
        reply(202, "{}"),
        reply(202, "{}"),
        reply(202, "{}"),
    ])
    .await;
    let mut endpoints = Endpoints::local(&url);
    endpoints.websocket = ws_url;
    let auth = AuthManager::load(Credentials::new("client", "secret"), path, endpoints)
        .await
        .unwrap();
    let (tx, mut rx) = mpsc::channel(32);
    let (_stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(EventSub::new(Api::new(auth, "channel").unwrap()).run(tx, shutdown));
    assert!(matches!(next_event(&mut rx).await, TwitchEvent::Ready));
    assert!(matches!(next_event(&mut rx).await, TwitchEvent::Fatal));
    tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    assert!(rx.recv().await.is_none());
    ws_server.abort();
}
