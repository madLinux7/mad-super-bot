#![allow(dead_code)]
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

pub async fn server(replies: Vec<(u16, String, String)>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    tokio::spawn(async move {
        for (status, headers, body) in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            recorded.lock().unwrap().push(request);
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (url, requests)
}

pub fn reply(status: u16, body: impl Into<String>) -> (u16, String, String) {
    (status, String::new(), body.into())
}
pub fn validated(client: &str, user: &str) -> String {
    serde_json::json!({"client_id":client,"user_id":user,"login":"bot","scopes":["user:read:chat","user:write:chat"],"expires_in":3600}).to_string()
}
pub fn tokens() -> String {
    serde_json::json!({"access_token":"old-access","refresh_token":"old-refresh","user_id":"bot-id"}).to_string()
}
#[cfg(windows)]
#[path = "../../src/token_file.rs"]
mod token_permissions;

pub fn token_file(dir: &std::path::Path) -> std::path::PathBuf {
    let file = dir.join("tokens.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&file, tokens()).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[cfg(windows)]
    {
        use std::io::Write;
        let mut private = token_permissions::create(dir).unwrap();
        private.write_all(tokens().as_bytes()).unwrap();
        private.persist(&file).unwrap();
    }
    file
}

pub async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    loop {
        let mut part = [0; 4096];
        let count = socket.read(&mut part).await.unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&part[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&bytes[..end]);
            let length = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|n| n.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8(bytes).unwrap()
}
