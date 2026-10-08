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

pub fn network_url(port: u16) -> Option<String> {
    lan_ip().map(|ip| format!("http://{ip}:{port}"))
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
