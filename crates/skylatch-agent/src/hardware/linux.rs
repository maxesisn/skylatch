use super::{classify_form_factor, DeviceFormFactor, HardwareHints, MonitorInfo};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn env_is_set(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|value| !value.is_empty())
}

pub(super) fn form_factor() -> DeviceFormFactor {
    let model = std::fs::read_to_string("/sys/class/dmi/id/product_name").unwrap_or_default();
    let vendor = std::fs::read_to_string("/sys/class/dmi/id/sys_vendor").unwrap_or_default();
    let hypervisor = std::fs::read_to_string("/sys/hypervisor/type").unwrap_or_default();
    let has_battery = std::fs::read_dir("/sys/class/power_supply")
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| entry.file_name().as_encoded_bytes().starts_with(b"BAT"));
    classify_form_factor(HardwareHints {
        vendor: &vendor,
        model: &model,
        hypervisor: &hypervisor,
        has_battery,
        headless_linux: !env_is_set("DISPLAY") && !env_is_set("WAYLAND_DISPLAY"),
        ..HardwareHints::default()
    })
}

pub(super) fn monitors() -> Vec<MonitorInfo> {
    if !env_is_set("DISPLAY") {
        return Vec::new();
    }
    query_xrandr().map_or_else(Vec::new, |output| parse_xrandr(&output))
}

fn query_xrandr() -> Option<String> {
    let mut child = Command::new("xrandr")
        .arg("--query")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    // Drain all mode lines so the child cannot block on a full pipe while we wait.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut output = String::new();
        stdout.read_to_string(&mut output).ok()?;
        Some(output)
    });
    // An unreachable or hung X server should not delay registration indefinitely.
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return reader.join().ok().flatten(),
            Ok(Some(_)) => return None,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn parse_xrandr(output: &str) -> Vec<MonitorInfo> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            if fields.next()? != "connected" {
                return None;
            }
            let next = fields.next()?;
            let primary = next == "primary";
            let geometry = if primary { fields.next()? } else { next };
            let (width, rest) = geometry.split_once('x')?;
            let first_sign = rest.find(['+', '-'])?;
            let (height, origin) = rest.split_at(first_sign);
            // xrandr can print a negative position with a literal leading '+' separator.
            let origin = origin.strip_prefix('+').unwrap_or(origin);
            let second_sign = 1 + origin.get(1..)?.find(['+', '-'])?;
            let (x, y) = origin.split_at(second_sign);
            let y = y.strip_prefix('+').unwrap_or(y);
            Some(MonitorInfo {
                id: name.into(),
                name: name.into(),
                x: x.parse().ok()?,
                y: y.parse().ok()?,
                width: width.parse().ok()?,
                height: height.parse().ok()?,
                primary,
                active: true,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_active_outputs_with_signed_origins() {
        let monitors = parse_xrandr(
            "\
Screen 0: minimum 8 x 8, current 6560 x 2880, maximum 32767 x 32767
DP-1 connected primary 5120x2880+0+0 (normal left inverted right x axis y axis)
   5120x2880     60.00*+
DP-2 connected 1440x2560+-1440-42 left (normal left inverted right x axis y axis)
HDMI-1 connected (normal left inverted right x axis y axis)
HDMI-2 disconnected (normal left inverted right x axis y axis)",
        );
        assert_eq!(
            monitors,
            [
                MonitorInfo {
                    id: "DP-1".into(),
                    name: "DP-1".into(),
                    x: 0,
                    y: 0,
                    width: 5120,
                    height: 2880,
                    primary: true,
                    active: true
                },
                MonitorInfo {
                    id: "DP-2".into(),
                    name: "DP-2".into(),
                    x: -1440,
                    y: -42,
                    width: 1440,
                    height: 2560,
                    primary: false,
                    active: true
                },
            ]
        );
    }
}
