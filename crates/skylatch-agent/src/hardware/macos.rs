use super::{classify_form_factor, clean_text, DeviceFormFactor, HardwareHints, MonitorInfo};
use std::ffi::{c_char, c_void, CStr};
use std::process::{Command, Stdio};
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

fn sysctl_string(name: &CStr) -> String {
    let mut buf = [0; 256];
    sysctl_read(name, &mut buf)
        .map(|len| String::from_utf8_lossy(&buf[..len]).into_owned())
        .map(|value| clean_text(&value))
        .unwrap_or_default()
}

/// Extracts a property value from `ioreg` output; handles both `<"text">` and `<hex>` forms.
fn parse_ioreg_property(output: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\" = <");
    let rest = output.lines().find_map(|line| {
        let line = line.trim_start();
        line.strip_prefix(needle.as_str())
    })?;
    let value = rest.trim_end().strip_suffix('>')?;
    let text = match value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        Some(quoted) => quoted.to_string(),
        None => {
            let bytes: Option<Vec<u8>> = (value.len() % 2 == 0)
                .then(|| {
                    (0..value.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(value.get(i..i + 2)?, 16).ok())
                        .collect()
                })
                .flatten();
            String::from_utf8_lossy(&bytes?).into_owned()
        }
    };
    Some(clean_text(&text)).filter(|text| !text.is_empty())
}

fn ioreg_product_name() -> Option<String> {
    let output = Command::new("/usr/sbin/ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    parse_ioreg_property(&String::from_utf8_lossy(&output.stdout), "product-name")
}

/// Marketing family for a `hw.model` identifier, for Macs whose IORegistry has no `product-name`.
fn family_from_identifier(identifier: &str) -> Option<&'static str> {
    const PREFIXES: &[(&str, &str)] = &[
        ("Macmini", "Mac mini"),
        ("MacStudio", "Mac Studio"),
        ("MacBookPro", "MacBook Pro"),
        ("MacBookAir", "MacBook Air"),
        ("MacBook", "MacBook"),
        ("iMacPro", "iMac Pro"),
        ("iMac", "iMac"),
        ("MacPro", "Mac Pro"),
    ];
    // Apple Silicon identifiers of the form `MacNN,M` do not encode the family.
    const EXACT: &[(&str, &str)] = &[
        ("Mac14,3", "Mac mini"),
        ("Mac14,12", "Mac mini"),
        ("Mac16,10", "Mac mini"),
        ("Mac16,11", "Mac mini"),
        ("Mac13,1", "Mac Studio"),
        ("Mac13,2", "Mac Studio"),
        ("Mac14,13", "Mac Studio"),
        ("Mac14,14", "Mac Studio"),
        ("Mac15,14", "Mac Studio"),
        ("Mac16,9", "Mac Studio"),
    ];
    EXACT
        .iter()
        .find(|(id, _)| *id == identifier)
        .or_else(|| {
            PREFIXES
                .iter()
                .find(|(prefix, _)| identifier.starts_with(prefix))
        })
        .map(|(_, family)| *family)
}

/// `Mac mini (Apple M4)`; the chip is only appended for Apple Silicon, and the identifier is
/// the fallback when the family is unknown.
fn format_mac_model(product_name: &str, identifier: &str, chip: &str) -> Option<String> {
    let (product_name, identifier, chip) = (
        clean_text(product_name),
        clean_text(identifier),
        clean_text(chip),
    );
    let name = if !product_name.is_empty() {
        product_name
    } else if let Some(family) = family_from_identifier(&identifier) {
        family.to_string()
    } else {
        identifier
    };
    if name.is_empty() {
        None
    } else if chip.starts_with("Apple ") {
        Some(format!("{name} ({chip})"))
    } else {
        Some(name)
    }
}

pub(super) fn hardware_model() -> Option<String> {
    format_mac_model(
        &ioreg_product_name().unwrap_or_default(),
        &sysctl_string(c"hw.model"),
        &sysctl_string(c"machdep.cpu.brand_string"),
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ioreg_properties() {
        let out =
            "  | {\n      \"product-name\" = <\"Mac mini\">\n      \"model\" = <\"Mac16,10\">\n";
        assert_eq!(
            parse_ioreg_property(out, "product-name").as_deref(),
            Some("Mac mini")
        );
        assert_eq!(
            parse_ioreg_property(out, "model").as_deref(),
            Some("Mac16,10")
        );
        assert_eq!(parse_ioreg_property(out, "missing"), None);
        // Data-typed property with a trailing NUL byte.
        let hex = "      \"product-name\" = <4d6163204d696e6900>\n";
        assert_eq!(
            parse_ioreg_property(hex, "product-name").as_deref(),
            Some("Mac Mini")
        );
    }

    #[test]
    fn formats_mac_models() {
        assert_eq!(
            format_mac_model("MacBook Pro", "Mac16,1", "Apple M4 Pro").as_deref(),
            Some("MacBook Pro (Apple M4 Pro)")
        );
        assert_eq!(
            format_mac_model("", "Mac16,10", "Apple M4").as_deref(),
            Some("Mac mini (Apple M4)")
        );
        assert_eq!(
            format_mac_model("", "Mac13,1\0", "Apple M1 Max").as_deref(),
            Some("Mac Studio (Apple M1 Max)")
        );
        assert_eq!(
            format_mac_model("", "MacBookPro16,1", "Intel(R) Core(TM) i9-9980HK").as_deref(),
            Some("MacBook Pro")
        );
        assert_eq!(
            format_mac_model(" \0", "Mac99,9", "Apple M9").as_deref(),
            Some("Mac99,9 (Apple M9)")
        );
        assert_eq!(format_mac_model("", "", ""), None);
    }
}
