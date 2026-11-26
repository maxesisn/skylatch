// SPDX-License-Identifier: Apache-2.0

//! Blocking hub HTTP and self-update exports. Call them off the main thread; each returns a
//! JSON envelope.

use crate::{clear_exception, json_reply, read_optional, read_string, runtime, update_json, wait};
use jni::{
    objects::{JMethodID, JObject, JString, JValue},
    signature::{Primitive, ReturnType},
    sys::jstring,
    JNIEnv,
};
use serde_json::{json, Value};
use skylatch_client_core::{
    self as core,
    updater::{self, Asset, CheckOptions, DownloadProgress, InstallKind, UpdateRelease},
};
use std::{path::PathBuf, sync::mpsc};

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeVersion(
    mut env: JNIEnv,
    _this: JObject,
) -> jstring {
    crate::new_jstring(&mut env, updater::current_version())
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeHubInfo(
    mut env: JNIEnv,
    _this: JObject,
    hub_url: JString,
) -> jstring {
    json_reply(&mut env, |env| {
        let hub_url = read_string(env, &hub_url, "hubUrl")?;
        let info = wait(core::hub_info(&hub_url))?;
        Ok(json!({ "data": info }))
    })
}

fn credentials(
    env: &mut JNIEnv,
    hub_url: &JString,
    username: &JString,
    password: &JString,
) -> Result<(String, String, String), String> {
    Ok((
        read_string(env, hub_url, "hubUrl")?,
        read_string(env, username, "username")?,
        read_string(env, password, "password")?,
    ))
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeLogin(
    mut env: JNIEnv,
    _this: JObject,
    hub_url: JString,
    username: JString,
    password: JString,
) -> jstring {
    json_reply(&mut env, |env| {
        let (hub_url, username, password) = credentials(env, &hub_url, &username, &password)?;
        let session = wait(core::login(&hub_url, &username, &password))?;
        Ok(json!({ "token": session.token, "account": session.account }))
    })
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeSignup(
    mut env: JNIEnv,
    _this: JObject,
    hub_url: JString,
    username: JString,
    password: JString,
) -> jstring {
    json_reply(&mut env, |env| {
        let (hub_url, username, password) = credentials(env, &hub_url, &username, &password)?;
        let session = wait(core::signup(&hub_url, &username, &password))?;
        Ok(json!({ "token": session.token, "account": session.account }))
    })
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeListDevices(
    mut env: JNIEnv,
    _this: JObject,
    hub_url: JString,
    token: JString,
) -> jstring {
    json_reply(&mut env, |env| {
        let hub_url = read_string(env, &hub_url, "hubUrl")?;
        let token = read_string(env, &token, "token")?;
        let devices = wait(core::list_devices(&hub_url, &token))?;
        Ok(json!({ "devices": devices }))
    })
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeDeleteDevice(
    mut env: JNIEnv,
    _this: JObject,
    hub_url: JString,
    token: JString,
    device_id: JString,
) -> jstring {
    json_reply(&mut env, |env| {
        let hub_url = read_string(env, &hub_url, "hubUrl")?;
        let token = read_string(env, &token, "token")?;
        let device_id = read_string(env, &device_id, "deviceId")?;
        wait(core::delete_device(&hub_url, &token, &device_id))?;
        Ok(Value::Null)
    })
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeCheckUpdate(
    mut env: JNIEnv,
    _this: JObject,
    api_url: JString,
) -> jstring {
    json_reply(&mut env, |env| {
        let options = CheckOptions {
            api_url: read_optional(env, &api_url, "apiUrl")?,
            kind: Some(InstallKind::AndroidApk),
            ..CheckOptions::default()
        };
        Ok(update_json(wait(updater::check_for_update_with_notes(
            options,
        ))?))
    })
}

#[no_mangle]
pub extern "system" fn Java_io_github_maxesisn_skylatch_NativeBridge_nativeDownloadAndVerifyApk(
    mut env: JNIEnv,
    _this: JObject,
    version: JString,
    download_url: JString,
    checksums_url: JString,
    asset_name: JString,
    dest_dir: JString,
    listener: JObject,
) -> jstring {
    json_reply(&mut env, |env| {
        let version = read_string(env, &version, "version")?;
        let download_url = read_string(env, &download_url, "downloadUrl")?;
        let checksums_url = read_string(env, &checksums_url, "checksumsUrl")?;
        let asset_name = read_string(env, &asset_name, "assetName")?;
        let dest_dir = PathBuf::from(read_string(env, &dest_dir, "destDir")?);
        if !asset_name.ends_with(".apk") {
            return Err(format!("{asset_name:?} is not an APK"));
        }
        let on_progress = progress_method(env, &listener)?;
        let release = UpdateRelease {
            version,
            kind: InstallKind::AndroidApk,
            package: Asset {
                name: asset_name,
                url: download_url,
                size: None,
            },
            checksums: Asset {
                name: "SHA256SUMS".into(),
                url: checksums_url,
                size: None,
            },
        };

        let rt = runtime()?;
        let (progress, updates) = mpsc::channel::<DownloadProgress>();
        let task = rt.spawn(async move {
            updater::download_and_verify(&release, &dest_dir, move |p| {
                let _ = progress.send(p);
            })
            .await
        });
        // The channel closes once the download future drops its progress callback.
        while let Ok(mut latest) = updates.recv() {
            while let Ok(next) = updates.try_recv() {
                latest = next;
            }
            if let Some(method) = on_progress {
                report_progress(env, &listener, method, latest);
            }
        }
        let package = rt
            .block_on(task)
            .map_err(|_| "APK download task failed".to_owned())?
            .map_err(|e| e.to_string())?;
        Ok(json!({
            "path": package.path.to_string_lossy(),
            "sha256": package.sha256,
            "version": package.version,
        }))
    })
}

fn progress_method(env: &mut JNIEnv, listener: &JObject) -> Result<Option<JMethodID>, String> {
    if listener.is_null() {
        return Ok(None);
    }
    let method = env
        .get_object_class(listener)
        .and_then(|class| env.get_method_id(&class, "onProgress", "(JJ)V"));
    match method {
        Ok(method) => Ok(Some(method)),
        Err(_) => {
            clear_exception(env);
            Err("listener has no onProgress(downloadedBytes: Long, totalBytes: Long)".into())
        }
    }
}

fn report_progress(
    env: &mut JNIEnv,
    listener: &JObject,
    method: JMethodID,
    progress: DownloadProgress,
) {
    let total = progress
        .total_bytes
        .and_then(|t| i64::try_from(t).ok())
        .unwrap_or(-1);
    let args = [
        JValue::Long(i64::try_from(progress.downloaded_bytes).unwrap_or(i64::MAX)).as_jni(),
        JValue::Long(total).as_jni(),
    ];
    // SAFETY: `method` was resolved on the listener's class with signature `(JJ)V`.
    let called = unsafe {
        env.call_method_unchecked(
            listener,
            method,
            ReturnType::Primitive(Primitive::Void),
            &args,
        )
    };
    if called.is_err() {
        clear_exception(env);
    }
}
