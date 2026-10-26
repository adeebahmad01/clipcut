//! QR code of the Network URL, drawn in the terminal and as SVG for Settings.

use qrcode::{Color, EcLevel, QrCode};

/// Light modules around the code. The spec asks for 4; phones read 2 fine,
/// and it keeps the terminal code small.
const TERMINAL_QUIET: usize = 2;
const SVG_QUIET: usize = 4;

pub struct Qr {
    /// Modules per side, without the quiet zone.
    width: usize,
    dark: Vec<bool>,
}

impl Qr {
    pub fn new(text: &str) -> anyhow::Result<Self> {
        let code = QrCode::with_error_correction_level(text, EcLevel::L)?;
        Ok(Self {
            width: code.width(),
            dark: code
                .to_colors()
                .into_iter()
                .map(|c| c == Color::Dark)
                .collect(),
        })
    }

    /// Coordinates include the quiet zone, which is light.
    fn is_dark(&self, x: usize, y: usize, quiet: usize) -> bool {
        let (Some(x), Some(y)) = (x.checked_sub(quiet), y.checked_sub(quiet)) else {
            return false;
        };
        x < self.width && y < self.width && self.dark[y * self.width + x]
    }

    /// Lines of Unicode half blocks, two module rows per line. The colors are
    /// set explicitly so the code reads the same on light and dark terminals.
    pub fn terminal(&self) -> Vec<String> {
        let side = self.width + 2 * TERMINAL_QUIET;
        (0..side)
            .step_by(2)
            .map(|y| {
                let mut line = String::from("\x1b[97;40m");
                for x in 0..side {
                    let top = !self.is_dark(x, y, TERMINAL_QUIET);
                    // The odd last row has nothing below it.
                    let bottom = y + 1 < side && !self.is_dark(x, y + 1, TERMINAL_QUIET);
                    line.push(match (top, bottom) {
                        (true, true) => '█',
                        (true, false) => '▀',
                        (false, true) => '▄',
                        (false, false) => ' ',
                    });
                }
                line.push_str("\x1b[0m");
                line
            })
            .collect()
    }

    /// A square SVG, one unit per module, with each run of dark modules as a
    /// single rectangle.
    pub fn svg(&self) -> String {
        let side = self.width + 2 * SVG_QUIET;
        let mut path = String::new();
        for y in 0..side {
            let mut x = 0;
            while x < side {
                if !self.is_dark(x, y, SVG_QUIET) {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < side && self.is_dark(x, y, SVG_QUIET) {
                    x += 1;
                }
                path.push_str(&format!("M{start} {y}h{}v1H{start}z", x - start));
            }
        }
        format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {side} {side}" shape-rendering="crispEdges"><rect width="{side}" height="{side}" fill="#fff"/><path fill="#000" d="{path}"/></svg>"##
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_network_url_fits_an_80_column_terminal() {
        let qr = Qr::new("http://192.168.100.226:7878").unwrap();
        // Version 2 at the lowest error correction holds 32 bytes.
        assert_eq!(qr.width, 25);
        let lines = qr.terminal();
        assert_eq!(lines.len(), (25 + 2 * TERMINAL_QUIET).div_ceil(2));
        for line in &lines {
            let visible = line
                .trim_start_matches("\x1b[97;40m")
                .trim_end_matches("\x1b[0m");
            assert_eq!(visible.chars().count(), 25 + 2 * TERMINAL_QUIET);
        }
        // The quiet zone is light: the top line is all full blocks.
        assert!(lines[0].contains(&"█".repeat(25 + 2 * TERMINAL_QUIET)));
        // A long IPv6 URL still fits beside the two-space indent.
        let qr = Qr::new("http://[fd12:3456:789a:1:abcd:ef01:2345:6789]:7878").unwrap();
        assert!(qr.width + 2 * TERMINAL_QUIET + 2 <= 80);
    }

    #[test]
    fn the_svg_draws_every_dark_module_and_nothing_else() {
        let qr = Qr::new("http://10.0.0.5:7878").unwrap();
        let svg = qr.svg();
        let side = qr.width + 2 * SVG_QUIET;
        assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(svg.contains(&format!("viewBox=\"0 0 {side} {side}\"")));
        assert!(svg.ends_with("</svg>"));
        // Add up the run lengths and compare with the matrix.
        let d = svg.split(" d=\"").nth(1).unwrap();
        let drawn: usize = d
            .split('h')
            .skip(1)
            .map(|run| run.split('v').next().unwrap().parse::<usize>().unwrap())
            .sum();
        assert_eq!(drawn, qr.dark.iter().filter(|&&d| d).count());
        // The finder pattern's top edge is one run of seven, inside the quiet zone.
        assert!(d.starts_with(&format!("M{SVG_QUIET} {SVG_QUIET}h7v1H{SVG_QUIET}z")));
    }
}
