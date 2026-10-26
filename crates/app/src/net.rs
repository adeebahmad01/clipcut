//! LAN address for the Network URL, and the optional desktop window.

use std::net::{IpAddr, UdpSocket};
use std::path::Path;
use std::process::Stdio;

/// The address other devices on the LAN reach us at. A UDP "connect" sends
/// nothing; it only makes the OS pick the outbound interface.
pub fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

/// The URL other devices open, or None when `host` only listens on loopback.
/// An unspecified host (`0.0.0.0`, `::`) or a hostname uses the LAN address;
/// a specific address is used as it is.
pub fn network_url(host: &str, port: u16) -> Option<String> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let ip = match bare.parse::<IpAddr>() {
        Ok(ip) if ip.is_loopback() => return None,
        Ok(ip) if !ip.is_unspecified() => ip,
        _ if bare.eq_ignore_ascii_case("localhost") => return None,
        _ => lan_ip()?,
    };
    Some(match ip {
        IpAddr::V4(ip) => format!("http://{ip}:{port}"),
        IpAddr::V6(ip) => format!("http://[{ip}]:{port}"),
    })
}

/// Open the UI as a chromeless Chromium app window with its own profile, so
/// it works even when the user's browser is already running.
pub fn open_window(chromium: &Path, url: &str, profile_dir: &Path) {
    let result = std::process::Command::new(chromium)
        .arg(format!("--app={url}"))
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg("--window-size=1440,920")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(e) = result {
        tracing::warn!("could not open the app window: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts_have_no_network_url() {
        for host in [
            "127.0.0.1",
            "127.0.0.2",
            "localhost",
            "LocalHost",
            "::1",
            "[::1]",
        ] {
            assert_eq!(network_url(host, 7878), None, "{host}");
        }
    }

    #[test]
    fn a_specific_address_is_used_as_it_is() {
        assert_eq!(
            network_url("192.168.1.20", 7878).as_deref(),
            Some("http://192.168.1.20:7878")
        );
        assert_eq!(
            network_url("fd00::5", 80).as_deref(),
            Some("http://[fd00::5]:80")
        );
        // Unspecified hosts use the LAN address, when there is one.
        assert_eq!(
            network_url("0.0.0.0", 7878),
            lan_ip().map(|ip| format!("http://{ip}:7878"))
        );
    }
}
