//! 32-point radial stick calibration.
//!
//! Ported directly from enable_procon2.py StickCalibrator.

/// Stick calibrator with 32 radial calibration points and deadzone.
#[derive(Clone)]
pub struct StickCalibrator {
    radii: [f64; 32],
    deadzone: f64,
}

/// Hardcoded calibration data for main (left) stick.
pub const MAIN_STICK_CAL: &str = "61.28 59.10 59.32 61.42 64.61 60.89 58.93 58.86 57.96 54.91 53.94 55.08 58.76 55.50 52.94 53.47 56.88 54.62 54.06 55.79 59.53 58.33 56.91 58.23 60.40 61.90 61.76 63.32 68.50 63.34 61.14 60.96";

/// Hardcoded calibration data for C (right) stick.
pub const C_STICK_CAL: &str = "54.74 52.52 52.24 54.58 58.28 55.75 54.01 54.52 55.03 53.14 52.31 53.07 56.86 52.77 51.99 52.16 53.86 52.02 51.43 53.31 56.98 53.29 52.09 52.24 55.01 53.96 53.79 56.05 59.98 56.49 54.20 54.46";

impl StickCalibrator {
    pub fn new(calibration_str: &str, deadzone: f64) -> Self {
        let mut radii = [0.0f64; 32];
        for (i, val) in calibration_str.split_whitespace().enumerate() {
            if i < 32 {
                radii[i] = val.parse().unwrap_or(50.0);
            }
        }
        Self { radii, deadzone }
    }

    /// Calibrate a centered stick position.
    ///
    /// Input: raw centered values (raw - center), range roughly [-2048, 2048].
    /// Output: calibrated values, range roughly [-100, 100].
    pub fn calibrate(&self, x: f64, y: f64) -> (f64, f64) {
        let magnitude = (x * x + y * y).sqrt() / 1.3;

        if magnitude < self.deadzone {
            return (0.0, 0.0);
        }

        let mut angle = y.atan2(x);
        if angle < 0.0 {
            angle += 2.0 * std::f64::consts::PI;
        }

        let angle_percent = angle / (2.0 * std::f64::consts::PI);
        let float_index = angle_percent * 32.0;
        let index1 = (float_index as usize) % 32;
        let index2 = (index1 + 1) % 32;
        let fraction = float_index - float_index.floor();

        let r1 = self.radii[index1];
        let r2 = self.radii[index2];
        let calibrated_radius_pct = r1 + (r2 - r1) * fraction;

        let scale_factor = 100.0 / calibrated_radius_pct;
        let corrected_magnitude = magnitude * scale_factor;

        let corrected_x = corrected_magnitude * angle.cos();
        let corrected_y = corrected_magnitude * angle.sin();

        (corrected_x, corrected_y)
    }
}

/// Auto-calibrate stick centers from a set of idle reports.
///
/// Returns (left_center, right_center) as (x, y) averages.
pub fn auto_calibrate_centers(reports: &[[u8; 64]]) -> ((u16, u16), (u16, u16)) {
    if reports.is_empty() {
        return ((2048, 2048), (2048, 2048));
    }

    let mut lx_sum: u64 = 0;
    let mut ly_sum: u64 = 0;
    let mut rx_sum: u64 = 0;
    let mut ry_sum: u64 = 0;

    for report in reports {
        let parsed = crate::input::parse_hid_report(report);
        lx_sum += parsed.left_stick_raw.0 as u64;
        ly_sum += parsed.left_stick_raw.1 as u64;
        rx_sum += parsed.right_stick_raw.0 as u64;
        ry_sum += parsed.right_stick_raw.1 as u64;
    }

    let n = reports.len() as u64;
    (
        ((lx_sum / n) as u16, (ly_sum / n) as u16),
        ((rx_sum / n) as u16, (ry_sum / n) as u16),
    )
}
