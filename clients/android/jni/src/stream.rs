// SPDX-License-Identifier: Apache-2.0

//! Streaming sessions. Each session owns one worker thread, attached to the JVM once for its
//! lifetime, that runs the session's network pump and delivers every `StreamCallback` call.

use crate::{
    clear_exception, guard, input_record, read_optional, read_string, send_status, Registry,
    ERR_HANDLE, ERR_INVALID, OK,
};
use jni::{
    objects::{GlobalRef, JByteArray, JMethodID, JObject, JString, JValue},
    signature::{Primitive, ReturnType},
    sys::{jboolean, jint, jlong, JNI_FALSE},
    JNIEnv, JavaVM,
};
use skylatch_client_core::{ClientError, Event, SessionClient, TransportMode};
use skylatch_proto::nsl1::{Cursor, INPUT_SIZE};
use std::{
    future::Future,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, PoisonError,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::sync::Notify;

static SESSIONS: Registry<Session> = Registry::new();

type Deliver = Box<dyn FnMut(Event) + Send>;

/// How long destroy waits for the WebSocket close handshake before dropping the pump.
const CLOSE_GRACE: Duration = Duration::from_secs(3);

struct Session {
    client: SessionClient,
    closed: Arc<AtomicBool>,
    shutdown: Arc<Notify>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone, Copy)]
struct Methods {
    video_frame: JMethodID,
    cursor: JMethodID,
    audio_pcm: JMethodID,
    monitor_list: JMethodID,
    clipboard_data: JMethodID,
    input_response: JMethodID,
    disconnected: JMethodID,
}

impl Methods {
    fn resolve(env: &mut JNIEnv, callback: &JObject) -> jni::errors::Result<Self> {
        let class = env.get_object_class(callback)?;
        let mut id = |name: &str, sig: &str| env.get_method_id(&class, name, sig);
        Ok(Self {
            video_frame: id("onVideoFrame", "(IIIIJZ[B)V")?,
            cursor: id("onCursor", "(IIII[B)V")?,
            audio_pcm: id("onAudioPcm", "(JII[S)V")?,
            monitor_list: id("onMonitorList", "(Ljava/lang/String;)V")?,
            clipboard_data: id("onClipboardData", "(Ljava/lang/String;)V")?,
            input_response: id("onInputResponse", "(II)V")?,
            disconnected: id("onDisconnected", "(Ljava/lang/String;)V")?,
        })
    }
}

/// Bit-preserving `u32` to Java `int`.
fn int(value: u32) -> JValue<'static, 'static> {
    JValue::Int(value as i32)
}

/// # Safety
/// `method` must belong to `target`'s class and take exactly `args` and return `void`.
unsafe fn call_void<const N: usize>(
    env: &mut JNIEnv,
    target: &JObject,
    method: JMethodID,
    args: [JValue; N],
) -> jni::errors::Result<()> {
    let args = args.map(|arg| arg.as_jni());
    // SAFETY: guaranteed by the caller.
    unsafe {
        env.call_method_unchecked(
            target,
            method,
            ReturnType::Primitive(Primitive::Void),
            &args,
        )
    }
    .map(drop)
}

struct Sink {
    vm: JavaVM,
    callback: GlobalRef,
    methods: Methods,
    closed: Arc<AtomicBool>,
    done: Arc<Notify>,
}

impl Sink {
    fn deliver(&self, event: Event) {
        let last = matches!(event, Event::Disconnected(_));
        if !self.closed.load(Ordering::Acquire) {
            let _ = catch_unwind(AssertUnwindSafe(|| self.dispatch(event)));
        }
        if last {
            self.done.notify_one();
        }
    }

    fn dispatch(&self, event: Event) {
        let Ok(mut env) = self.vm.get_env() else {
            return;
        };
        // The worker never returns to Java, so every local reference must be freed here.
        let _ = env.with_local_frame(4, |env| self.call(env, event));
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
    }

    fn call(&self, env: &mut JNIEnv, event: Event) -> jni::errors::Result<()> {
        let target = self.callback.as_obj();
        let m = &self.methods;
        // SAFETY: each method ID was resolved on the callback's class with the signature that
        // matches the arguments passed here.
        unsafe {
            match event {
                Event::Frame(frame) => {
                    let data = env.byte_array_from_slice(&frame.annex_b)?;
                    call_void(
                        env,
                        target,
                        m.video_frame,
                        [
                            int(frame.generation),
                            int(frame.sequence),
                            JValue::Int(frame.width.into()),
                            JValue::Int(frame.height.into()),
                            JValue::Long(frame.capture_us as i64),
                            JValue::Bool(frame.is_keyframe.into()),
                            JValue::Object(&data),
                        ],
                    )
                }
                Event::Cursor(Cursor::Reset) => {
                    let none = JObject::null();
                    let zero = JValue::Int(0);
                    call_void(
                        env,
                        target,
                        m.cursor,
                        [zero, zero, zero, zero, JValue::Object(&none)],
                    )
                }
                Event::Cursor(Cursor::Image {
                    width,
                    height,
                    hot_x,
                    hot_y,
                    rgba,
                }) => {
                    let data = env.byte_array_from_slice(&rgba)?;
                    call_void(
                        env,
                        target,
                        m.cursor,
                        [
                            JValue::Int(width.into()),
                            JValue::Int(height.into()),
                            JValue::Int(hot_x.into()),
                            JValue::Int(hot_y.into()),
                            JValue::Object(&data),
                        ],
                    )
                }
                Event::Audio(pcm) => {
                    let Ok(len) = i32::try_from(pcm.samples.len()) else {
                        return Ok(());
                    };
                    let samples = env.new_short_array(len)?;
                    env.set_short_array_region(&samples, 0, &pcm.samples)?;
                    call_void(
                        env,
                        target,
                        m.audio_pcm,
                        [
                            JValue::Long(pcm.capture_us as i64),
                            int(pcm.sample_rate),
                            JValue::Int(pcm.channels.into()),
                            JValue::Object(&samples),
                        ],
                    )
                }
                Event::MonitorList(monitors) => {
                    let json = serde_json::to_string(&monitors).unwrap_or_else(|_| "[]".into());
                    let json = env.new_string(json)?;
                    call_void(env, target, m.monitor_list, [JValue::Object(&json)])
                }
                Event::ClipboardData(text) => {
                    let text = env.new_string(text)?;
                    call_void(env, target, m.clipboard_data, [JValue::Object(&text)])
                }
                Event::InputResponse { sequence, status } => call_void(
                    env,
                    target,
                    m.input_response,
                    [int(sequence), JValue::Int(status as i32)],
                ),
                Event::Disconnected(result) => {
                    let error = match result {
                        Ok(()) => JObject::null(),
                        Err(error) => env.new_string(error.to_string())?.into(),
                    };
                    call_void(env, target, m.disconnected, [JValue::Object(&error)])
                }
                Event::ClipboardNotice => Ok(()),
            }
        }
    }
}

/// Connects on a fresh worker thread and blocks until the handshake finishes. Returns the
/// session handle, or `0` after `onDisconnected(error)` was delivered on the calling thread.
/// A callback that lacks a `StreamCallback` method leaves `NoSuchMethodError` pending.
fn start<F, Fut>(
    env: &mut JNIEnv,
    callback: &JObject,
    audio: bool,
    open: Result<F, String>,
) -> jlong
where
    F: FnOnce(Deliver) -> Fut + Send + 'static,
    Fut: Future<Output = Result<SessionClient, ClientError>>,
{
    if callback.is_null() {
        let _ = env.throw_new("java/lang/NullPointerException", "callback is null");
        return 0;
    }
    let Ok(methods) = Methods::resolve(env, callback) else {
        return 0;
    };
    let started = open.and_then(|open| {
        let vm = env.get_java_vm().map_err(|e| e.to_string())?;
        let callback = env.new_global_ref(callback).map_err(|e| e.to_string())?;
        spawn(vm, callback, methods, audio, open)
    });
    match started {
        Ok(handle) => handle,
        Err(error) => {
            clear_exception(env);
            if let Ok(error) = env.new_string(error) {
                // SAFETY: `disconnected` was resolved on the callback's class as `(String)V`.
                let _ = unsafe {
                    call_void(
                        env,
                        callback,
                        methods.disconnected,
                        [JValue::Object(&error)],
                    )
                };
            }
            0
        }
    }
}

fn spawn<F, Fut>(
    vm: JavaVM,
    callback: GlobalRef,
    methods: Methods,
    audio: bool,
    open: F,
) -> Result<jlong, String>
where
    F: FnOnce(Deliver) -> Fut + Send + 'static,
    Fut: Future<Output = Result<SessionClient, ClientError>>,
{
    let closed = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(Notify::new());
    let done = Arc::new(Notify::new());
    let (ready, status) = mpsc::sync_channel::<Result<SessionClient, String>>(1);
    let sink = Sink {
        vm,
        callback,
        methods,
        closed: closed.clone(),
        done: done.clone(),
    };
    let stop = shutdown.clone();
    let worker = thread::Builder::new()
        .name("skylatch-session".into())
        .spawn(move || {
            if let Err(error) = sink.vm.attach_current_thread_permanently() {
                let _ = ready.send(Err(error.to_string()));
                return;
            }
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(error) => {
                    let _ = ready.send(Err(error.to_string()));
                    return;
                }
            };
            rt.block_on(async move {
                match open(Box::new(move |event| sink.deliver(event))).await {
                    Ok(client) => {
                        client.set_audio_enabled(audio);
                        if ready.send(Ok(client)).is_ok() {
                            tokio::select! {
                                _ = done.notified() => {}
                                _ = stop.notified() => {
                                    let _ = tokio::time::timeout(CLOSE_GRACE, done.notified()).await;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error.to_string()));
                    }
                }
            });
            // Dropping the runtime drops the pump and its callback; none can run afterwards.
            rt.shutdown_background();
        })
        .map_err(|e| e.to_string())?;
    let error = match status.recv() {
        Ok(Ok(client)) => {
            return Ok(SESSIONS.insert(Session {
                client,
                closed,
                shutdown,
                worker: Mutex::new(Some(worker)),
            }))
        }
        Ok(Err(error)) => error,
        Err(_) => "session worker stopped during connect".into(),
    };
    let _ = worker.join();
    Err(error)
}

pub(crate) fn with_session(handle: jlong, action: impl FnOnce(&SessionClient) -> jint) -> jint {
    guard(ERR_INVALID, || {
        SESSIONS
            .get(handle)
            .map_or(ERR_HANDLE, |session| action(&session.client))
    })
}

pub(crate) fn transport_code(handle: jlong) -> jint {
    guard(0, || match SESSIONS.get(handle) {
        None => 0,
        Some(session) => match session.client.transport_mode() {
            TransportMode::Direct => 1,
            TransportMode::Relay => 2,
        },
    })
}

/// Stops callbacks, closes the session and joins its worker. Unknown handles are ignored.
/// From inside a callback the worker cannot be joined; it exits once the callback returns.
pub(crate) fn destroy(handle: jlong) {
    guard((), || {
        let Some(session) = SESSIONS.remove(handle) else {
            return;
        };
        session.closed.store(true, Ordering::Release);
        session.client.disconnect();
        session.shutdown.notify_one();
        let worker = session
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(worker) = worker {
            if worker.thread().id() != thread::current().id() {
                let _ = worker.join();
            }
        }
    })
}

fn flag(value: jboolean) -> bool {
    value != JNI_FALSE
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeConnectHub(
    mut env: JNIEnv,
    _this: JObject,
    hub_url: JString,
    token: JString,
    device_id: JString,
    prefer_direct: jboolean,
    audio_enabled: jboolean,
    callback: JObject,
) -> jlong {
    guard(0, || {
        let args = (|| {
            Ok::<_, String>((
                read_string(&mut env, &hub_url, "hubUrl")?,
                read_string(&mut env, &token, "token")?,
                read_string(&mut env, &device_id, "deviceId")?,
            ))
        })();
        let open = args.map(|(hub_url, token, device_id)| {
            move |callback: Deliver| async move {
                SessionClient::connect_hub(
                    &hub_url,
                    &token,
                    &device_id,
                    flag(prefer_direct),
                    callback,
                )
                .await
            }
        });
        start(&mut env, &callback, flag(audio_enabled), open)
    })
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeConnectDirect(
    mut env: JNIEnv,
    _this: JObject,
    ws_url: JString,
    token: JString,
    audio_enabled: jboolean,
    callback: JObject,
) -> jlong {
    guard(0, || {
        let args = (|| {
            Ok::<_, String>((
                read_string(&mut env, &ws_url, "wsUrl")?,
                read_optional(&mut env, &token, "token")?,
            ))
        })();
        let open = args.map(|(ws_url, token)| {
            move |callback: Deliver| async move {
                SessionClient::connect_direct(&ws_url, token.as_deref(), callback).await
            }
        });
        start(&mut env, &callback, flag(audio_enabled), open)
    })
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeTransportMode(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jint {
    transport_code(handle)
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeSendAck(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    generation: jint,
    sequence: jint,
    viewer_wait_us: jint,
) -> jint {
    with_session(handle, |client| {
        send_status(client.send_ack(
            generation as u32,
            sequence as u32,
            viewer_wait_us.max(0) as u32,
        ))
    })
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeSendInput(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
    record: JByteArray,
) -> jint {
    guard(ERR_INVALID, || {
        if record.is_null() || env.get_array_length(&record).ok() != Some(INPUT_SIZE as i32) {
            return ERR_INVALID;
        }
        let mut raw = [0i8; INPUT_SIZE];
        if env.get_byte_array_region(&record, 0, &mut raw).is_err() {
            return ERR_INVALID;
        }
        match input_record(&raw.map(|b| b as u8)) {
            Ok(record) => with_session(handle, |client| send_status(client.send_input(record))),
            Err(code) => code,
        }
    })
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeSendClipboard(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    text: JString,
) -> jint {
    guard(ERR_INVALID, || match read_string(&mut env, &text, "text") {
        Ok(text) => with_session(handle, |client| send_status(client.send_clipboard(&text))),
        Err(_) => ERR_INVALID,
    })
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeSelectMonitor(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    monitor_id: JString,
) -> jint {
    guard(ERR_INVALID, || {
        match read_string(&mut env, &monitor_id, "monitorId") {
            Ok(id) => with_session(handle, |client| send_status(client.select_monitor(&id))),
            Err(_) => ERR_INVALID,
        }
    })
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeSetAudioEnabled(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    enabled: jboolean,
) -> jint {
    with_session(handle, |client| {
        client.set_audio_enabled(flag(enabled));
        OK
    })
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeDestroy(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) {
    destroy(handle);
}
