use super::{classify_form_factor, DeviceFormFactor, HardwareHints, MonitorInfo};
use std::ffi::{c_char, c_void, CStr};
use std::{mem, ptr};

#[repr(C)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGGetActiveDisplayList(max_displays: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGMainDisplayID() -> u32;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingService(port: u32, matching: *mut c_void) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
}

extern "C" {
    fn sysctlbyname(
        name: *const c_char,
        old_value: *mut c_void,
        old_len: *mut usize,
        new_value: *const c_void,
        new_len: usize,
    ) -> i32;
}

fn sysctl_read(name: &CStr, buf: &mut [u8]) -> Option<usize> {
    let mut len = buf.len();
    // SAFETY: name is NUL-terminated, buf is writable for len bytes, and a null new_value
    // requests a read only.
    let status = unsafe {
        sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            ptr::null(),
            0,
        )
    };
    (status == 0 && len <= buf.len()).then_some(len)
}

fn has_internal_battery() -> bool {
    // SAFETY: IOKit consumes the matching dictionary, and the returned service is released once.
    unsafe {
        let matching = IOServiceMatching(c"AppleSmartBattery".as_ptr());
        if matching.is_null() {
            return false;
        }
        let service = IOServiceGetMatchingService(0, matching);
        if service == 0 {
            return false;
        }
        IOObjectRelease(service);
        true
    }
}

pub(super) fn form_factor() -> DeviceFormFactor {
    let mut vmm = [0; mem::size_of::<i32>()];
    if sysctl_read(c"kern.hv_vmm_present", &mut vmm) == Some(vmm.len())
        && i32::from_ne_bytes(vmm) == 1
    {
        return DeviceFormFactor::Vm;
    }
    let mut buf = [0; 256];
    let model = sysctl_read(c"hw.model", &mut buf)
        .and_then(|len| CStr::from_bytes_until_nul(&buf[..len]).ok())
        .and_then(|model| model.to_str().ok())
        .unwrap_or_default();
    classify_form_factor(HardwareHints {
        model,
        has_battery: has_internal_battery(),
        ..HardwareHints::default()
    })
}

pub(super) fn monitors() -> Vec<MonitorInfo> {
    let mut count = 0;
    // SAFETY: a null list queries the count; the second call bounds writes by the allocation.
    unsafe {
        if CGGetActiveDisplayList(0, ptr::null_mut(), &mut count) != 0 || count == 0 {
            return Vec::new();
        }
        let mut ids = vec![0; count as usize];
        if CGGetActiveDisplayList(count, ids.as_mut_ptr(), &mut count) != 0 {
            return Vec::new();
        }
        ids.truncate(count as usize);
        let main = CGMainDisplayID();
        ids.into_iter()
            .enumerate()
            .filter_map(|(index, id)| {
                let bounds = CGDisplayBounds(id);
                let width = bounds.size.width.round();
                let height = bounds.size.height.round();
                let sizes = 1.0..=f64::from(u16::MAX);
                if !sizes.contains(&width) || !sizes.contains(&height) {
                    return None;
                }
                Some(MonitorInfo {
                    id: id.to_string(),
                    name: format!("Display {}", index + 1),
                    x: bounds.origin.x.round() as i32,
                    y: bounds.origin.y.round() as i32,
                    width: width as u16,
                    height: height as u16,
                    primary: id == main,
                    active: true,
                })
            })
            .collect()
    }
}
