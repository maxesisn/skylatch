// SPDX-License-Identifier: Apache-2.0

//! JNI bridge behind `io.github.maxesisn.skylatch.NativeBridge`, built as
//! `libskylatch_android.so`.
//!
//! Every export catches panics and reports failure as a JSON envelope
//! (`{"ok":false,"error":"..."}`), a negative status code or a `0` handle; nothing unwinds into
//! the JVM. Status codes are the `SKYLATCH_*` values of `skylatch_client_core::ffi`.

mod hub;
mod stream;

use jni::{objects::JString, sys::jstring, JNIEnv};
use serde_json::{json, Map, Value};
use skylatch_client_core::{
    ffi,
    updater::{self, ReleaseNotes, UpdateRelease},
    SendError,
};
use skylatch_proto::nsl1::{InputRecord, INPUT_SIZE};
use std::{
    collections::BTreeMap,
    fmt::Display,
    future::Future,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
    },
};

pub const OK: i32 = ffi::SKYLATCH_OK;
pub const ERR_HANDLE: i32 = ffi::SKYLATCH_ERR_NULL;
pub const ERR_CLOSED: i32 = ffi::SKYLATCH_ERR_CLOSED;
pub const ERR_QUEUE_FULL: i32 = ffi::SKYLATCH_ERR_QUEUE_FULL;
pub const ERR_NOT_READY: i32 = ffi::SKYLATCH_ERR_NOT_READY;
pub const ERR_INVALID: i32 = ffi::SKYLATCH_ERR_INVALID;
pub const ERR_UNSUPPORTED: i32 = ffi::SKYLATCH_ERR_UNSUPPORTED;

/// `{"ok":true, ...fields}` for an object (other values add no fields), or
/// `{"ok":false,"error":...}`.
pub fn envelope<E: Display>(result: Result<Value, E>) -> String {
    let mut body = Map::new();
    match result {
        Ok(value) => {
            if let Value::Object(fields) = value {
                body.extend(fields);
            }
            body.insert("ok".into(), Value::Bool(true));
        }
        Err(error) => {
            body.insert("ok".into(), Value::Bool(false));
            body.insert("error".into(), Value::String(error.to_string()));
        }
    }
    Value::Object(body).to_string()
}

/// Validates one 16-byte `NSL1` input record before it reaches a session.
pub fn input_record(bytes: &[u8]) -> Result<[u8; INPUT_SIZE], i32> {
    let record: [u8; INPUT_SIZE] = bytes.try_into().map_err(|_| ERR_INVALID)?;
    InputRecord::decode(&record).map_err(|_| ERR_INVALID)?;
    Ok(record)
}

pub fn send_status(result: Result<(), SendError>) -> i32 {
    match result {
        Ok(()) => OK,
        Err(SendError::Closed) => ERR_CLOSED,
        Err(SendError::QueueFull) => ERR_QUEUE_FULL,
        Err(SendError::NotReady) => ERR_NOT_READY,
        Err(SendError::Unsupported) => ERR_UNSUPPORTED,
        Err(SendError::InvalidAck | SendError::InvalidInput(_) | SendError::InvalidSl1(_)) => {
            ERR_INVALID
        }
    }
}

/// The update-check payload for the Android APK asset.
pub fn update_json(found: Option<(UpdateRelease, ReleaseNotes)>) -> Value {
    match found {
        None => json!({ "available": false, "version": updater::current_version() }),
        Some((release, notes)) => json!({
            "available": true,
            "version": release.version,
            "asset_name": release.package.name,
            "size": release.package.size,
            "download_url": release.package.url,
            "checksums_url": release.checksums.url,
            "release_url": notes.url,
            "notes": notes.body,
        }),
    }
}

/// Opaque, never-zero handles. A removed or unknown handle resolves to nothing, so a stale or
/// repeated handle from Java can never reach freed memory.
pub struct Registry<T> {
    next: AtomicI64,
    entries: Mutex<BTreeMap<i64, Arc<T>>>,
}

impl<T> Registry<T> {
    pub const fn new() -> Self {
        Self {
            next: AtomicI64::new(1),
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    fn entries(&self) -> MutexGuard<'_, BTreeMap<i64, Arc<T>>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn insert(&self, value: T) -> i64 {
        let handle = self.next.fetch_add(1, Ordering::Relaxed);
        self.entries().insert(handle, Arc::new(value));
        handle
    }

    pub fn get(&self, handle: i64) -> Option<Arc<T>> {
        self.entries().get(&handle).cloned()
    }

    pub fn remove(&self, handle: i64) -> Option<Arc<T>> {
        self.entries().remove(&handle)
    }
}

impl<T> Default for Registry<T> {
    fn default() -> Self {
        Self::new()
    }
}

fn runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("skylatch-jni")
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Runs a core request to completion on the calling (Java) thread.
fn wait<T, E: Display>(future: impl Future<Output = Result<T, E>>) -> Result<T, String> {
    runtime()?.block_on(future).map_err(|e| e.to_string())
}

fn guard<T>(fallback: T, body: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or(fallback)
}

fn clear_exception(env: &mut JNIEnv) {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
}

fn read_string(env: &mut JNIEnv, value: &JString, name: &str) -> Result<String, String> {
    if value.is_null() {
        return Err(format!("{name} is null"));
    }
    match env.get_string(value) {
        Ok(text) => Ok(text.into()),
        Err(_) => {
            clear_exception(env);
            Err(format!("{name} is not a readable string"))
        }
    }
}

fn read_optional(env: &mut JNIEnv, value: &JString, name: &str) -> Result<Option<String>, String> {
    if value.is_null() {
        return Ok(None);
    }
    read_string(env, value, name).map(|s| Some(s).filter(|s| !s.is_empty()))
}

fn new_jstring(env: &mut JNIEnv, text: &str) -> jstring {
    clear_exception(env);
    env.new_string(text)
        .map_or(std::ptr::null_mut(), JString::into_raw)
}

/// Runs `body` and returns its JSON envelope as a Java string.
fn json_reply(
    env: &mut JNIEnv,
    body: impl FnOnce(&mut JNIEnv) -> Result<Value, String>,
) -> jstring {
    let text = catch_unwind(AssertUnwindSafe(|| envelope(body(env))))
        .unwrap_or_else(|_| envelope::<&str>(Err("internal error in the native bridge")));
    new_jstring(env, &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use skylatch_client_core::updater::{select_assets, Asset, InstallKind};
    use skylatch_proto::nsl1::{InputEvent, WheelAxis};

    fn parse(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn envelope_wraps_fields_and_errors() {
        assert_eq!(
            parse(&envelope::<String>(Ok(json!({ "token": "t" })))),
            json!({ "ok": true, "token": "t" })
        );
        assert_eq!(
            parse(&envelope::<String>(Ok(Value::Null))),
            json!({ "ok": true })
        );
        assert_eq!(
            parse(&envelope::<String>(Ok(json!({ "ok": false })))),
            json!({ "ok": true })
        );
        assert_eq!(
            parse(&envelope(Err::<Value, _>("hub said \"no\"\n"))),
            json!({ "ok": false, "error": "hub said \"no\"\n" })
        );
    }

    fn record(event: InputEvent) -> [u8; INPUT_SIZE] {
        InputRecord::new(event).encode().unwrap()
    }

    #[test]
    fn input_records_are_validated() {
        let records = [
            record(InputEvent::PointerMove { x: 10, y: 20 }),
            record(InputEvent::KeyDown { usage: 4 }),
            record(InputEvent::Text { scalar: '漢' }),
            record(InputEvent::Wheel {
                axis: WheelAxis::Vertical,
                delta: -240,
                x: 1,
                y: 2,
            }),
            record(InputEvent::SmoothWheel {
                axis: WheelAxis::Horizontal,
                delta: 37,
                x: 1,
                y: 2,
            }),
        ];
        for bytes in records {
            assert_eq!(input_record(&bytes), Ok(bytes));
        }
        let text = record(InputEvent::Text { scalar: 'a' });
        assert_eq!(text[0], 8);

        assert_eq!(input_record(&[]), Err(ERR_INVALID));
        assert_eq!(input_record(&text[..15]), Err(ERR_INVALID));
        assert_eq!(
            input_record(&[text.as_slice(), &[0]].concat()),
            Err(ERR_INVALID)
        );
        let mut reserved = text;
        reserved[1] = 1;
        assert_eq!(input_record(&reserved), Err(ERR_INVALID));
        let mut unknown = text;
        unknown[0] = 42;
        assert_eq!(input_record(&unknown), Err(ERR_INVALID));
        let mut bad_wheel = record(InputEvent::Wheel {
            axis: WheelAxis::Vertical,
            delta: 120,
            x: 0,
            y: 0,
        });
        bad_wheel[7] = 121;
        assert_eq!(input_record(&bad_wheel), Err(ERR_INVALID));
    }

    #[test]
    fn send_errors_map_to_ffi_codes() {
        assert_eq!(send_status(Ok(())), 0);
        assert_eq!(send_status(Err(SendError::Closed)), ERR_CLOSED);
        assert_eq!(send_status(Err(SendError::QueueFull)), ERR_QUEUE_FULL);
        assert_eq!(send_status(Err(SendError::NotReady)), ERR_NOT_READY);
        assert_eq!(send_status(Err(SendError::Unsupported)), ERR_UNSUPPORTED);
        assert_eq!(send_status(Err(SendError::InvalidAck)), ERR_INVALID);
    }

    #[test]
    fn registry_handles_are_opaque_and_single_use() {
        let registry = Registry::new();
        assert!(registry.get(0).is_none());
        let a = registry.insert("a");
        let b = registry.insert("b");
        assert!(a != 0 && b != 0 && a != b);
        assert_eq!(registry.get(a).as_deref(), Some(&"a"));
        assert_eq!(registry.remove(a).as_deref(), Some(&"a"));
        assert!(registry.remove(a).is_none());
        assert!(registry.get(a).is_none());
        assert!(registry.get(-1).is_none());
        assert_eq!(registry.get(b).as_deref(), Some(&"b"));
    }

    #[test]
    fn unknown_session_handles_are_harmless() {
        for handle in [0, -1, i64::MAX] {
            assert_eq!(stream::transport_code(handle), 0);
            assert_eq!(stream::with_session(handle, |_| OK), ERR_HANDLE);
            stream::destroy(handle);
            stream::destroy(handle);
        }
    }

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.into(),
            url: format!("https://example.invalid/{name}"),
            size: Some(7),
        }
    }

    #[test]
    fn android_update_selects_the_apk() {
        let assets = [
            asset("Skylatch-0.3.0-macos-arm64.dmg"),
            asset("Skylatch-0.3.0-android-arm64.apk"),
            asset("SHA256SUMS"),
        ];
        assert_eq!(
            InstallKind::AndroidApk.asset_name("0.3.0"),
            "Skylatch-0.3.0-android-arm64.apk"
        );
        let (package, checksums) =
            select_assets(InstallKind::AndroidApk, "0.3.0", &assets).unwrap();
        let release = UpdateRelease {
            version: "0.3.0".into(),
            kind: InstallKind::AndroidApk,
            package,
            checksums,
        };
        let notes = ReleaseNotes {
            url: Some("https://example.invalid/v0.3.0".into()),
            body: Some("fixes".into()),
        };
        assert_eq!(
            update_json(Some((release, notes))),
            json!({
                "available": true,
                "version": "0.3.0",
                "asset_name": "Skylatch-0.3.0-android-arm64.apk",
                "size": 7,
                "download_url": "https://example.invalid/Skylatch-0.3.0-android-arm64.apk",
                "checksums_url": "https://example.invalid/SHA256SUMS",
                "release_url": "https://example.invalid/v0.3.0",
                "notes": "fixes",
            })
        );
        assert!(select_assets(InstallKind::AndroidApk, "0.3.0", &assets[..1]).is_err());
        assert_eq!(
            update_json(None),
            json!({ "available": false, "version": updater::current_version() })
        );
    }
}
