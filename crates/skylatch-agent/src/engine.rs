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
    Spawn(Spawner),
}

pub struct Spawner {
    bin: PathBuf,
    port: Option<u16>,
    args: Vec<String>,
    running: Option<Running>,
    /// Set once the engine was needed; it then keeps running.
    wanted: bool,
    /// Engines stopped because the console session changed.
    switches: u64,
    /// Started from Session 0, so the engine must run in the active console session.
    console: bool,
}

struct Running {
    child: EngineChild,
    port: u16,
}

enum EngineChild {
    Local(Box<Child>),
    #[cfg(windows)]
    Console(crate::engine_win_session::ConsoleChild),
}

impl EngineChild {
    fn try_exit(&mut self) -> Option<String> {
        match self {
            Self::Local(child) => match child.try_wait() {
                Ok(Some(status)) => Some(status.to_string()),
                _ => None,
            },
            #[cfg(windows)]
            Self::Console(child) => child.try_exit(),
        }
    }
}

impl EngineSource {
    pub fn attach(base: Url) -> Self {
        Self::Attach { base }
    }

    pub fn spawn(bin: PathBuf, port: Option<u16>, args: Vec<String>) -> Self {
        #[cfg(windows)]
        let console = crate::engine_win_session::in_session_zero();
        #[cfg(not(windows))]
        let console = false;
        Self::Spawn(Spawner {
            bin,
            port,
            args,
            running: None,
            wanted: false,
            switches: 0,
            console,
        })
    }

    /// Whether the engine is started into the active console session from Session 0.
    pub fn follows_console(&self) -> bool {
        matches!(self, Self::Spawn(spawn) if spawn.console)
    }

    /// How often a running engine was stopped to move it to a new console session.
    pub fn session_switches(&self) -> u64 {
        match self {
            Self::Attach { .. } => 0,
            Self::Spawn(spawn) => spawn.switches,
        }
    }

    /// Makes sure the engine is running and returns the URL of its stream endpoint.
    pub async fn ensure_ready(&mut self) -> Result<Url, EngineError> {
        match self {
            Self::Attach { base } => {
                stream_url(base.host_str().unwrap_or("127.0.0.1"), base.port())
            }
            Self::Spawn(spawn) => {
                let port = spawn.ready_port().await?;
                stream_url("127.0.0.1", Some(port))
            }
        }
    }

    /// Moves a running engine to the active console session, or restarts a needed one.
    #[cfg(windows)]
    pub(crate) fn follow_console(&mut self, active: Option<u32>) -> Result<(), EngineError> {
        use crate::engine_win_session::plan;
        match self {
            Self::Spawn(spawn) if spawn.console => {
                let step = plan(spawn.running_session(), active, spawn.wanted);
                spawn.apply(step)
            }
            _ => Ok(()),
        }
    }

    /// Opens the link, waiting for a just-started engine to come up.
    pub async fn open_link(
        &mut self,
        generation: u64,
        events: mpsc::Sender<EngineEvent>,
    ) -> Result<LocalLink, EngineError> {
        let url = self.ensure_ready().await?;
        let switches = self.session_switches();
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            let connect = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                LocalLink::connect(&url, generation, events.clone()),
            );
            match connect
                .await
                .map_err(|_| EngineError::NotReady("WebSocket handshake timed out".into()))?
            {
                Ok(mut link) => {
                    link.switches = switches;
                    return Ok(link);
                }
                Err(err) => {
                    if let Self::Spawn(spawn) = self {
                        if let Some(status) = spawn.reap() {
                            return Err(EngineError::Exited(status));
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

impl Spawner {
    /// Drops an exited engine and returns its exit status.
    fn reap(&mut self) -> Option<String> {
        let status = self.running.as_mut()?.child.try_exit()?;
        self.running = None;
        Some(status)
    }

    fn engine_args(&self, port: u16) -> Vec<String> {
        let mut args: Vec<String> = ["--no-viewer", "--insecure-local", "--bind", "127.0.0.1"]
            .map(String::from)
            .into();
        args.extend(["--port".into(), port.to_string()]);
        args.extend(self.args.iter().cloned());
        args
    }

    fn launch_port(&self) -> Result<u16, EngineError> {
        match self.port {
            Some(p) => Ok(p),
            None => free_loopback_port(),
        }
    }

    async fn ready_port(&mut self) -> Result<u16, EngineError> {
        self.wanted = true;
        if let Some(status) = self.reap() {
            tracing::warn!(%status, "engine exited; restarting");
        }
        #[cfg(windows)]
        if self.console {
            let active = crate::engine_win_session::wait_for_console(READY_TIMEOUT)
                .await
                .ok_or_else(|| {
                    EngineError::NotReady("no session is attached to the console".into())
                })?;
            let step = crate::engine_win_session::plan(self.running_session(), Some(active), true);
            self.apply(step)?;
        }
        if self.running.is_none() {
            let chosen = self.launch_port()?;
            let child = Command::new(&self.bin)
                .args(self.engine_args(chosen))
                .kill_on_drop(true)
                .spawn()
                .map_err(|source| EngineError::Spawn {
                    path: self.bin.display().to_string(),
                    source,
                })?;
            tracing::info!(port = chosen, "started engine");
            self.running = Some(Running {
                child: EngineChild::Local(Box::new(child)),
                port: chosen,
            });
        }
        Ok(self.running.as_ref().map(|r| r.port).unwrap_or_default())
    }

    #[cfg(windows)]
    fn running_session(&mut self) -> Option<u32> {
        if let Some(status) = self.reap() {
            tracing::warn!(%status, "engine exited");
        }
        match &self.running.as_ref()?.child {
            EngineChild::Console(child) => Some(child.session),
            EngineChild::Local(_) => None,
        }
    }

    #[cfg(windows)]
    fn apply(&mut self, step: crate::engine_win_session::Step) -> Result<(), EngineError> {
        use crate::engine_win_session::{ConsoleChild, Step};
        let session = match step {
            Step::Keep | Step::Wait => return Ok(()),
            Step::Launch(to) => to,
            Step::Switch { from, to } => {
                tracing::info!(from, to, "console session changed; moving the engine");
                self.running = None;
                self.switches += 1;
                to
            }
        };
        let port = self.launch_port()?;
        let child = ConsoleChild::launch(&self.bin, &self.engine_args(port), session).map_err(
            |source| EngineError::Spawn {
                path: self.bin.display().to_string(),
                source,
            },
        )?;
        tracing::info!(
            port,
            session,
            pid = child.pid,
            "started engine in the console session"
        );
        self.running = Some(Running {
            child: EngineChild::Console(child),
            port,
        });
        Ok(())
    }
}

fn stream_url(host: &str, port: Option<u16>) -> Result<Url, EngineError> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
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
    /// [`EngineSource::session_switches`] when the link was opened.
    pub switches: u64,
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
            switches: 0,
        })
    }

    /// Forwards a binary `NSL1` message from the hub to the engine.
    pub async fn send(&self, data: Bytes) -> bool {
        self.tx.send(Message::Binary(data)).await.is_ok()
    }

    /// Sends a WebSocket close and tears the link down.
    pub async fn close(mut self) {
        self.reader.abort();
        if tokio::time::timeout(Duration::from_secs(2), async {
            let _ = self.tx.send(Message::Close(None)).await;
            let _ = (&mut self.writer).await;
        })
        .await
        .is_err()
        {
            tracing::debug!("engine link close timed out");
        }
    }
}

impl Drop for LocalLink {
    fn drop(&mut self) {
        self.reader.abort();
        self.writer.abort();
    }
}
