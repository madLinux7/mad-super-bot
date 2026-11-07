mod support;
use futures_util::SinkExt;
use mad_super_bot::{
    auth::{AuthManager, Credentials, Endpoints},
    config::ConfigPaths,
    runtime,
    twitch::Api,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::TcpListener,
    sync::{mpsc, watch},
};
#[derive(Clone)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn process_combines_commands_reload_loops_and_shutdown_with_network_doubles() {
    let logs = LogBuffer(Arc::new(Mutex::new(Vec::new())));
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || writer.clone())
        .finish();
    let _logging = tracing::subscriber::set_default(subscriber);
    let dir = tempfile::tempdir().unwrap();
    let commands = include_str!("fixtures/config.yml")
        .split("# live-only or always-on")
        .next()
        .unwrap()
        .replace("user_cooldown: 1", "user_cooldown: 17");
    std::fs::write(dir.path().join("config.yml"), format!("{commands}\nmode: always-on\nloops:\n  hint:\n    interval: 0.002\n    message: loop text\n")).unwrap();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", http.local_addr().unwrap());
    let sent = Arc::new(Mutex::new(Vec::<String>::new()));
    let messages = sent.clone();
    let times = Arc::new(Mutex::new(Vec::<std::time::Instant>::new()));
    let timestamps = times.clone();
    let api_task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = http.accept().await.unwrap();
            let request = support::read_request(&mut stream).await;
            let body = if request.starts_with("GET /validate") {
                support::validated("client", "bot-id")
            } else if request.starts_with("GET /streams") {
                "{\"data\":[]}".into()
            } else if request.starts_with("POST /chat/messages") {
                let value: serde_json::Value =
                    serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                messages
                    .lock()
                    .unwrap()
                    .push(value["message"].as_str().unwrap().into());
                timestamps.lock().unwrap().push(std::time::Instant::now());
                "{\"data\":[{\"message_id\":\"sent\",\"is_sent\":true}]}".into()
            } else {
                "{}".into()
            };
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    let ws = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_url = format!("ws://{}", ws.local_addr().unwrap());
    let (chat_tx, mut chats) = mpsc::channel::<String>(8);
    let ws_task = tokio::spawn(async move {
        let (stream, _) = ws.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::json!({"metadata":{"message_type":"session_welcome"},"payload":{"session":{"id":"test","keepalive_timeout_seconds":10}}}).to_string().into())).await.unwrap();
        while let Some(text) = chats.recv().await {
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(text.into()))
                .await
                .unwrap();
        }
    });
    let mut endpoints = Endpoints::local(&base);
    endpoints.websocket = ws_url;
    let auth = AuthManager::load(
        Credentials::new("client", "secret"),
        support::token_file(dir.path()),
        endpoints,
    )
    .await
    .unwrap();
    let api = Api::new(auth, "channel").unwrap();
    let paths = ConfigPaths::new(dir.path().join("config.yml")).unwrap();
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(runtime::run(paths.clone(), api, shutdown));
    let event = |id: &str, user: &str| {
        serde_json::json!({"metadata":{"message_type":"notification","message_id":id,"subscription_type":"channel.chat.message"},"payload":{"event":{"chatter_user_id":user,"chatter_user_login":"viewer","message":{"text":"!project"}}}}).to_string()
    };
    chat_tx.send(event("one", "one")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if !sent.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        sent.lock().unwrap()[0],
        commands
            .split("message: \"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
    );
    std::fs::write(&paths.file, "commands: [invalid").unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    chat_tx.send(event("invalid-save", "third")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if sent
                .lock()
                .unwrap()
                .iter()
                .filter(|s| s.starts_with("Aktuell arbeite ich"))
                .count()
                == 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    std::fs::write(
        &paths.file,
        commands
            .replace("Aktuell arbeite ich", "Changed answer:")
            .replace("channel: dein_channel", "channel: different_channel")
            .replace("bot_credentials: bitwarden", "bot_credentials: yml")
            .replace("# client_id: deine_client_id", "client_id: changed-client")
            .replace(
                "# client_secret: dein_client_secret",
                "client_secret: sensitive-reload-secret",
            )
            .replace(
                "token_file: tokens.json",
                "token_file: unused-new-tokens.json",
            ),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    // The same user's cooldown survives reload; a new user sees the latest text.
    chat_tx.send(event("two", "one")).await.unwrap();
    chat_tx.send(event("three", "two")).await.unwrap();
    let mut barrier: serde_json::Value =
        serde_json::from_str(&event("barrier", "barrier-user")).unwrap();
    barrier["payload"]["event"]["message"]["text"] = serde_json::json!("!discord");
    chat_tx.send(barrier.to_string()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if sent
                .lock()
                .unwrap()
                .iter()
                .any(|s| s == "https://discord.com/invite/YH9J6ptHt7")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        sent.lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("Changed answer:"))
            .count(),
        1
    );
    assert!(sent.lock().unwrap().iter().any(|s| s == "loop text"));
    let diagnostic = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(diagnostic.contains("restart required"), "{diagnostic}");
    assert!(
        diagnostic.contains("keeping last valid snapshot"),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("sensitive-reload-secret"));
    assert!(!dir.path().join("unused-new-tokens.json").exists());
    assert!(times.lock().unwrap().len() >= 3);
    assert!(
        times
            .lock()
            .unwrap()
            .windows(2)
            .all(|pair| pair[1].duration_since(pair[0]) >= Duration::from_secs(1))
    );
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    api_task.abort();
    ws_task.abort();
}

#[tokio::test]
async fn live_only_ignores_status_queries_from_before_reconnect_and_newer_events() {
    let dir = tempfile::tempdir().unwrap();
    let commands = include_str!("fixtures/config.yml")
        .split("# live-only or always-on")
        .next()
        .unwrap();
    std::fs::write(dir.path().join("config.yml"), format!("{commands}\nmode: live-only\nloops:\n  hint:\n    interval: 0.002\n    message: live hint\n")).unwrap();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", http.local_addr().unwrap());
    let sent = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = sent.clone();
    let (queries, mut pending_queries) = mpsc::channel::<tokio::sync::oneshot::Sender<bool>>(4);
    let api_task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            let (mut stream, _) = http.accept().await.unwrap();
            let recorded = recorded.clone();
            let queries = queries.clone();
            tasks.spawn(async move {
                let request = support::read_request(&mut stream).await;
                let body = if request.starts_with("GET /validate") {
                    support::validated("client", "bot-id")
                } else if request.starts_with("GET /streams") {
                    let (reply, result) = tokio::sync::oneshot::channel();
                    queries.send(reply).await.unwrap();
                    if result.await.unwrap_or(false) { "{\"data\":[{\"id\":\"live\"}]}".to_owned() } else { "{\"data\":[]}".to_owned() }
                } else if request.starts_with("POST /chat/messages") {
                    let value: serde_json::Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                    recorded.lock().unwrap().push(value["message"].as_str().unwrap().to_owned());
                    "{\"data\":[{\"message_id\":\"sent\",\"is_sent\":true}]}".to_owned()
                } else { "{}".to_owned() };
                let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
            });
        }
    });
    let ws = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_url = format!("ws://{}", ws.local_addr().unwrap());
    let (disconnect, disconnected) = tokio::sync::oneshot::channel();
    let (connected, reconnected) = tokio::sync::oneshot::channel();
    let (frames, mut to_send) = mpsc::channel::<String>(8);
    let ws_task = tokio::spawn(async move {
        let (stream, _) = ws.accept().await.unwrap();
        let mut first_socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        first_socket.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::json!({"metadata":{"message_type":"session_welcome"},"payload":{"session":{"id":"first","keepalive_timeout_seconds":10}}}).to_string().into())).await.unwrap();
        disconnected.await.unwrap();
        drop(first_socket);
        let (stream, _) = ws.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::json!({"metadata":{"message_type":"session_welcome"},"payload":{"session":{"id":"second","keepalive_timeout_seconds":10}}}).to_string().into())).await.unwrap();
        let _ = connected.send(());
        while let Some(frame) = to_send.recv().await {
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(frame.into()))
                .await
                .unwrap();
        }
    });
    let mut endpoints = Endpoints::local(&base);
    endpoints.websocket = ws_url;
    let auth = AuthManager::load(
        Credentials::new("client", "secret"),
        support::token_file(dir.path()),
        endpoints,
    )
    .await
    .unwrap();
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(runtime::run(
        ConfigPaths::new(dir.path().join("config.yml")).unwrap(),
        Api::new(auth, "channel").unwrap(),
        shutdown,
    ));
    let first_query = tokio::time::timeout(Duration::from_secs(3), pending_queries.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(sent.lock().unwrap().is_empty());
    disconnect.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(4), reconnected)
        .await
        .unwrap()
        .unwrap();
    let stream_event = |id: &str, kind: &str| {
        serde_json::json!({"metadata":{"message_type":"notification","message_id":id,"subscription_type":kind},"payload":{"event":{}}}).to_string()
    };
    frames
        .send(stream_event("online", "stream.online"))
        .await
        .unwrap();
    async fn wait_for(sent: &Arc<Mutex<Vec<String>>>, text: &str, count: usize) {
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                if sent
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|message| message.as_str() == text)
                    .count()
                    >= count
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    wait_for(&sent, "live hint", 1).await;
    first_query.send(false).unwrap();
    let second_query = tokio::time::timeout(Duration::from_secs(3), pending_queries.recv())
        .await
        .unwrap()
        .unwrap();
    wait_for(&sent, "live hint", 2).await;
    frames
        .send(stream_event("offline", "stream.offline"))
        .await
        .unwrap();
    let command = |id: &str, text: &str| {
        serde_json::json!({"metadata":{"message_type":"notification","message_id":id,"subscription_type":"channel.chat.message"},"payload":{"event":{"chatter_user_id":id,"chatter_user_login":id,"message":{"text":text}}}}).to_string()
    };
    frames
        .send(command("offline-barrier", "!discord"))
        .await
        .unwrap();
    wait_for(&sent, "https://discord.com/invite/YH9J6ptHt7", 1).await;
    second_query.send(true).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    frames
        .send(command("final-barrier", "!youtube"))
        .await
        .unwrap();
    wait_for(&sent, "https://www.youtube.com/@madLinux", 1).await;
    assert_eq!(
        sent.lock()
            .unwrap()
            .iter()
            .filter(|text| text.as_str() == "live hint")
            .count(),
        2
    );
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    api_task.abort();
    ws_task.abort();
}
