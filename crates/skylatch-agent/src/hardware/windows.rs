use super::{classify_form_factor, DeviceFormFactor, HardwareHints, MonitorInfo};
use std::ffi::c_void;
use std::{mem, ptr};

type Handle = *mut c_void;

#[repr(C)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct MonitorInfoExW {
    size: u32,
    monitor: Rect,
    work: Rect,
    flags: u32,
    device: [u16; 32],
}

#[repr(C)]
struct SystemPowerStatus {
    ac_line_status: u8,
    battery_flag: u8,
    battery_life_percent: u8,
    system_status_flag: u8,
    battery_life_time: u32,
    battery_full_life_time: u32,
}

type MonitorCallback = unsafe extern "system" fn(Handle, Handle, *mut Rect, isize) -> i32;

#[link(name = "user32")]
extern "system" {
    fn EnumDisplayMonitors(
        hdc: Handle,
        clip: *const Rect,
        callback: MonitorCallback,
        data: isize,
    ) -> i32;
    fn GetMonitorInfoW(monitor: Handle, info: *mut MonitorInfoExW) -> i32;
    fn SetThreadDpiAwarenessContext(context: Handle) -> Handle;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetSystemPowerStatus(status: *mut SystemPowerStatus) -> i32;
}

#[link(name = "advapi32")]
extern "system" {
    fn RegGetValueW(
        key: Handle,
        subkey: *const u16,
        value: *const u16,
        flags: u32,
        value_type: *mut u32,
        data: *mut c_void,
        size: *mut u32,
    ) -> i32;
}

fn bios_value(value: &str) -> String {
    const HKEY_LOCAL_MACHINE: Handle = 0x8000_0002_u32 as i32 as isize as Handle;
    const RRF_RT_REG_SZ: u32 = 2;
    let subkey: Vec<_> = r"HARDWARE\DESCRIPTION\System\BIOS"
        .encode_utf16()
        .chain([0])
        .collect();
    let value: Vec<_> = value.encode_utf16().chain([0]).collect();
    let mut output = [0u16; 256];
    let mut bytes = mem::size_of_val(&output) as u32;
    // SAFETY: both names are NUL-terminated; size is the buffer capacity in bytes.
    if unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            output.as_mut_ptr().cast(),
            &mut bytes,
        )
    } != 0
    {
        return String::new();
    }
    utf16_name(&output)
}

fn utf16_name(name: &[u16]) -> String {
    let end = name
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(name.len());
    String::from_utf16_lossy(&name[..end])
}

pub(super) fn form_factor() -> DeviceFormFactor {
    let model = bios_value("SystemProductName");
    let vendor = bios_value("SystemManufacturer");
    let mut status = mem::MaybeUninit::<SystemPowerStatus>::uninit();
    // SAFETY: a successful call initializes the whole status structure.
    let has_battery = unsafe {
        GetSystemPowerStatus(status.as_mut_ptr()) != 0
            && !matches!(status.assume_init().battery_flag, 128 | 255)
    };
    classify_form_factor(HardwareHints {
        model: &model,
        vendor: &vendor,
        has_battery,
        ..HardwareHints::default()
    })
}

impl MonitorInfoExW {
    fn monitor_info(&self) -> Option<MonitorInfo> {
        let rect = &self.monitor;
        let width = u16::try_from(rect.right.checked_sub(rect.left)?).ok()?;
        let height = u16::try_from(rect.bottom.checked_sub(rect.top)?).ok()?;
        let name = utf16_name(&self.device);
        if width == 0 || height == 0 || name.is_empty() {
            return None;
        }
        Some(MonitorInfo {
            id: name.clone(),
            name,
            x: rect.left,
            y: rect.top,
            width,
            height,
            primary: self.flags & 1 != 0,
            active: true,
        })
    }
}

unsafe extern "system" fn collect_monitor(
    monitor: Handle,
    _: Handle,
    _: *mut Rect,
    data: isize,
) -> i32 {
    // SAFETY: the structure contains only integers, and the callback data is the exclusive
    // vector passed to the synchronous EnumDisplayMonitors call below.
    unsafe {
        let mut info: MonitorInfoExW = mem::zeroed();
        info.size = mem::size_of::<MonitorInfoExW>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            if let Some(info) = info.monitor_info() {
                (*(data as *mut Vec<MonitorInfo>)).push(info);
            }
        }
    }
    1
}

pub(super) fn monitors() -> Vec<MonitorInfo> {
    let mut monitors = Vec::new();
    // Per-monitor DPI awareness prevents DPI virtualization of mixed-scale desktop bounds.
    // SAFETY: a null hdc/clip enumerates the virtual desktop; callbacks finish before the
    // function returns. The DPI context applies only to this thread and is restored below.
    unsafe {
        let previous = SetThreadDpiAwarenessContext(-3_isize as Handle);
        let ok = EnumDisplayMonitors(
            ptr::null_mut(),
            ptr::null(),
            collect_monitor,
            (&mut monitors as *mut Vec<MonitorInfo>) as isize,
        );
        if !previous.is_null() {
            SetThreadDpiAwarenessContext(previous);
        }
        if ok == 0 {
            monitors.clear();
        }
    }
    monitors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_native_monitor_rectangles_without_swapping_axes() {
        let device = [u16::from(b'D'); 32];
        let work = || Rect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let landscape = MonitorInfoExW {
            size: 0,
            monitor: Rect {
                left: 0,
                top: 0,
                right: 5120,
                bottom: 2880,
            },
            work: work(),
            flags: 1,
            device,
        }
        .monitor_info()
        .unwrap();
        let portrait = MonitorInfoExW {
            size: 0,
            monitor: Rect {
                left: -1440,
                top: 120,
                right: 0,
                bottom: 2680,
            },
            work: work(),
            flags: 0,
            device,
        }
        .monitor_info()
        .unwrap();
        assert_eq!(
            (landscape.x, landscape.y, landscape.width, landscape.height),
            (0, 0, 5120, 2880)
        );
        assert!(landscape.primary && landscape.active);
        assert_eq!(
            (portrait.x, portrait.y, portrait.width, portrait.height),
            (-1440, 120, 1440, 2560)
        );
        assert!(!portrait.primary && portrait.active);
    }
}
