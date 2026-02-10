//! Pro Controller BT protocol: SPI read responses, subcommand replies,
//! and 0x30 input report building.
//!
//! All constant data is derived from NXBT/joycontrol sources.

/// SPI flash read response data.
/// Maps (address, length) to pre-built response bytes.
pub fn spi_read_response(addr: u32, len: u8) -> Vec<u8> {
    match (addr, len) {
        // Serial number
        (0x6000, 0x10) => vec![
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
        // Controller color (matches NXBT defaults)
        (0x6050, 0x0D) => vec![
            0x82, 0x82, 0x82, // body color
            0x0F, 0x0F, 0x0F, // button color
            0xFF, 0xFF, 0xFF, // left grip
            0xFF, 0xFF, 0xFF, // right grip
            0xFF, // spacer
        ],
        // Factory stick calibration (matches NXBT)
        (0x603D, 0x12) => {
            // Left stick cal (9 bytes) + Right stick cal (9 bytes)
            vec![
                // Left stick: above_center, center, below_center
                0xBA, 0xF5, 0x62, 0x6F, 0xC8, 0x77, 0xED, 0x95, 0x5B, // Right stick
                0x16, 0xD8, 0x7D, 0xF2, 0xB5, 0x5F, 0x86, 0x65, 0x5E,
            ]
        }
        // User stick calibration (all 0xFF = no user calibration)
        (0x8010, 0x16) => vec![0xFF; 0x16],
        // Stick parameters (deadzone, range ratio)
        (0x6086, 0x12) => vec![
            0x0F, 0x30, 0x61, 0x96, 0x30, 0xF3, 0xD4, 0x14, 0x54, 0x41, 0x15, 0x54, 0xC7, 0x79,
            0x9C, 0x33, 0x36, 0x63,
        ],
        // IMU factory calibration (matches NXBT)
        (0x6020, 0x18) => vec![
            0xD3, 0xFF, 0xD5, 0xFF, 0x55, 0x01, // Acceleration origin
            0x00, 0x40, 0x00, 0x40, 0x00, 0x40, // Acceleration sensitivity
            0x19, 0x00, 0xDD, 0xFF, 0xDC, 0xFF, // Gyro origin
            0x3B, 0x34, 0x3B, 0x34, 0x3B, 0x34, // Gyro sensitivity
        ],
        // IMU user calibration (all 0xFF = no user calibration)
        (0x8026, 0x1A) => vec![0xFF; 0x1A],
        // Factory sensor/stick device parameters + stick params 1
        (0x6080, 0x06) => vec![0x50, 0xFD, 0x00, 0x00, 0xC6, 0x0F],
        // Full 6080 read (sensor params + stick params)
        (0x6080, 0x18) => vec![
            0x50, 0xFD, 0x00, 0x00, 0xC6, 0x0F,
            // Stick parameters (deadzone, range ratio)
            0x0F, 0x30, 0x61, 0x96, 0x30, 0xF3, 0xD4, 0x14, 0x54, 0x41, 0x15, 0x54, 0xC7, 0x79,
            0x9C, 0x33, 0x36, 0x63,
        ],
        // Stick device parameters 2 (same as params 1, per NXBT)
        (0x6098, 0x12) => vec![
            0x0F, 0x30, 0x61, 0x96, 0x30, 0xF3, 0xD4, 0x14, 0x54, 0x41, 0x15, 0x54, 0xC7, 0x79,
            0x9C, 0x33, 0x36, 0x63,
        ],
        // Catch-all: return zeros
        _ => vec![0x00; len as usize],
    }
}

/// Build a subcommand reply (0x21 report).
pub fn build_subcommand_reply(timer: u8, subcmd: u8, ack: u8, data: &[u8]) -> [u8; 50] {
    let mut reply = [0u8; 50];
    reply[0] = 0xA1; // HID transaction header
    reply[1] = 0x21; // Subcommand reply report ID
    reply[2] = timer;
    reply[3] = 0x90; // Battery + connection info

    // Stick centers (0x800 packed as 12-bit); buttons at [4..6] are zero (neutral)
    reply[7..10].copy_from_slice(&[0x00, 0x08, 0x80]);
    reply[10..13].copy_from_slice(&[0x00, 0x08, 0x80]);
    reply[13] = 0xB0; // Vibrator byte

    reply[14] = ack;
    reply[15] = subcmd;

    let copy_len = data.len().min(reply.len() - 16);
    reply[16..16 + copy_len].copy_from_slice(&data[..copy_len]);

    reply
}

/// Handle a subcommand from the Switch and return the reply data.
///
/// `subcmd_data` is the full subcommand payload starting after the rumble data.
/// Returns (ack_byte, subcmd_id, reply_data).
pub fn handle_subcommand(subcmd_id: u8, subcmd_data: &[u8]) -> (u8, Vec<u8>) {
    match subcmd_id {
        // 0x02: Request device info
        0x02 => {
            let data = vec![
                0x03, 0x8B, // FW version (matches NXBT)
                0x03, // Pro Controller
                0x02, // Unknown
                // MAC address (fake)
                0x98, 0xB6, 0xE9, 0x46, 0x50, 0x6A, 0x01, // Unknown
                0x01, // Colors in SPI: yes
            ];
            (0x82, data)
        }

        // 0x03: Set input report mode
        0x03 => (0x80, vec![]),

        // 0x04: Trigger buttons elapsed time
        0x04 => (0x83, vec![]),

        // 0x08: Set shipment low power state
        0x08 => (0x80, vec![]),

        // 0x10: SPI flash read
        0x10 => {
            if let Some(header) = subcmd_data.get(..5) {
                let addr = u32::from_le_bytes(header[..4].try_into().unwrap());
                let length = header[4];
                let mut reply_data = header.to_vec();
                reply_data.extend_from_slice(&spi_read_response(addr, length));
                (0x90, reply_data)
            } else {
                (0x80, vec![])
            }
        }

        // 0x21: Set NFC/IR MCU configuration
        0x21 => (0xA0, vec![0x01, 0x00, 0xFF, 0x00, 0x08, 0x00, 0x1B, 0x01]),

        // 0x22: Set NFC/IR state
        0x22 => (0x80, vec![]),

        // 0x30: Set player lights
        0x30 => (0x80, vec![]),

        // 0x38: Set HOME light
        0x38 => (0x80, vec![]),

        // 0x40: Enable IMU
        0x40 => (0x80, vec![]),

        // 0x41: Set IMU sensitivity
        0x41 => (0x80, vec![]),

        // 0x48: Enable vibration
        0x48 => (0x82, vec![]),

        // Unknown subcommand: generic ACK
        _ => {
            tracing::debug!("[BT] Unknown subcommand: 0x{:02X}", subcmd_id);
            (0x80, vec![])
        }
    }
}
