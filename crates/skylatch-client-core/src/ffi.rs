//! Minimal C ABI.
//!
//! All functions are safe to call from any thread and never panic across the boundary.

use skylatch_proto::nsl1::{Header, Nsl1Error};
use std::os::raw::{c_char, c_int};

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

static VERSION_C: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");

/// Returns a static NUL-terminated version string. The pointer is valid for the process lifetime.
#[no_mangle]
pub extern "C" fn skylatch_proto_version() -> *const c_char {
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

#[cfg(test)]
mod tests {
    use super::*;
    use skylatch_proto::nsl1::Message;
    use std::ffi::CStr;

    #[test]
    fn version_string() {
        let s = unsafe { CStr::from_ptr(skylatch_proto_version()) };
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
