//! Async viewing sessions and the `NSL1` WebSocket data pump.

use crate::transport::{self, Socket, WRITE_TIMEOUT};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use skylatch_proto::nsl1::{
    Cursor, InputEvent, InputRecord, InputStatus, Message, MonitorInfo, Nsl1Error, INPUT_SIZE,
    MAX_CLIPBOARD_DATA, MAX_MONITOR_ID,
};
use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    },
};
use tokio::{
    sync::{mpsc, watch},
    time::timeout,
};
use tokio_tungstenite::tungstenite;

const OUTBOUND_CAPACITY: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("invalid WebSocket or hub base URL")]
    InvalidUrl,
    #[error("{0}")]
    InvalidArgument(&'static str),
    #[error("{0} timed out")]
    Timeout(&'static str),
    #[error("WebSocket: {0}")]
    WebSocket(Box<tungstenite::Error>),
    #[error("hub control JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("hub rejected the request ({code}): {message}")]
    Hub { code: String, message: String },
    #[error("hub ended the session: {0}")]
    SessionClosed(String),
    #[error("{0}")]
    Unexpected(&'static str),
    #[error("NSL1: {0}")]
    Protocol(#[from] Nsl1Error),
}

impl From<tungstenite::Error> for ClientError {
    fn from(error: tungstenite::Error) -> Self {
        Self::WebSocket(Box::new(error))
    }
}

/// Which data path was established during the connection handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMode {
    Direct,
    Relay,
}

/// One H.264 Annex B access unit and its presentation/ACK metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub generation: u32,
    pub sequence: u32,
    pub width: u16,
    pub height: u16,
    pub capture_us: u64,
    pub is_keyframe: bool,
    pub annex_b: Bytes,
}

/// Events are delivered serially on the networking task. Keep the callback short; dispatch
/// expensive work elsewhere. No client events follow `Disconnected`.
#[derive(Debug)]
pub enum Event {
    Frame(Frame),
    Cursor(Cursor),
    ClipboardNotice,
    MonitorList(Vec<MonitorInfo>),
    ClipboardData(String),
    /// The host answered an input request. Sequence 0 answers the smooth-wheel query the session
    /// sends after the first host message; `Accepted` there enables smooth-wheel records.
    InputResponse {
        sequence: u32,
        status: InputStatus,
    },
    Disconnected(Result<(), ClientError>),
}

/// A send enqueues one message without waiting for the network.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    #[error("session is closed")]
    Closed,
    #[error("outbound queue is full")]
    QueueFull,
    #[error("no NSL1 generation received yet")]
    NotReady,
    #[error("invalid ACK generation or sequence")]
    InvalidAck,
    #[error("invalid input record: {0}")]
    InvalidInput(Nsl1Error),
    #[error("invalid SL1 message: {0}")]
    InvalidSl1(Nsl1Error),
    #[error("the host has not accepted smooth-wheel input")]
    Unsupported,
}

/// A connected session handle. Safe to share between threads. Dropping it requests disconnect.
pub struct SessionClient {
    mode: TransportMode,
    outbound: mpsc::Sender<Bytes>,
    stop: watch::Sender<bool>,
    generation: Arc<AtomicU32>,
    smooth_wheel: Arc<AtomicBool>,
}

impl SessionClient {
    /// Authenticate, open a relay and optionally try the host's advertised direct endpoints.
    /// A failed direct probe leaves the already-open relay in use.
    pub async fn connect_hub(
        hub_url: &str,
        token: &str,
        device_id: &str,
        prefer_direct: bool,
        callback: impl FnMut(Event) + Send + 'static,
    ) -> Result<Self, ClientError> {
        let connection = Connection::hub(hub_url, token, device_id, prefer_direct).await?;
        let (client, pump) = connection.start(callback);
        tokio::spawn(pump);
        Ok(client)
    }

    /// Connect to a standalone listener. Accepts a `ws://` or `wss://` origin or `/stream` URL.
    /// With a token, it must be the host's 64-hex-digit direct token, not the hub account token.
    pub async fn connect_direct(
        ws_url: &str,
        token: Option<&str>,
        callback: impl FnMut(Event) + Send + 'static,
    ) -> Result<Self, ClientError> {
        let connection = Connection::direct(ws_url, token).await?;
        let (client, pump) = connection.start(callback);
        tokio::spawn(pump);
        Ok(client)
    }

    pub fn transport_mode(&self) -> TransportMode {
        self.mode
    }

    /// True once the host accepted the smooth-wheel query; until then smooth-wheel sends fail.
    pub fn smooth_wheel(&self) -> bool {
        self.smooth_wheel.load(Ordering::Relaxed)
    }

    pub fn send_ack(
        &self,
        generation: u32,
        sequence: u32,
        viewer_wait_us: u32,
    ) -> Result<(), SendError> {
        if generation == 0 || sequence == 0 {
            return Err(SendError::InvalidAck);
        }
        self.enqueue(
            Message::Ack {
                generation,
                sequence,
                viewer_wait_us: Some(viewer_wait_us),
            }
            .encode()
            .map_err(|_| SendError::InvalidAck)?,
        )
    }

    /// Sends one validated input record using the latest generation received from the host.
    /// Smooth-wheel records fail with [`SendError::Unsupported`] until the host accepts them,
    /// because hosts without them close the stream.
    pub fn send_input(&self, record: [u8; INPUT_SIZE]) -> Result<(), SendError> {
        let generation = self.current_generation()?;
        let record = InputRecord::decode(&record).map_err(SendError::InvalidInput)?;
        if matches!(record.event, InputEvent::SmoothWheel { .. }) && !self.smooth_wheel() {
            return Err(SendError::Unsupported);
        }
        self.enqueue(
            Message::Input {
                generation,
                sequence: 0,
                record,
            }
            .encode()
            .map_err(SendError::InvalidInput)?,
        )
    }

    /// Queues UTF-8 clipboard text for the host. Empty text clears the text clipboard.
    pub fn send_clipboard(&self, text: &str) -> Result<(), SendError> {
        if text.len() > MAX_CLIPBOARD_DATA {
            return Err(SendError::InvalidSl1(Nsl1Error::PayloadTooLarge(
                text.len(),
            )));
        }
        let generation = self.current_generation()?;
        self.enqueue(
            Message::ClipboardData {
                generation,
                text: text.into(),
            }
            .encode()
            .map_err(SendError::InvalidSl1)?,
        )
    }

    /// Queues a request to select a display from the host's latest monitor list.
    pub fn select_monitor(&self, monitor_id: &str) -> Result<(), SendError> {
        if monitor_id.len() > MAX_MONITOR_ID {
            return Err(SendError::InvalidSl1(Nsl1Error::PayloadTooLarge(
                monitor_id.len(),
            )));
        }
        if monitor_id.is_empty() {
            return Err(SendError::InvalidSl1(Nsl1Error::InvalidPayload(
                "select monitor",
            )));
        }
        let generation = self.current_generation()?;
        self.enqueue(
            Message::SelectMonitor {
                generation,
                id: monitor_id.into(),
            }
            .encode()
            .map_err(SendError::InvalidSl1)?,
        )
    }

    fn current_generation(&self) -> Result<u32, SendError> {
        if *self.stop.borrow() || self.outbound.is_closed() {
            return Err(SendError::Closed);
        }
        match self.generation.load(Ordering::Relaxed) {
            0 => Err(SendError::NotReady),
            generation => Ok(generation),
        }
    }

    fn enqueue(&self, bytes: Bytes) -> Result<(), SendError> {
        if *self.stop.borrow() {
            return Err(SendError::Closed);
        }
        self.outbound.try_send(bytes).map_err(|e| match e {
            mpsc::error::TrySendError::Closed(_) => SendError::Closed,
            mpsc::error::TrySendError::Full(_) => SendError::QueueFull,
        })
    }

    /// Request shutdown without blocking. Queued messages need not be sent after this call.
    pub fn disconnect(&self) {
        self.stop.send_replace(true);
    }
}

impl Drop for SessionClient {
    fn drop(&mut self) {
        self.disconnect();
    }
}

pub(crate) struct Connection {
    socket: Socket,
    mode: TransportMode,
}

impl Connection {
    pub(crate) async fn hub(
        url: &str,
        token: &str,
        device: &str,
        prefer_direct: bool,
    ) -> Result<Self, ClientError> {
        // Ticket-aware agents share one engine between direct and relay. Try the direct connection
        // before opening a relay, so fallback never takes over its own viewer.
        if prefer_direct {
            if let Ok(access) = crate::http::direct_session(url, token, device).await {
                if let Some(socket) =
                    transport::probe_direct(&access.endpoints, Some(&access.ticket)).await
                {
                    return Ok(Self {
                        socket,
                        mode: TransportMode::Direct,
                    });
                }
            }
        }
        let (mut relay, endpoints) = transport::open_relay(url, token, device).await?;
        if prefer_direct && !endpoints.is_empty() {
            if let Some(socket) = transport::probe_direct(&endpoints, None).await {
                transport::close(&mut relay, true).await;
                return Ok(Self {
                    socket,
                    mode: TransportMode::Direct,
                });
            }
        }
        Ok(Self {
            socket: relay,
            mode: TransportMode::Relay,
        })
    }

    pub(crate) async fn direct(url: &str, token: Option<&str>) -> Result<Self, ClientError> {
        let socket = transport::open_direct(url, token).await?;
        Ok(Self {
            socket,
            mode: TransportMode::Direct,
        })
    }

    pub(crate) fn start(
        self,
        mut callback: impl FnMut(Event) + Send + 'static,
    ) -> (SessionClient, impl Future<Output = ()> + Send + 'static) {
        let Self { mut socket, mode } = self;
        let (outbound, mut queue) = mpsc::channel(OUTBOUND_CAPACITY);
        let (stop, mut stopped) = watch::channel(false);
        let generation = Arc::new(AtomicU32::new(0));
        let smooth_wheel = Arc::new(AtomicBool::new(false));
        let client = SessionClient {
            mode,
            outbound,
            stop,
            generation: generation.clone(),
            smooth_wheel: smooth_wheel.clone(),
        };
        let pump = async move {
            let relay = mode == TransportMode::Relay;
            let result = tokio::select! {
                biased;
                _ = stopped.changed() => Ok(()),
                result = pump_messages(&mut socket, &mut queue, &generation, &smooth_wheel, relay, &mut callback) => result,
            };
            queue.close();
            transport::close(&mut socket, relay).await;
            callback(Event::Disconnected(result));
        };
        (client, pump)
    }
}

/// Asks whether the host accepts smooth-wheel records. Older hosts ignore an ACK of sequence 0.
async fn send_smooth_wheel_query(socket: &mut Socket, generation: u32) -> Result<(), ClientError> {
    let query = Message::Ack {
        generation,
        sequence: 0,
        viewer_wait_us: None,
    }
    .encode()?;
    timeout(
        WRITE_TIMEOUT,
        socket.send(tungstenite::Message::Binary(query)),
    )
    .await
    .map_err(|_| ClientError::Timeout("WebSocket write"))??;
    Ok(())
}

async fn pump_messages(
    socket: &mut Socket,
    queue: &mut mpsc::Receiver<Bytes>,
    generation: &AtomicU32,
    smooth_wheel: &AtomicBool,
    relay: bool,
    callback: &mut (impl FnMut(Event) + Send),
) -> Result<(), ClientError> {
    loop {
        tokio::select! {
            incoming = socket.next() => match incoming {
                Some(Ok(tungstenite::Message::Binary(bytes))) => {
                    let (current, event) = transport::decode_stream_event(&bytes)?;
                    if generation.swap(current, Ordering::Relaxed) == 0 {
                        send_smooth_wheel_query(socket, current).await?;
                    }
                    if let Event::InputResponse { sequence: 0, status } = event {
                        smooth_wheel.store(status == InputStatus::Accepted, Ordering::Relaxed);
                    }
                    callback(event);
                    // A socket with many already-buffered frames can remain ready without
                    // touching Tokio I/O. Cooperate so the enclosing cancel select is polled.
                    tokio::task::consume_budget().await;
                }
                Some(Ok(tungstenite::Message::Text(text))) if relay => {
                    transport::decode_control(&text)?;
                    return Err(ClientError::Unexpected("unexpected hub control message during streaming"));
                }
                Some(Ok(tungstenite::Message::Ping(_))) => {
                    timeout(WRITE_TIMEOUT, socket.flush()).await.map_err(|_| ClientError::Timeout("WebSocket pong"))??;
                }
                Some(Ok(tungstenite::Message::Pong(_))) => (),
                Some(Ok(tungstenite::Message::Close(_))) => {
                    // Tungstenite queued the close reply while reading the peer's close.
                    let _ = timeout(WRITE_TIMEOUT, socket.flush()).await;
                    return Ok(());
                }
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(()),
                _ => return Err(ClientError::Unexpected("unexpected WebSocket message during streaming")),
            },
            bytes = queue.recv() => {
                let Some(bytes) = bytes else { return Ok(()) };
                timeout(WRITE_TIMEOUT, socket.send(tungstenite::Message::Binary(bytes)))
                    .await.map_err(|_| ClientError::Timeout("WebSocket write"))??;
            }
        }
    }
}
