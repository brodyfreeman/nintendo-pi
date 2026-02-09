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
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ],
        // Controller color (Pro Controller gray)
        (0x6050, 0x0D) => vec![
            0x32, 0x32, 0x32, // body color (dark gray)
            0xFF, 0xFF, 0xFF, // button color (white)
            0x32, 0x32, 0x32, // left grip
            0xFF, 0xFF, 0xFF, // right grip
            0x03, // ??? (Pro Controller flag?)
        ],
        // Factory stick calibration
        (0x603D, 0x12) => {
            // Left stick cal (9 bytes) + Right stick cal (9 bytes)
            vec![
                // Left stick: center_x, center_y, x_min, y_min, x_max, y_max (packed 12-bit)
                0x00, 0x07, 0x70, 0x00, 0x08, 0x80, 0x00, 0x07, 0x70,
                // Right stick
                0x00, 0x07, 0x70, 0x00, 0x08, 0x80, 0x00, 0x07, 0x70,
            ]
        }
        // User stick calibration
        (0x8010, 0x16) => {
            // Magic bytes indicating no user calibration
            let mut data = vec![0xFF; 0x16];
            data[0] = 0xFF;
            data[1] = 0xFF;
            data
        }
        // Stick parameters (deadzone, range ratio)
        (0x6086, 0x12) => vec![
            0x0F, 0x30, 0x61, 0x96, 0x30, 0xF3,
            0xD4, 0x14, 0x54, 0x41, 0x15, 0x54,
            0xC7, 0x79, 0x9C, 0x33, 0x36, 0x63,
        ],
        // IMU factory calibration
        (0x6020, 0x18) => vec![
            0xBE, 0xFF, 0x3E, 0x00, 0xF0, 0x01,
            0x00, 0x40, 0x00, 0x40, 0x00, 0x40,
            0xFE, 0xFF, 0xFE, 0xFF, 0x08, 0x00,
            0xE7, 0x3B, 0xE7, 0x3B, 0xE7, 0x3B,
        ],
        // IMU user calibration
        (0x8026, 0x1A) => {
            let mut data = vec![0xFF; 0x1A];
            data[0] = 0xFF;
            data[1] = 0xFF;
            data
        }
        // Factory sensor/stick device parameters
        (0x6080, 0x06) => vec![0x50, 0xFD, 0x00, 0x00, 0xC6, 0x0F],
        // Catch-all: return zeros
        _ => vec![0x00; len as usize],
    }
}

/// Build a subcommand reply (0x21 report).
///
/// Format:
///   [0] = 0x21
///   [1] = timer
///   [2..4] = button state (zeros for reply)
///   [5..8] = left stick (center)
///   [8..11] = right stick (center)
///   [11] = vibrator
///   [12] = ACK byte
///   [13] = subcommand ID being replied to
///   [14..] = subcommand-specific data
pub fn build_subcommand_reply(timer: u8, subcmd: u8, ack: u8, data: &[u8]) -> Vec<u8> {
    let mut reply = vec![0u8; 50];
    reply[0] = 0x21;
    reply[1] = timer;

    // Buttons at neutral (zeros)
    // Left stick at center
    reply[5] = 0x00;
    reply[6] = 0x08;
    reply[7] = 0x80;
    // Right stick at center
    reply[8] = 0x00;
    reply[9] = 0x08;
    reply[10] = 0x80;

    reply[12] = ack;
    reply[13] = subcmd;

    // Copy subcommand data
    let copy_len = data.len().min(reply.len() - 14);
    reply[14..14 + copy_len].copy_from_slice(&data[..copy_len]);

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
                0x03, 0x48, // FW version
                0x03, // Pro Controller
                0x02, // Unknown
                // MAC address (fake)
                0x98, 0xB6, 0xE9, 0x46, 0x50, 0x6A,
                0x01, // Unknown
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
            if subcmd_data.len() >= 5 {
                let addr = u32::from_le_bytes([
                    subcmd_data[0],
                    subcmd_data[1],
                    subcmd_data[2],
                    subcmd_data[3],
                ]);
                let length = subcmd_data[4];

                let mut reply_data = vec![
                    subcmd_data[0],
                    subcmd_data[1],
                    subcmd_data[2],
                    subcmd_data[3],
                    length,
                ];
                reply_data.extend_from_slice(&spi_read_response(addr, length));
                (0x90, reply_data)
            } else {
                (0x80, vec![])
            }
        }

        // 0x21: Set NFC/IR MCU configuration
        0x21 => (0xA0, vec![0x01, 0x00, 0xFF, 0x00, 0x03, 0x00, 0x05, 0x01]),

        // 0x30: Set player lights
        0x30 => (0x80, vec![]),

        // 0x38: Set HOME light
        0x38 => (0x80, vec![]),

        // 0x40: Enable IMU
        0x40 => (0x80, vec![]),

        // 0x41: Set IMU sensitivity
        0x41 => (0x80, vec![]),

        // 0x48: Enable vibration
        0x48 => (0x80, vec![]),

        // Unknown subcommand: generic ACK
        _ => {
            tracing::debug!("[BT] Unknown subcommand: 0x{:02X}", subcmd_id);
            (0x80, vec![])
        }
    }
}
