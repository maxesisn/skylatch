//! Startup hardware metadata, independent of the capture engine.

use skylatch_proto::{control::DeviceFormFactor, nsl1::MonitorInfo};

#[cfg(target_os = "linux")]
#[path = "hardware/linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "hardware/macos.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "hardware/windows.rs"]
mod platform;

#[derive(Debug, Default)]
pub struct HardwareProfile {
    pub form_factor: DeviceFormFactor,
    pub monitor_list: Vec<MonitorInfo>,
    /// Human-readable model such as `Mac mini (Apple M4)`; `None` when it cannot be determined.
    pub hardware_model: Option<String>,
}

#[derive(Default)]
struct HardwareHints<'a> {
    vendor: &'a str,
    model: &'a str,
    hypervisor: &'a str,
    is_vm: bool,
    has_battery: bool,
    headless_linux: bool,
}

fn classify_form_factor(hints: HardwareHints<'_>) -> DeviceFormFactor {
    use DeviceFormFactor::*;
    const VM_MARKERS: &[&str] = &[
        "vmware",
        "qemu",
        "kvm",
        "virtualbox",
        "hyper-v",
        "xen",
        "bochs",
        "parallels",
        "bhyve",
        "virtual machine",
    ];
    let model = hints.model.trim().to_ascii_lowercase();
    if hints.is_vm
        || [hints.vendor, &model, hints.hypervisor]
            .iter()
            .map(|value| value.to_ascii_lowercase())
            .any(|value| VM_MARKERS.iter().any(|marker| value.contains(marker)))
    {
        Vm
    } else if model.starts_with("macmini")
        || model.starts_with("macstudio")
        || matches!(
            model.as_str(),
            "mac13,1"
                | "mac13,2"
                | "mac14,3"
                | "mac14,12"
                | "mac14,13"
                | "mac14,14"
                | "mac15,14"
                | "mac16,9"
                | "mac16,10"
                | "mac16,11"
        )
    {
        Mini
    } else if model.starts_with("macbook") || hints.has_battery {
        Laptop
    } else if hints.headless_linux {
        Server
    } else {
        Desktop
    }
}

/// Strips NUL and other control characters and trims surrounding whitespace.
fn clean_text(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Generic firmware strings that carry no model information.
fn is_placeholder(value: &str) -> bool {
    const PLACEHOLDERS: &[&str] = &[
        "to be filled by o.e.m.",
        "system product name",
        "default string",
        "standard pc",
        "system version",
        "not specified",
        "not applicable",
        "o.e.m.",
        "oem",
        "none",
        "unknown",
        "n/a",
    ];
    let value = value.to_ascii_lowercase();
    value.is_empty() || PLACEHOLDERS.contains(&value.as_str())
}

/// Builds a model name from DMI/BIOS strings. `family` is a friendlier product name that some
/// vendors (e.g. Lenovo, whose product name is a machine-type code) publish separately.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn compose_firmware_model(vendor: &str, product: &str, family: &str) -> Option<String> {
    let (vendor, product, family) = (clean_text(vendor), clean_text(product), clean_text(family));
    if vendor.to_ascii_lowercase().contains("lenovo") && !is_placeholder(&family) {
        return Some(family);
    }
    if !is_placeholder(&product) {
        return Some(product);
    }
    // Hypervisors often publish only a bare "Standard PC"; the vendor makes it meaningful.
    (product.eq_ignore_ascii_case("standard pc") && !is_placeholder(&vendor))
        .then(|| format!("{vendor} {product}"))
}

/// Collects a startup snapshot. Call on a blocking worker when running in an async runtime.
pub fn detect_hardware_profile() -> HardwareProfile {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    let (form_factor, mut monitor_list) = (platform::form_factor(), platform::monitors());
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let (form_factor, mut monitor_list) =
        (classify_form_factor(HardwareHints::default()), Vec::new());
    if monitor_list.is_empty() {
        // Keep the legacy single-display placeholder without inventing physical bounds.
        monitor_list.push(MonitorInfo {
            id: "default".into(),
            name: "Default display".into(),
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            primary: true,
            active: false,
        });
    }
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    let hardware_model = platform::hardware_model();
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let hardware_model = None;
    HardwareProfile {
        form_factor,
        monitor_list,
        hardware_model,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DeviceFormFactor::*;

    #[test]
    fn virtualization_takes_precedence() {
        for marker in [
            "VMware",
            "QEMU",
            "KVM",
            "VirtualBox",
            "Hyper-V",
            "Xen",
            "Bochs",
            "Parallels",
            "bhyve",
            "Virtual Machine",
        ] {
            assert_eq!(
                classify_form_factor(HardwareHints {
                    vendor: marker,
                    model: "MacBookPro",
                    has_battery: true,
                    headless_linux: true,
                    ..HardwareHints::default()
                }),
                Vm,
                "{marker}"
            );
        }
        assert_eq!(
            classify_form_factor(HardwareHints {
                is_vm: true,
                model: "Mac14,3",
                ..HardwareHints::default()
            }),
            Vm
        );
        assert_eq!(
            classify_form_factor(HardwareHints {
                hypervisor: "xen",
                ..HardwareHints::default()
            }),
            Vm
        );
    }

    #[test]
    fn classifies_physical_hardware() {
        for (model, expected) in [
            ("Macmini9,1", Mini),
            ("MacStudio", Mini),
            ("Mac14,3", Mini),
            ("Mac16,11", Mini),
            ("MacBookAir", Laptop),
            ("iMac", Desktop),
            ("MacPro", Desktop),
        ] {
            assert_eq!(
                classify_form_factor(HardwareHints {
                    model,
                    ..HardwareHints::default()
                }),
                expected
            );
        }
        assert_eq!(
            classify_form_factor(HardwareHints {
                vendor: "Microsoft Corporation",
                model: "Surface Laptop",
                has_battery: true,
                headless_linux: true,
                ..HardwareHints::default()
            }),
            Laptop
        );
        assert_eq!(
            classify_form_factor(HardwareHints {
                headless_linux: true,
                ..HardwareHints::default()
            }),
            Server
        );
        assert_eq!(classify_form_factor(HardwareHints::default()), Desktop);
    }

    #[test]
    fn cleans_nul_and_whitespace() {
        assert_eq!(clean_text(" Mac mini\0\0 \n"), "Mac mini");
        assert_eq!(clean_text("Think\tPad  X1\0"), "Think Pad X1");
    }

    #[test]
    fn filters_generic_firmware_placeholders() {
        for placeholder in [
            "To be filled by O.E.M.",
            "System Product Name",
            "Default string",
            "Standard PC",
            "",
        ] {
            assert_eq!(compose_firmware_model("", placeholder, ""), None);
            assert_eq!(compose_firmware_model("ASUS", placeholder, ""), {
                (placeholder == "Standard PC").then(|| "ASUS Standard PC".to_string())
            });
        }
    }

    #[test]
    fn composes_firmware_models() {
        assert_eq!(
            compose_firmware_model("LENOVO", "21KC004UGE", "ThinkPad X1 Carbon Gen 12").as_deref(),
            Some("ThinkPad X1 Carbon Gen 12")
        );
        assert_eq!(
            compose_firmware_model("LENOVO", "21KC004UGE", "To be filled by O.E.M.").as_deref(),
            Some("21KC004UGE")
        );
        assert_eq!(
            compose_firmware_model("Dell Inc.", "XPS 13 9340\0", "XPS").as_deref(),
            Some("XPS 13 9340")
        );
        assert_eq!(
            compose_firmware_model("QEMU", "Standard PC", "").as_deref(),
            Some("QEMU Standard PC")
        );
        assert_eq!(
            compose_firmware_model("QEMU", "Standard PC (Q35 + ICH9, 2009)", "").as_deref(),
            Some("Standard PC (Q35 + ICH9, 2009)")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn detects_clean_macos_model() {
        let model = detect_hardware_profile()
            .hardware_model
            .expect("a macOS model");
        assert_eq!(model, clean_text(&model));
        eprintln!("hardware_model: {model}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires an active graphical display"]
    fn detects_native_display_geometry() {
        let profile = detect_hardware_profile();
        assert!(profile
            .monitor_list
            .iter()
            .all(|m| m.active && m.width > 0 && m.height > 0));
        assert_eq!(profile.monitor_list.iter().filter(|m| m.primary).count(), 1);
        eprintln!("{:?}: {:?}", profile.form_factor, profile.monitor_list);
    }
}
