//! Audio negotiation and decoding against a scripted host on the direct path.

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use opusic_sys as opus;
use skylatch_client_core::{
    ffi::{
        skylatch_connect_direct, skylatch_destroy, skylatch_send_ack, skylatch_set_audio_enabled,
        SkylatchCallbacks, SkylatchClient, SKYLATCH_ERR_UNSUPPORTED, SKYLATCH_OK,
    },
    AudioPcm, Event, SessionClient,
};
use skylatch_proto::nsl1::{AudioFormat, InputStatus, Message};
use std::{
    ffi::{c_void, CString},
    future::Future,
    net::Ipv4Addr,
    ptr,
    sync::mpsc as sync_mpsc,
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinHandle,
    time::timeout,
};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        handshake::server::{Request, Response},
        http::{header, HeaderValue},
        Message as WsMessage,
    },
    WebSocketStream,
};

const GENERATION: u32 = 29;

type Host = WebSocketStream<TcpStream>;

async fn deadline<T>(future: impl Future<Output = T>) -> T {
    timeout(Duration::from_secs(5), future)
        .await
        .expect("test timed out")
}

async fn accept(listener: TcpListener) -> Host {
    let (stream, _) = deadline(listener.accept()).await.unwrap();
    #[allow(clippy::result_large_err)] // The handshake callback has a fixed error type.
    let respond = |_: &Request, mut res: Response| {
        res.headers_mut().insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("nsl1"),
        );
        Ok(res)
    };
    deadline(accept_hdr_async(stream, respond)).await.unwrap()
}

async fn send(host: &mut Host, message: Message) {
    host.send(WsMessage::Binary(message.encode().unwrap()))
        .await
        .unwrap();
}

async fn expect(host: &mut Host, expected: Message) {
    let received = deadline(host.next()).await.unwrap().unwrap();
    let WsMessage::Binary(bytes) = received else {
        panic!("expected a binary message: {received:?}")
    };
    assert_eq!(Message::decode(&bytes).unwrap(), expected);
}

fn video(sequence: u32) -> Message {
    Message::Frame {
        generation: GENERATION,
        sequence,
        keyframe: sequence == 1,
        width: 640,
        height: 360,
        capture_us: 0,
        data: Bytes::from_static(&[0, 0, 0, 1, 0x65]),
    }
}

fn query() -> Message {
    Message::Ack {
        generation: GENERATION,
        sequence: 0,
        viewer_wait_us: None,
    }
}

fn ack(sequence: u32) -> Message {
    Message::Ack {
        generation: GENERATION,
        sequence,
        viewer_wait_us: Some(5),
    }
}

fn probe_reply(status: InputStatus) -> Message {
    Message::InputResponse {
        generation: GENERATION,
        sequence: 0,
        status,
    }
}

fn audio_control(enabled: bool) -> Message {
    Message::AudioControl {
        generation: GENERATION,
        enabled,
    }
}

fn audio_config() -> Message {
    Message::AudioConfig {
        generation: GENERATION,
        format: AudioFormat::opus(),
    }
}

/// 20 ms packets of a 440 Hz stereo tone, encoded like the host does.
fn tone_frames(count: u32) -> Vec<Message> {
    let mut error = 0;
    let encoder =
        unsafe { opus::opus_encoder_create(48_000, 2, opus::OPUS_APPLICATION_AUDIO, &mut error) };
    assert!(!encoder.is_null() && error == opus::OPUS_OK);
    let mut phase = 0f32;
    let frames = (1..=count)
        .map(|sequence| {
            let pcm: Vec<i16> = (0..960)
                .flat_map(|_| {
                    phase += 440.0 * std::f32::consts::TAU / 48_000.0;
                    let sample = (phase.sin() * 12_000.0) as i16;
                    [sample, sample]
                })
                .collect();
            let mut packet = vec![0u8; 4000];
            let len =
                unsafe { opus::opus_encode(encoder, pcm.as_ptr(), 960, packet.as_mut_ptr(), 4000) };
            packet.truncate(usize::try_from(len).unwrap());
            Message::AudioFrame {
                generation: GENERATION,
                sequence,
                capture_us: u64::from(sequence) * 20_000,
                data: packet.into(),
            }
        })
        .collect();
    unsafe { opus::opus_encoder_destroy(encoder) };
    frames
}

fn loud(samples: &[i16]) -> bool {
    let energy = samples.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / samples.len() as f64;
    energy > 1e6
}

#[derive(Debug, PartialEq, Eq)]
enum Native {
    Frame(u32),
    Audio(AudioPcm),
}

unsafe extern "C" fn native_frame(
    data: *mut c_void,
    _: u32,
    sequence: u32,
    _: u16,
    _: u16,
    _: u64,
    _: bool,
    _: *const u8,
    _: usize,
) {
    let tx = unsafe { &*data.cast::<mpsc::UnboundedSender<Native>>() };
    let _ = tx.send(Native::Frame(sequence));
}

unsafe extern "C" fn native_audio(
    data: *mut c_void,
    capture_us: u64,
    sample_rate: u32,
    channels: u8,
    pcm: *const i16,
    samples_per_channel: usize,
) {
    let tx = unsafe { &*data.cast::<mpsc::UnboundedSender<Native>>() };
    let samples =
        unsafe { std::slice::from_raw_parts(pcm, samples_per_channel * usize::from(channels)) };
    let _ = tx.send(Native::Audio(AudioPcm {
        capture_us,
        sample_rate,
        channels,
        samples: samples.to_vec(),
    }));
}

enum Command {
    SetAudio(i32),
    Ack(u32),
}

/// A C-ABI client on a blocking thread. Each command's return code comes back on `codes`;
/// dropping the command sender destroys the client.
struct CClient {
    events: mpsc::UnboundedReceiver<Native>,
    commands: sync_mpsc::Sender<Command>,
    codes: mpsc::UnboundedReceiver<i32>,
    task: JoinHandle<()>,
}

impl CClient {
    fn connect(listener: &TcpListener, audio: bool) -> Self {
        let url = CString::new(format!("ws://{}", listener.local_addr().unwrap())).unwrap();
        let (tx, events) = mpsc::unbounded_channel();
        let (commands, run) = sync_mpsc::channel();
        let (reply, codes) = mpsc::unbounded_channel();
        let task = tokio::task::spawn_blocking(move || unsafe {
            let callbacks = SkylatchCallbacks {
                on_frame: Some(native_frame),
                on_audio_pcm: audio.then_some(native_audio as _),
                ..Default::default()
            };
            let client: *mut SkylatchClient = skylatch_connect_direct(
                url.as_ptr(),
                ptr::null(),
                &callbacks,
                ptr::from_ref(&tx).cast_mut().cast(),
                ptr::null_mut(),
                0,
            );
            assert!(!client.is_null());
            while let Ok(command) = run.recv() {
                let code = match command {
                    Command::SetAudio(enabled) => skylatch_set_audio_enabled(client, enabled),
                    Command::Ack(sequence) => skylatch_send_ack(client, GENERATION, sequence, 5),
                };
                reply.send(code).unwrap();
            }
            skylatch_destroy(client);
        });
        Self {
            events,
            commands,
            codes,
            task,
        }
    }

    async fn run(&mut self, command: Command) -> i32 {
        self.commands.send(command).unwrap();
        deadline(self.codes.recv()).await.unwrap()
    }

    async fn next(&mut self) -> Native {
        deadline(self.events.recv()).await.unwrap()
    }

    async fn close(self) {
        drop(self.commands);
        deadline(self.task).await.unwrap();
    }
}

#[tokio::test]
async fn older_hosts_never_receive_audio_control() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut client = CClient::connect(&listener, true);
    let mut host = accept(listener).await;
    send(&mut host, video(1)).await;
    assert_eq!(client.next().await, Native::Frame(1));
    expect(&mut host, query()).await;
    // A v0.1 host ignores the query; asking for audio again must still send nothing.
    send(&mut host, video(2)).await;
    assert_eq!(client.next().await, Native::Frame(2));
    assert_eq!(client.run(Command::SetAudio(1)).await, SKYLATCH_OK);
    assert_eq!(client.run(Command::Ack(1)).await, SKYLATCH_OK);
    expect(&mut host, ack(1)).await;
    // A host that declines the query has no audio either.
    send(&mut host, probe_reply(InputStatus::Unsupported)).await;
    send(&mut host, video(3)).await;
    assert_eq!(client.next().await, Native::Frame(3));
    assert_eq!(client.run(Command::Ack(2)).await, SKYLATCH_OK);
    expect(&mut host, ack(2)).await;
    client.close().await;
    assert!(matches!(
        deadline(host.next()).await,
        Some(Ok(WsMessage::Close(_)))
    ));
}

#[tokio::test]
async fn accepting_host_gets_audio_control_and_pcm_reaches_the_c_callback() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut client = CClient::connect(&listener, true);
    let mut host = accept(listener).await;
    send(&mut host, video(1)).await;
    assert_eq!(client.next().await, Native::Frame(1));
    expect(&mut host, query()).await;
    send(&mut host, probe_reply(InputStatus::Accepted)).await;
    expect(&mut host, audio_control(true)).await;

    let mut frames = tone_frames(7).into_iter();
    send(&mut host, audio_config()).await;
    for frame in frames.by_ref().take(5) {
        send(&mut host, frame).await;
    }
    for sequence in 1..=5u64 {
        let Native::Audio(pcm) = client.next().await else {
            panic!("expected audio")
        };
        assert_eq!(pcm.capture_us, sequence * 20_000);
        assert_eq!((pcm.sample_rate, pcm.channels), (48_000, 2));
        assert_eq!(pcm.samples_per_channel(), 960);
        if sequence == 5 {
            assert!(loud(&pcm.samples));
        }
    }

    assert_eq!(client.run(Command::SetAudio(0)).await, SKYLATCH_OK);
    expect(&mut host, audio_control(false)).await;
    // Packets already in flight when muting are not played.
    send(&mut host, frames.next().unwrap()).await;
    send(&mut host, video(2)).await;
    assert_eq!(client.next().await, Native::Frame(2));
    assert_eq!(client.run(Command::SetAudio(1)).await, SKYLATCH_OK);
    expect(&mut host, audio_control(true)).await;
    client.close().await;
}

#[tokio::test]
async fn without_an_audio_callback_audio_is_never_requested() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut client = CClient::connect(&listener, false);
    let mut host = accept(listener).await;
    send(&mut host, video(1)).await;
    assert_eq!(client.next().await, Native::Frame(1));
    expect(&mut host, query()).await;
    send(&mut host, probe_reply(InputStatus::Accepted)).await;
    assert_eq!(
        client.run(Command::SetAudio(1)).await,
        SKYLATCH_ERR_UNSUPPORTED
    );
    assert_eq!(client.run(Command::Ack(1)).await, SKYLATCH_OK);
    expect(&mut host, ack(1)).await;
    client.close().await;
}

#[tokio::test]
async fn rust_sessions_request_audio_only_when_enabled() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (client, mut host) = tokio::join!(
        deadline(SessionClient::connect_direct(&url, None, move |event| {
            let _ = tx.send(event);
        })),
        accept(listener)
    );
    let client = client.unwrap();
    send(&mut host, video(1)).await;
    assert!(matches!(deadline(rx.recv()).await, Some(Event::Frame(_))));
    expect(&mut host, query()).await;
    send(&mut host, probe_reply(InputStatus::Accepted)).await;
    assert!(matches!(
        deadline(rx.recv()).await,
        Some(Event::InputResponse { .. })
    ));
    client.send_ack(GENERATION, 1, 5).unwrap();
    expect(&mut host, ack(1)).await;

    client.set_audio_enabled(true);
    expect(&mut host, audio_control(true)).await;
    send(&mut host, audio_config()).await;
    for frame in tone_frames(3) {
        send(&mut host, frame).await;
    }
    let mut decoded = Vec::new();
    for _ in 0..3 {
        let Some(Event::Audio(pcm)) = deadline(rx.recv()).await else {
            panic!("expected audio")
        };
        decoded.extend(pcm.samples);
    }
    assert_eq!(decoded.len(), 3 * 960 * 2);
    assert!(loud(&decoded[960 * 2..]));
}
