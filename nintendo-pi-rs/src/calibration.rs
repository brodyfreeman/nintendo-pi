//! Stick calibration: centering, radial correction, deadzone, and normalization.
//!
//! `StickPair` is the main entry point — it owns calibrators and centers for
//! both sticks and produces values ready for the BT report builder.

use tracing::{debug, trace, warn};

/// Stick calibrator with 32 radial calibration points and deadzone.
#[derive(Clone)]
pub struct StickCalibrator {
    radii: [f64; 32],
    pub deadzone: f64,
}

/// Default stick deadzone.
pub const DEFAULT_DEADZONE: f64 = 10.0;

/// Hardcoded calibration data for main (left) stick.
pub const MAIN_STICK_CAL: &str = "61.28 59.10 59.32 61.42 64.61 60.89 58.93 58.86 57.96 54.91 53.94 55.08 58.76 55.50 52.94 53.47 56.88 54.62 54.06 55.79 59.53 58.33 56.91 58.23 60.40 61.90 61.76 63.32 68.50 63.34 61.14 60.96";

/// Hardcoded calibration data for C (right) stick.
pub const C_STICK_CAL: &str = "54.74 52.52 52.24 54.58 58.28 55.75 54.01 54.52 55.03 53.14 52.31 53.07 56.86 52.77 51.99 52.16 53.86 52.02 51.43 53.31 56.98 53.29 52.09 52.24 55.01 53.96 53.79 56.05 59.98 56.49 54.20 54.46";

impl StickCalibrator {
    pub fn new(calibration_str: &str, deadzone: f64) -> Self {
        let mut radii = [0.0f64; 32];
        let mut count = 0;
        for (i, val) in calibration_str.split_whitespace().enumerate() {
            if i < 32 {
                radii[i] = val.parse().unwrap_or(50.0);
                count += 1;
            }
        }
        if count < 32 {
            warn!("[CAL] Only parsed {count}/32 calibration radii, remaining default to 0.0");
        }
        let min_r = radii.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_r = radii.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        debug!("[CAL] Loaded {count} radii (range {min_r:.1}–{max_r:.1}), deadzone={deadzone:.1}");
        Self { radii, deadzone }
    }

    /// Calibrate a centered stick position.
    ///
    /// Input: raw centered values (raw - center), range roughly [-2048, 2048].
    /// Output: calibrated values, range roughly [-100, 100].
    pub fn calibrate(&self, x: f64, y: f64) -> (f64, f64) {
        let magnitude = (x * x + y * y).sqrt() / 1.3;

        if magnitude < self.deadzone {
            trace!(
                "[CAL] Deadzone: raw=({x:.1},{y:.1}) mag={magnitude:.1} < {dz:.1}",
                dz = self.deadzone,
            );
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

        trace!(
            "[CAL] raw=({x:.1},{y:.1}) mag={magnitude:.1} angle={deg:.1}° pts=[{index1},{index2}] \
             radius={calibrated_radius_pct:.1} → ({corrected_x:.1},{corrected_y:.1})",
            deg = angle.to_degrees(),
        );

        (corrected_x, corrected_y)
    }
}

/// Both sticks with their calibrators and auto-calibrated centers.
///
/// Full pipeline: raw 12-bit stick → centered → radial calibration → normalized to [-100, 100].
pub struct StickPair {
    left: StickCalibrator,
    right: StickCalibrator,
    left_center: (u16, u16),
    right_center: (u16, u16),
}

impl StickPair {
    pub fn new(
        left: StickCalibrator,
        right: StickCalibrator,
        left_center: (u16, u16),
        right_center: (u16, u16),
    ) -> Self {
        Self {
            left,
            right,
            left_center,
            right_center,
        }
    }

    /// Calibrate both sticks from raw 12-bit values.
    ///
    /// Returns (left, right) as (x, y) in [-100, 100], ready for the BT report.
    pub fn calibrate(
        &self,
        left_raw: (u16, u16),
        right_raw: (u16, u16),
    ) -> ((f64, f64), (f64, f64)) {
        (
            calibrate_one(&self.left, left_raw, self.left_center),
            calibrate_one(&self.right, right_raw, self.right_center),
        )
    }

    pub fn set_deadzone(&mut self, deadzone: f64) {
        self.left.deadzone = deadzone;
        self.right.deadzone = deadzone;
    }
}

/// Center, calibrate, and normalize a single stick.
fn calibrate_one(calibrator: &StickCalibrator, raw: (u16, u16), center: (u16, u16)) -> (f64, f64) {
    let x_c = raw.0 as f64 - center.0 as f64;
    let y_c = raw.1 as f64 - center.1 as f64;
    let (x_cal, y_cal) = calibrator.calibrate(x_c, y_c);
    (
        (x_cal * 100.0 / 2048.0).clamp(-100.0, 100.0),
        (y_cal * 100.0 / 2048.0).clamp(-100.0, 100.0),
    )
}

/// Auto-calibrate stick centers from a set of idle reports.
///
/// Returns (left_center, right_center) as (x, y) averages.
pub fn auto_calibrate_centers(reports: &[[u8; 64]]) -> ((u16, u16), (u16, u16)) {
    if reports.is_empty() {
        warn!("[CAL] No reports for auto-calibration, using default centers (2048, 2048)");
        return ((2048, 2048), (2048, 2048));
    }

    let mut lx_sum: u64 = 0;
    let mut ly_sum: u64 = 0;
    let mut rx_sum: u64 = 0;
    let mut ry_sum: u64 = 0;

    let mut lx_min = u16::MAX;
    let mut lx_max = u16::MIN;
    let mut ly_min = u16::MAX;
    let mut ly_max = u16::MIN;
    let mut rx_min = u16::MAX;
    let mut rx_max = u16::MIN;
    let mut ry_min = u16::MAX;
    let mut ry_max = u16::MIN;

    for report in reports {
        let parsed = crate::input::parse_hid_report(report);
        let (lx, ly) = parsed.left_stick_raw;
        let (rx, ry) = parsed.right_stick_raw;

        lx_sum += lx as u64;
        ly_sum += ly as u64;
        rx_sum += rx as u64;
        ry_sum += ry as u64;

        lx_min = lx_min.min(lx);
        lx_max = lx_max.max(lx);
        ly_min = ly_min.min(ly);
        ly_max = ly_max.max(ly);
        rx_min = rx_min.min(rx);
        rx_max = rx_max.max(rx);
        ry_min = ry_min.min(ry);
        ry_max = ry_max.max(ry);
    }

    let n = reports.len() as u64;
    let left = ((lx_sum / n) as u16, (ly_sum / n) as u16);
    let right = ((rx_sum / n) as u16, (ry_sum / n) as u16);

    let lx_spread = lx_max - lx_min;
    let ly_spread = ly_max - ly_min;
    let rx_spread = rx_max - rx_min;
    let ry_spread = ry_max - ry_min;

    debug!(
        "[CAL] Auto-calibrate from {n} samples: \
         left=({},{}) spread=({lx_spread},{ly_spread}), \
         right=({},{}) spread=({rx_spread},{ry_spread})",
        left.0, left.1, right.0, right.1,
    );

    if lx_spread > 100 || ly_spread > 100 || rx_spread > 100 || ry_spread > 100 {
        warn!(
            "[CAL] High stick variance during calibration (spreads: L=({lx_spread},{ly_spread}) \
             R=({rx_spread},{ry_spread})) — sticks may have been touched"
        );
    }

    (left, right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deadzone() {
        let cal = StickCalibrator::new(MAIN_STICK_CAL, 10.0);
        // Small input well within deadzone
        assert_eq!(cal.calibrate(1.0, 1.0), (0.0, 0.0));
        assert_eq!(cal.calibrate(0.0, 0.0), (0.0, 0.0));
        assert_eq!(cal.calibrate(-5.0, 5.0), (0.0, 0.0));
    }

    #[test]
    fn test_center_returns_zero() {
        let cal = StickCalibrator::new(MAIN_STICK_CAL, 10.0);
        let (x, y) = cal.calibrate(0.0, 0.0);
        assert_eq!(x, 0.0);
        assert_eq!(y, 0.0);
    }

    #[test]
    fn test_full_tilt_positive_x() {
        let cal = StickCalibrator::new(MAIN_STICK_CAL, 10.0);
        // Full tilt right: ~2048 raw centered
        let (x, y) = cal.calibrate(2048.0, 0.0);
        // Should produce a large positive X, near-zero Y
        assert!(x > 50.0, "Expected large positive X, got {x}");
        assert!(y.abs() < 1.0, "Expected near-zero Y, got {y}");
    }

    #[test]
    fn test_opposite_directions() {
        let cal = StickCalibrator::new(MAIN_STICK_CAL, 10.0);
        let (x1, _y1) = cal.calibrate(1000.0, 0.0);
        let (x2, _y2) = cal.calibrate(-1000.0, 0.0);
        // Opposite directions should produce opposite signs
        assert!(x1 > 0.0, "Right tilt should be positive: {x1}");
        assert!(x2 < 0.0, "Left tilt should be negative: {x2}");
        // Magnitudes should be in the same ballpark (within 15% of each other)
        // since real calibration radii aren't perfectly symmetric
        let ratio = x1.abs() / x2.abs();
        assert!(
            ratio > 0.8 && ratio < 1.2,
            "Magnitude ratio {ratio} too far from 1.0"
        );
    }

    #[test]
    fn test_calibrator_from_string() {
        // Verify that both calibration strings parse correctly (32 values)
        let main_cal = StickCalibrator::new(MAIN_STICK_CAL, 10.0);
        let c_cal = StickCalibrator::new(C_STICK_CAL, 10.0);

        // All radii should be positive (real calibration data)
        for r in &main_cal.radii {
            assert!(*r > 0.0, "Main stick radius should be positive: {r}");
        }
        for r in &c_cal.radii {
            assert!(*r > 0.0, "C stick radius should be positive: {r}");
        }
    }

    #[test]
    fn test_auto_calibrate_centers_empty() {
        let (left, right) = auto_calibrate_centers(&[]);
        assert_eq!(left, (2048, 2048));
        assert_eq!(right, (2048, 2048));
    }

    #[test]
    fn test_auto_calibrate_centers_known_data() {
        // Create reports with known stick values
        // Left stick at (0x800, 0x800) = (2048, 2048)
        // Unpacking: a = data[0] | (data[1] & 0x0F) << 8
        //            b = (data[1] >> 4) | data[2] << 4
        // a=0x800: data[0]=0x00, data[1] low nibble=0x8 → data[1]=0x08
        // b=0x800: data[1] high nibble=0x0, data[2]=0x80
        let mut r1 = [0u8; 64];
        r1[6] = 0x00;
        r1[7] = 0x08;
        r1[8] = 0x80;
        // Right stick also at center
        r1[9] = 0x00;
        r1[10] = 0x08;
        r1[11] = 0x80;

        let reports = [r1, r1, r1]; // 3 identical reports
        let (left, right) = auto_calibrate_centers(&reports);
        assert_eq!(left, (0x800, 0x800));
        assert_eq!(right, (0x800, 0x800));
    }

    #[test]
    fn test_auto_calibrate_averages() {
        // Two reports with different stick values, check averaging
        let mut r1 = [0u8; 64];
        let mut r2 = [0u8; 64];

        // r1: left stick X=100, Y=200
        // 100 = 0x064: lo8=0x64, hi4=0x0
        // 200 = 0x0C8: lo4=0x0C, hi8=0x0C (wait, let me compute properly)
        // unpack: a = data[0] | (data[1] & 0x0F) << 8
        //         b = (data[1] >> 4) | data[2] << 4
        // To pack X=100 (0x64), Y=200 (0xC8):
        // data[0] = X & 0xFF = 0x64
        // data[1] = ((X >> 8) & 0x0F) | ((Y & 0x0F) << 4) = 0x00 | 0x80 = 0x80
        // data[2] = (Y >> 4) & 0xFF = 0x0C
        r1[6] = 0x64;
        r1[7] = 0x80;
        r1[8] = 0x0C;

        // r2: left stick X=200, Y=100
        r2[6] = 0xC8;
        r2[7] = 0x40;
        r2[8] = 0x06;

        let reports = [r1, r2];
        let (left, _) = auto_calibrate_centers(&reports);
        // Average: X=(100+200)/2=150, Y=(200+100)/2=150
        assert_eq!(left.0, 150);
        assert_eq!(left.1, 150);
    }

    #[test]
    fn test_stick_pair_center_returns_zero() {
        let pair = StickPair::new(
            StickCalibrator::new(MAIN_STICK_CAL, 10.0),
            StickCalibrator::new(C_STICK_CAL, 10.0),
            (2048, 2048),
            (2048, 2048),
        );
        let (left, right) = pair.calibrate((2048, 2048), (2048, 2048));
        assert_eq!(left, (0.0, 0.0));
        assert_eq!(right, (0.0, 0.0));
    }

    #[test]
    fn test_stick_pair_full_tilt() {
        let pair = StickPair::new(
            StickCalibrator::new(MAIN_STICK_CAL, 10.0),
            StickCalibrator::new(C_STICK_CAL, 10.0),
            (2048, 2048),
            (2048, 2048),
        );
        // Full tilt right on left stick
        let (left, _) = pair.calibrate((4095, 2048), (2048, 2048));
        assert!(left.0 > 50.0, "Expected large positive X, got {}", left.0);
        assert!(left.1.abs() < 1.0, "Expected near-zero Y, got {}", left.1);
    }

    #[test]
    fn test_stick_pair_set_deadzone() {
        let mut pair = StickPair::new(
            StickCalibrator::new(MAIN_STICK_CAL, 10.0),
            StickCalibrator::new(C_STICK_CAL, 10.0),
            (2048, 2048),
            (2048, 2048),
        );
        // Small movement with default deadzone (10) → zero
        let (left, _) = pair.calibrate((2058, 2048), (2048, 2048));
        assert_eq!(left, (0.0, 0.0));

        // Lower deadzone → same input now registers
        pair.set_deadzone(1.0);
        let (left, _) = pair.calibrate((2058, 2048), (2048, 2048));
        assert!(
            left.0 > 0.0,
            "Expected non-zero with low deadzone, got {}",
            left.0
        );
    }

    #[test]
    fn test_stick_pair_output_clamped() {
        let pair = StickPair::new(
            StickCalibrator::new(MAIN_STICK_CAL, 10.0),
            StickCalibrator::new(C_STICK_CAL, 10.0),
            (0, 0),
            (0, 0),
        );
        // Extreme raw values with center at 0 → output should be clamped to [-100, 100]
        let (left, right) = pair.calibrate((4095, 4095), (4095, 4095));
        assert!(left.0 <= 100.0 && left.0 >= -100.0);
        assert!(left.1 <= 100.0 && left.1 >= -100.0);
        assert!(right.0 <= 100.0 && right.0 >= -100.0);
        assert!(right.1 <= 100.0 && right.1 >= -100.0);
    }
}
