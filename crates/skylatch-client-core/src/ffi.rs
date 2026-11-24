//! C ABI for native viewers. See `include/skylatch_client_core.h` for the public contract.

use crate::{session::Connection, ClientError, Event, SendError, SessionClient};
use skylatch_proto::nsl1::{self, Cursor, Header, Nsl1Error, INPUT_SIZE};
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    future::Future,
    panic::{catch_unwind, AssertUnwindSafe},
    ptr,
    sync::mpsc,
    thread::{self, JoinHandle},
};

/// Header fields as laid out for C callers.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SkylatchNsl1Header {
    pub msg_type: u8,
    pub flags: u16,
    pub generation: u32,
    pub sequence: u32,
    pub payload_len: u32,
}

pub const SKYLATCH_OK: c_int = 0;
pub const SKYLATCH_ERR_NULL: c_int = -1;
pub const SKYLATCH_ERR_TRUNCATED: c_int = -2;
pub const SKYLATCH_ERR_BAD_MAGIC: c_int = -3;
pub const SKYLATCH_ERR_BAD_VERSION: c_int = -4;
pub const SKYLATCH_ERR_UNKNOWN_TYPE: c_int = -5;
pub const SKYLATCH_ERR_BAD_FLAGS: c_int = -6;
pub const SKYLATCH_ERR_ZERO_GENERATION: c_int = -7;
pub const SKYLATCH_ERR_TOO_LARGE: c_int = -8;

pub const SKYLATCH_ERR_CLOSED: c_int = -9;
pub const SKYLATCH_ERR_QUEUE_FULL: c_int = -10;
pub const SKYLATCH_ERR_NOT_READY: c_int = -11;
pub const SKYLATCH_ERR_INVALID: c_int = -12;

static VERSION_C: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");

/// Returns the numeric version of the `NSL1` wire protocol.
#[no_mangle]
pub extern "C" fn skylatch_proto_version() -> u32 {
    u32::from(nsl1::VERSION)
}

/// Returns a static NUL-terminated library version string.
#[no_mangle]
pub extern "C" fn skylatch_version_string() -> *const c_char {
    VERSION_C.as_ptr().cast()
}

/// Parses the 20-byte `NSL1` header at the start of `buf`.
///
/// Returns [`SKYLATCH_OK`] and fills `out`, or a negative `SKYLATCH_ERR_*` code.
///
/// # Safety
/// `buf` must point to `len` readable bytes and `out` to a writable [`SkylatchNsl1Header`].
#[no_mangle]
pub unsafe extern "C" fn skylatch_parse_nsl1_header(
    buf: *const u8,
    len: usize,
    out: *mut SkylatchNsl1Header,
) -> c_int {
    if buf.is_null() || out.is_null() {
        return SKYLATCH_ERR_NULL;
    }
    // SAFETY: the caller guarantees `buf` points to `len` readable bytes.
    let slice = unsafe { std::slice::from_raw_parts(buf, len) };
    match Header::decode(slice) {
        Ok(h) => {
            // SAFETY: the caller guarantees `out` is writable.
            unsafe {
                *out = SkylatchNsl1Header {
                    msg_type: h.msg_type as u8,
                    flags: h.flags,
                    generation: h.generation,
                    sequence: h.sequence,
                    payload_len: h.payload_len,
                };
            }
            SKYLATCH_OK
        }
        Err(e) => error_code(&e),
    }
}

fn error_code(e: &Nsl1Error) -> c_int {
    match e {
        Nsl1Error::Truncated => SKYLATCH_ERR_TRUNCATED,
        Nsl1Error::BadMagic => SKYLATCH_ERR_BAD_MAGIC,
        Nsl1Error::BadVersion(_) => SKYLATCH_ERR_BAD_VERSION,
        Nsl1Error::UnknownType(_) => SKYLATCH_ERR_UNKNOWN_TYPE,
        Nsl1Error::BadFlags(_) => SKYLATCH_ERR_BAD_FLAGS,
        Nsl1Error::ZeroGeneration => SKYLATCH_ERR_ZERO_GENERATION,
        _ => SKYLATCH_ERR_TOO_LARGE,
    }
}

/// A C callback table. Byte slices and error strings are only borrowed for the call.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SkylatchCallbacks {
    pub on_frame:
        Option<unsafe extern "C" fn(*mut c_void, u32, u32, u16, u16, u64, bool, *const u8, usize)>,
    pub on_cursor: Option<unsafe extern "C" fn(*mut c_void, u16, u16, u16, u16, *const u8, usize)>,
    pub on_clipboard_notice: Option<unsafe extern "C" fn(*mut c_void)>,
    pub on_disconnected: Option<unsafe extern "C" fn(*mut c_void, *const c_char)>,
}

struct ForeignCallbacks {
    table: SkylatchCallbacks,
    user_data: *mut c_void,
}

// SAFETY: the connect caller guarantees callback code and user_data are valid for the worker
// thread, with any access from other threads synchronized by the caller.
unsafe impl Send for ForeignCallbacks {}

impl ForeignCallbacks {
    fn dispatch(&self, event: Event) {
        let user = self.user_data;
        // SAFETY: function pointers and user_data are valid until the worker has stopped. Each
        // payload is owned here and stays live throughout its respective callback.
        unsafe {
            match event {
                Event::Frame(frame) => {
                    if let Some(f) = self.table.on_frame {
                        f(
                            user,
                            frame.generation,
                            frame.sequence,
                            frame.width,
                            frame.height,
                            frame.capture_us,
                            frame.is_keyframe,
                            frame.annex_b.as_ptr(),
                            frame.annex_b.len(),
                        );
                    }
                }
                Event::Cursor(cursor) => {
                    if let Some(f) = self.table.on_cursor {
                        match cursor {
                            Cursor::Reset => f(user, 0, 0, 0, 0, ptr::null(), 0),
                            Cursor::Image {
                                width,
                                height,
                                hot_x,
                                hot_y,
                                rgba,
                            } => {
                                f(user, width, height, hot_x, hot_y, rgba.as_ptr(), rgba.len());
                            }
                        }
                    }
                }
                Event::ClipboardNotice => {
                    if let Some(f) = self.table.on_clipboard_notice {
                        f(user);
                    }
                }
                Event::Disconnected(result) => {
                    if let Some(f) = self.table.on_disconnected {
                        let error = result.err().map(|err| c_error(&err.to_string()));
                        f(user, error.as_ref().map_or(ptr::null(), |s| s.as_ptr()));
                    }
                }
            }
        }
    }
}

/// Opaque owner of a connected session and its worker thread.
pub struct SkylatchClient {
    session: SessionClient,
    worker: JoinHandle<()>,
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

fn start_worker(
    open: impl Future<Output = Result<Connection, ClientError>> + Send + 'static,
    callbacks: ForeignCallbacks,
) -> Result<SkylatchClient, String> {
    let (ready, status) = mpsc::sync_channel::<Result<SessionClient, String>>(0);
    let worker = thread::Builder::new()
        .name("skylatch-client".into())
        .spawn(move || {
            let rt = match runtime() {
                Ok(rt) => rt,
                Err(error) => {
                    let _ = ready.send(Err(error.to_string()));
                    return;
                }
            };
            rt.block_on(async move {
                match open.await {
                    Ok(connection) => {
                        let (client, pump) =
                            connection.start(move |event| callbacks.dispatch(event));
                        if ready.send(Ok(client)).is_ok() {
                            pump.await;
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error.to_string()));
                    }
                }
            });
            // Timed-out DNS lookups may still hold a Tokio blocking-pool thread.
            rt.shutdown_background();
        })
        .map_err(|e| e.to_string())?;
    let error = match status.recv() {
        Ok(Ok(session)) => return Ok(SkylatchClient { session, worker }),
        Ok(Err(error)) => error,
        Err(_) => "client worker stopped during handshake".into(),
    };
    let _ = worker.join();
    Err(error)
}

fn c_error(error: &str) -> CString {
    // Errors can contain remote-provided text. Exclude interior NULs.
    CString::new(error.replace('\0', "\\0")).unwrap_or_default()
}

/// # Safety
/// `value` must be null or a valid, NUL-terminated string.
unsafe fn copy_arg(value: *const c_char, name: &str) -> Result<String, String> {
    if value.is_null() {
        return Err(format!("{name} is null"));
    }
    // SAFETY: guaranteed by caller.
    let value = unsafe { CStr::from_ptr(value) };
    value
        .to_str()
        .map(str::to_owned)
        .map_err(|_| format!("{name} is not UTF-8"))
}

/// # Safety
/// `err_buf` is null or points to `err_buf_len` writable bytes (non-overlapping with inputs).
unsafe fn ffi_result<T>(
    err_buf: *mut c_char,
    err_buf_len: usize,
    action: impl FnOnce() -> Result<*mut T, String>,
) -> *mut T {
    let result = catch_unwind(AssertUnwindSafe(action))
        .unwrap_or_else(|_| Err("unexpected client error".into()));
    if !err_buf.is_null() && err_buf_len > 0 {
        let error = result
            .as_ref()
            .err()
            .map_or_else(String::new, |e| e.replace('\0', "\\0"));
        let mut bytes = error.len().min(err_buf_len - 1);
        while !error.is_char_boundary(bytes) {
            bytes -= 1;
        }
        // SAFETY: caller provides a buffer large enough, and `error` is a separate allocation.
        unsafe {
            ptr::copy_nonoverlapping(error.as_ptr(), err_buf.cast(), bytes);
            *err_buf.add(bytes) = 0;
        }
    }
    result.unwrap_or(ptr::null_mut())
}

/// List devices as a UTF-8 JSON array. Free a successful result with [`skylatch_free_string`].
///
/// # Safety
/// `hub_url` and `token` must be valid NUL-terminated strings or null. Non-null `err_buf`
/// must point to `err_buf_len` writable bytes and must not overlap any input.
#[no_mangle]
pub unsafe extern "C" fn skylatch_list_devices_json(
    hub_url: *const c_char,
    token: *const c_char,
    err_buf: *mut c_char,
    err_buf_len: usize,
) -> *mut c_char {
    unsafe {
        ffi_result(err_buf, err_buf_len, || {
            let url = copy_arg(hub_url, "hub URL")?;
            let token = copy_arg(token, "hub token")?;
            let json = thread::Builder::new()
                .name("skylatch-discovery".into())
                .spawn(move || {
                    let rt = runtime().map_err(|e| e.to_string())?;
                    let devices = rt.block_on(crate::list_devices(&url, &token));
                    rt.shutdown_background();
                    let devices = devices.map_err(|e| e.to_string())?;
                    serde_json::to_string(&devices).map_err(|e| e.to_string())
                })
                .map_err(|e| e.to_string())?
                .join()
                .map_err(|_| "device discovery worker stopped".to_string())??;
            Ok(c_error(&json).into_raw())
        })
    }
}

/// Free a string returned by [`skylatch_list_devices_json`].
///
/// # Safety
/// `s` is null or is a result from this library that has not already been freed or modified.
#[no_mangle]
pub unsafe extern "C" fn skylatch_free_string(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: the caller transfers the original CString allocation back exactly once.
        drop(unsafe { CString::from_raw(s) });
    }
}

/// Connect to a registered host through the hub. Returns null on failure.
///
/// # Safety
/// String arguments must be NUL-terminated or null. Non-null `err_buf` must be writable
/// for `err_buf_len` bytes and must not overlap inputs. `callbacks` is null or a readable
/// callback table; callback functions and `user_data` must remain valid for the worker
/// until [`skylatch_destroy`] returns. See the header for threading and reentrancy rules.
#[no_mangle]
pub unsafe extern "C" fn skylatch_connect_hub(
    hub_url: *const c_char,
    token: *const c_char,
    device_id: *const c_char,
    prefer_direct: bool,
    callbacks: *const SkylatchCallbacks,
    user_data: *mut c_void,
    err_buf: *mut c_char,
    err_buf_len: usize,
) -> *mut SkylatchClient {
    unsafe {
        ffi_result(err_buf, err_buf_len, || {
            let url = copy_arg(hub_url, "hub URL")?;
            let token = copy_arg(token, "hub token")?;
            let device = copy_arg(device_id, "device ID")?;
            let callbacks = ForeignCallbacks {
                table: callbacks.as_ref().copied().unwrap_or_default(),
                user_data,
            };
            let client = start_worker(
                async move { Connection::hub(&url, &token, &device, prefer_direct).await },
                callbacks,
            )?;
            Ok(Box::into_raw(Box::new(client)))
        })
    }
}

/// Connect to a standalone `NSL1` listener. Returns null on failure.
///
/// # Safety
/// Same pointer, callback and lifetime rules as [`skylatch_connect_hub`].
#[no_mangle]
pub unsafe extern "C" fn skylatch_connect_direct(
    ws_url: *const c_char,
    token_or_null: *const c_char,
    callbacks: *const SkylatchCallbacks,
    user_data: *mut c_void,
    err_buf: *mut c_char,
    err_buf_len: usize,
) -> *mut SkylatchClient {
    unsafe {
        ffi_result(err_buf, err_buf_len, || {
            let url = copy_arg(ws_url, "stream URL")?;
            let token = (!token_or_null.is_null())
                .then(|| copy_arg(token_or_null, "direct token"))
                .transpose()?;
            let callbacks = ForeignCallbacks {
                table: callbacks.as_ref().copied().unwrap_or_default(),
                user_data,
            };
            let client = start_worker(
                async move { Connection::direct(&url, token.as_deref()).await },
                callbacks,
            )?;
            Ok(Box::into_raw(Box::new(client)))
        })
    }
}

fn send_code(result: Result<(), SendError>) -> i32 {
    match result {
        Ok(()) => SKYLATCH_OK,
        Err(SendError::Closed) => SKYLATCH_ERR_CLOSED,
        Err(SendError::QueueFull) => SKYLATCH_ERR_QUEUE_FULL,
        Err(SendError::NotReady) => SKYLATCH_ERR_NOT_READY,
        Err(SendError::InvalidAck | SendError::InvalidInput(_)) => SKYLATCH_ERR_INVALID,
    }
}

/// Queue an ACK without blocking on the network.
///
/// # Safety
/// `client` must be null or a live handle not concurrently being destroyed.
#[no_mangle]
pub unsafe extern "C" fn skylatch_send_ack(
    client: *mut SkylatchClient,
    generation: u32,
    sequence: u32,
    viewer_wait_us: u32,
) -> i32 {
    let Some(client) = (unsafe { client.as_ref() }) else {
        return SKYLATCH_ERR_NULL;
    };
    send_code(
        client
            .session
            .send_ack(generation, sequence, viewer_wait_us),
    )
}

/// Queue one input record without blocking on the network.
///
/// # Safety
/// `client` must be null or a live handle not concurrently being destroyed, and a non-null
/// `record` must point to 16 readable bytes.
#[no_mangle]
pub unsafe extern "C" fn skylatch_send_input(
    client: *mut SkylatchClient,
    record: *const u8,
) -> i32 {
    let Some(client) = (unsafe { client.as_ref() }) else {
        return SKYLATCH_ERR_NULL;
    };
    if record.is_null() {
        return SKYLATCH_ERR_NULL;
    }
    // SAFETY: caller provides a readable input record. The array has byte alignment.
    send_code(
        client
            .session
            .send_input(unsafe { record.cast::<[u8; INPUT_SIZE]>().read() }),
    )
}

/// Disconnect, wait for the worker to stop, and free the handle.
///
/// # Safety
/// `client` must be null or a live handle returned by a connect function. Call exactly once,
/// with no other calls using the same handle in progress, and not from one of its callbacks.
#[no_mangle]
pub unsafe extern "C" fn skylatch_destroy(client: *mut SkylatchClient) {
    if client.is_null() {
        return;
    }
    // Do not self-join if called from a callback in violation of the API contract.
    let worker_id = unsafe { &*client }.worker.thread().id();
    if worker_id == thread::current().id() {
        return;
    }
    // SAFETY: the caller transfers the handle back exactly once.
    let SkylatchClient { session, worker } = *unsafe { Box::from_raw(client) };
    drop(session);
    let _ = worker.join();
}

#[cfg(test)]
mod tests {
    use super::*;
    use skylatch_proto::nsl1::Message;
    use std::ffi::CStr;

    #[test]
    fn version_string() {
        assert_eq!(skylatch_proto_version(), 1);
        let s = unsafe { CStr::from_ptr(skylatch_version_string()) };
        assert_eq!(s.to_str().unwrap(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn parses_header() {
        let wire = Message::ClipboardNotice { generation: 9 }.encode().unwrap();
        let mut out = SkylatchNsl1Header::default();
        let rc = unsafe { skylatch_parse_nsl1_header(wire.as_ptr(), wire.len(), &mut out) };
        assert_eq!(rc, SKYLATCH_OK);
        assert_eq!(out.msg_type, 7);
        assert_eq!(out.generation, 9);
        assert_eq!(out.payload_len, 0);
    }

    #[test]
    fn reports_errors() {
        let mut out = SkylatchNsl1Header::default();
        let rc = unsafe { skylatch_parse_nsl1_header(std::ptr::null(), 0, &mut out) };
        assert_eq!(rc, SKYLATCH_ERR_NULL);
        let short = [0u8; 4];
        let rc = unsafe { skylatch_parse_nsl1_header(short.as_ptr(), short.len(), &mut out) };
        assert_eq!(rc, SKYLATCH_ERR_TRUNCATED);
        let junk = [0u8; 20];
        let rc = unsafe { skylatch_parse_nsl1_header(junk.as_ptr(), junk.len(), &mut out) };
        assert_eq!(rc, SKYLATCH_ERR_BAD_MAGIC);
        let mut wire = Message::ClipboardNotice { generation: 1 }
            .encode()
            .unwrap()
            .to_vec();
        wire[5] = 99;
        let rc = unsafe { skylatch_parse_nsl1_header(wire.as_ptr(), wire.len(), &mut out) };
        assert_eq!(rc, SKYLATCH_ERR_UNKNOWN_TYPE);
    }
}
