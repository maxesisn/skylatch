//! Local engine supervision and the loopback WebSocket link to it.

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use skylatch_proto::nsl1::{HEADER_SIZE, MAX_PAYLOAD, SUBPROTOCOL};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{protocol::WebSocketConfig, Message};
use url::Url;

/// How long to wait for a freshly started engine to accept connections.
const READY_TIMEOUT: Duration = Duration::from_secs(20);
const RETRY_INTERVAL: Duration = Duration::from_millis(100);
/// Take over the viewer and enable clipboard synchronization on the engine stream.
const STREAM_PATH: &str = "/stream?take=1&clipboard=1";

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("cannot start engine `{path}`: {source}")]
    Spawn {
        path: String,
        source: std::io::Error,
    },
    #[error("engine exited before it was ready ({0})")]
    Exited(String),
    #[error("engine did not become ready: {0}")]
    NotReady(String),
    #[error("no free loopback port: {0}")]
    Port(std::io::Error),
}

/// Where the local engine comes from.
pub enum EngineSource {
    /// Attach to an engine somebody else runs.
    Attach { base: Url },
    /// Start `skylatch-host-engine` on loopback and restart it if it dies.
    Spawn {
        bin: PathBuf,
        port: Option<u16>,
        args: Vec<String>,
        running: Option<Box<(Child, u16)>>,
    },
}

impl EngineSource {
    pub fn attach(base: Url) -> Self {
        Self::Attach { base }
    }

    pub fn spawn(bin: PathBuf, port: Option<u16>, args: Vec<String>) -> Self {
        Self::Spawn {
            bin,
            port,
            args,
            running: None,
        }
    }

    /// Makes sure the engine is running and returns the URL of its stream endpoint.
    pub async fn ensure_ready(&mut self) -> Result<Url, EngineError> {
        match self {
            Self::Attach { base } => {
                stream_url(base.host_str().unwrap_or("127.0.0.1"), base.port())
            }
            Self::Spawn {
                bin,
                port,
                args,
                running,
            } => {
                if let Some(boxed) = running {
                    if let Ok(Some(status)) = boxed.0.try_wait() {
                        tracing::warn!(%status, "engine exited; restarting");
                        *running = None;
                    }
                }
                if running.is_none() {
                    let chosen = match port {
                        Some(p) => *p,
                        None => free_loopback_port()?,
                    };
                    let child = Command::new(&*bin)
                        .arg("--no-viewer")
                        .arg("--insecure-local")
                        .args(["--bind", "127.0.0.1"])
                        .args(["--port", &chosen.to_string()])
                        .args(args.iter())
                        .kill_on_drop(true)
                        .spawn()
                        .map_err(|source| EngineError::Spawn {
                            path: bin.display().to_string(),
                            source,
                        })?;
                    tracing::info!(port = chosen, "started engine");
                    *running = Some(Box::new((child, chosen)));
                }
                let port = running.as_deref().map(|(_, p)| *p).unwrap_or_default();
                stream_url("127.0.0.1", Some(port))
            }
        }
    }

    /// Opens the link, waiting for a just-started engine to come up.
    pub async fn open_link(
        &mut self,
        generation: u64,
        events: mpsc::Sender<EngineEvent>,
    ) -> Result<LocalLink, EngineError> {
        let url = self.ensure_ready().await?;
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            match LocalLink::connect(&url, generation, events.clone()).await {
                Ok(link) => return Ok(link),
                Err(err) => {
                    if let Self::Spawn {
                        running: Some(boxed),
                        ..
                    } = self
                    {
                        if let Ok(Some(status)) = boxed.0.try_wait() {
                            return Err(EngineError::Exited(status.to_string()));
                        }
                    }
                    if Instant::now() >= deadline {
                        return Err(EngineError::NotReady(err));
                    }
                    tokio::time::sleep(RETRY_INTERVAL).await;
                }
            }
        }
    }
}

fn stream_url(host: &str, port: Option<u16>) -> Result<Url, EngineError> {
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let port = port.map(|p| format!(":{p}")).unwrap_or_default();
    Url::parse(&format!("ws://{host}{port}{STREAM_PATH}"))
        .map_err(|e| EngineError::NotReady(e.to_string()))
}

fn free_loopback_port() -> Result<u16, EngineError> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(EngineError::Port)?;
    Ok(listener.local_addr().map_err(EngineError::Port)?.port())
}

/// Event from the local engine socket, tagged with the link generation.
#[derive(Debug)]
pub enum EngineEvent {
    Binary(u64, Bytes),
    Closed(u64),
}

/// One WebSocket connection to the engine's `/stream`.
pub struct LocalLink {
    tx: mpsc::Sender<Message>,
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
    pub generation: u64,
}

impl LocalLink {
    async fn connect(
        url: &Url,
        generation: u64,
        events: mpsc::Sender<EngineEvent>,
    ) -> Result<Self, String> {
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| e.to_string())?;
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_static(SUBPROTOCOL),
        );
        // The engine owns monitor selection and the clipboard; all NSL1 messages, including
        // SL1, stay on this same socket and retain the engine's wire generation and payload.
        let limit = HEADER_SIZE + MAX_PAYLOAD;
        let config = WebSocketConfig::default()
            .max_message_size(Some(limit))
            .max_frame_size(Some(limit));
        let (ws, _) = tokio_tungstenite::connect_async_with_config(request, Some(config), true)
            .await
            .map_err(|e| e.to_string())?;
        let (mut sink, mut stream) = ws.split();
        let (tx, mut rx) = mpsc::channel::<Message>(256);
        let writer = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let closing = matches!(msg, Message::Close(_));
                if sink.send(msg).await.is_err() || closing {
                    break;
                }
            }
            let _ = sink.close().await;
        });
        let reader = tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(Message::Binary(data)) => {
                        if events
                            .send(EngineEvent::Binary(generation, data))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            let _ = events.send(EngineEvent::Closed(generation)).await;
        });
        Ok(Self {
            tx,
            reader,
            writer,
            generation,
        })
    }

    /// Forwards a binary `NSL1` message from the hub to the engine.
    pub async fn send(&self, data: Bytes) -> bool {
        self.tx.send(Message::Binary(data)).await.is_ok()
    }

    /// Sends a WebSocket close and tears the link down.
    pub async fn close(self) {
        let _ = self.tx.send(Message::Close(None)).await;
        drop(self.tx);
        self.reader.abort();
        if tokio::time::timeout(Duration::from_secs(2), self.writer)
            .await
            .is_err()
        {
            tracing::debug!("engine link close timed out");
        }
    }
}
