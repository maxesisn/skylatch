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
    HardwareProfile {
        form_factor,
        monitor_list,
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
