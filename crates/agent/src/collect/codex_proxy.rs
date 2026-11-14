//! JSONL adapter for the bridge's private app-server WebSocket.
use futures_util::{SinkExt, StreamExt};
use std::ffi::OsStr;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// Both the standalone agent and in-process Tauri executable support this
/// mode, so Node needs no WebSocket package.
pub fn run_if_requested() -> bool {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("codex-proxy")) {
        return false;
    }
    let path = match (args.next(), args.next()) {
        (Some(path), None) => path,
        _ => {
            eprintln!("usage: codex-proxy <private Unix socket | ws://127.0.0.1:PORT>");
            std::process::exit(2);
        }
    };
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let result = runtime.block_on(forward(&path));
        // Tokio stdin uses an uncancellable blocking read. This process mode
        // must exit on backend closure even while its parent keeps stdin open.
        runtime.shutdown_background();
        result
    })();
    if let Err(error) = result {
        eprintln!("Codex transport: {error}");
        std::process::exit(1);
    }
    true
}

async fn forward(target: &OsStr) -> Result<(), Box<dyn std::error::Error>> {
    // Android's bridge listens on loopback: Codex cannot use Unix sockets there.
    if let Some(url) = target.to_str().filter(|t| t.starts_with("ws://")) {
        let token = std::env::var("CDASH_CODEX_TOKEN")
            .map_err(|_| "CDASH_CODEX_TOKEN is not set")?;
        let mut request = url.into_client_request()?;
        request.headers_mut().insert(AUTHORIZATION, format!("Bearer {token}").parse()?);
        let (socket, _) = tokio_tungstenite::connect_async_with_config(request, Some(config()), false).await?;
        return pump(socket, tokio::io::stdin(), tokio::io::stdout()).await;
    }
    pump(unix(target).await?, tokio::io::stdin(), tokio::io::stdout()).await
}

/// 64 MiB covers an image-sized message (images go as data URLs, 32 MiB budget) and still bounds a misbehaving peer.
const MAX_FRAME: usize = 64 << 20;

fn config() -> WebSocketConfig {
    WebSocketConfig::default().max_message_size(Some(MAX_FRAME)).max_frame_size(Some(MAX_FRAME))
}

async fn unix(path: &OsStr) -> Result<WebSocketStream<tokio::net::UnixStream>, Box<dyn std::error::Error>> {
    let stream = tokio::net::UnixStream::connect(path).await?;
    let (socket, _) = tokio_tungstenite::client_async_with_config("ws://localhost/", stream, Some(config())).await?;
    Ok(socket)
}

async fn pump<S: AsyncRead + AsyncWrite + Unpin>(
    mut socket: WebSocketStream<S>,
    input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut input = BufReader::new(input).lines();
    loop {
        tokio::select! {
            line = input.next_line() => match line? {
                Some(line) => socket.send(Message::text(line)).await?,
                None => { socket.close(None).await?; return Ok(()); }
            },
            frame = socket.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    output.write_all(text.as_bytes()).await?;
                    output.write_all(b"\n").await?;
                    output.flush().await?;
                }
                Some(Ok(Message::Ping(_))) => socket.flush().await?,
                Some(Ok(Message::Close(_))) | None => return Ok(()),
                Some(Ok(Message::Pong(_))) => {},
                Some(Ok(_)) => return Err("expected a text app-server message".into()),
                Some(Err(error)) => return Err(error.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_app_server_message_over_the_websocket_defaults_reaches_the_bridge_whole() {
        let dir = std::env::temp_dir().join(format!("cdash-proxy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let big = "x".repeat(24 * 1024 * 1024); // one frame, over tungstenite's 16 MiB default
        let sent = big.clone();
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            ws.send(Message::text(sent)).await.unwrap();
            ws.close(None).await.unwrap();
        });
        let (_keep_open, input) = tokio::io::duplex(64); // stdin that never ends
        let mut out = Vec::new();
        pump(unix(path.as_os_str()).await.unwrap(), input, &mut out).await.unwrap();
        assert_eq!(out.len(), big.len() + 1);
    }
}
