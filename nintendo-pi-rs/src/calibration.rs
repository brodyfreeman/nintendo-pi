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
}
