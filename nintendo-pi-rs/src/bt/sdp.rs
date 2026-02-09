//! SDP service registration and Bluetooth adapter configuration.
//!
//! Uses zbus to talk to BlueZ over D-Bus:
//! - Set adapter class to 0x002508 (gamepad)
//! - Set adapter alias to "Pro Controller"
//! - Register HID SDP profile
//! - Set the adapter to be discoverable and pairable

use tracing::{info, warn};
use zbus::Connection;
use zbus::names::InterfaceName;

/// HID SDP service record XML for a Pro Controller.
/// This tells the Switch that we are a Bluetooth HID gamepad.
const SDP_RECORD: &str = r#"<?xml version="1.0" encoding="UTF-8" ?>
<record>
    <attribute id="0x0001">
        <sequence>
            <uuid value="0x1124"/>
        </sequence>
    </attribute>
    <attribute id="0x0004">
        <sequence>
            <sequence>
                <uuid value="0x0100"/>
                <uint16 value="0x0011"/>
            </sequence>
            <sequence>
                <uuid value="0x0011"/>
            </sequence>
        </sequence>
    </attribute>
    <attribute id="0x0005">
        <sequence>
            <uuid value="0x1002"/>
        </sequence>
    </attribute>
    <attribute id="0x0006">
        <sequence>
            <uint16 value="0x656E"/>
            <uint16 value="0x006A"/>
            <uint16 value="0x0100"/>
        </sequence>
    </attribute>
    <attribute id="0x0009">
        <sequence>
            <sequence>
                <uuid value="0x1124"/>
                <uint16 value="0x0100"/>
            </sequence>
        </sequence>
    </attribute>
    <attribute id="0x000D">
        <sequence>
            <sequence>
                <sequence>
                    <uuid value="0x0100"/>
                    <uint16 value="0x0013"/>
                </sequence>
                <sequence>
                    <uuid value="0x0011"/>
                </sequence>
            </sequence>
        </sequence>
    </attribute>
    <attribute id="0x0100">
        <text value="Wireless Gamepad"/>
    </attribute>
    <attribute id="0x0101">
        <text value="Gamepad"/>
    </attribute>
    <attribute id="0x0102">
        <text value="Nintendo"/>
    </attribute>
    <attribute id="0x0200">
        <uint16 value="0x0100"/>
    </attribute>
    <attribute id="0x0201">
        <uint16 value="0x0111"/>
    </attribute>
    <attribute id="0x0202">
        <uint8 value="0x08"/>
    </attribute>
    <attribute id="0x0203">
        <uint8 value="0x00"/>
    </attribute>
    <attribute id="0x0204">
        <boolean value="true"/>
    </attribute>
    <attribute id="0x0205">
        <boolean value="true"/>
    </attribute>
    <attribute id="0x0206">
        <sequence>
            <sequence>
                <uint8 value="0x22"/>
                <text encoding="hex" value="05010905a1010601ff852109217508953081028530093075089530810285310931750896690181028532093275089669018102853309337508966901810285340934750896690181028535093575089530810285390939750895308102853a093a7508953081020501093009310933093426ff00463fff00750895048102750895018101c0"/>
            </sequence>
        </sequence>
    </attribute>
    <attribute id="0x0207">
        <sequence>
            <sequence>
                <uint16 value="0x0409"/>
                <uint16 value="0x0100"/>
            </sequence>
        </sequence>
    </attribute>
    <attribute id="0x020B">
        <uint16 value="0x0100"/>
    </attribute>
    <attribute id="0x020C">
        <uint16 value="0x0C80"/>
    </attribute>
    <attribute id="0x020D">
        <boolean value="true"/>
    </attribute>
    <attribute id="0x020E">
        <boolean value="true"/>
    </attribute>
</record>"#;

/// Configure the Bluetooth adapter for Pro Controller emulation.
pub async fn configure_adapter(connection: &Connection) -> anyhow::Result<()> {
    info!("[BT] Configuring Bluetooth adapter...");

    // Set adapter alias to "Pro Controller"
    let proxy = zbus::fdo::PropertiesProxy::builder(connection)
        .destination("org.bluez")?
        .path("/org/bluez/hci0")?
        .build()
        .await?;

    let adapter_iface = InterfaceName::from_static_str_unchecked("org.bluez.Adapter1");

    // Set alias
    proxy
        .set(adapter_iface.clone(), "Alias", &zbus::zvariant::Value::from("Pro Controller"))
        .await?;
    info!("[BT] Adapter alias set to 'Pro Controller'");

    // Set discoverable
    proxy
        .set(adapter_iface.clone(), "Discoverable", &zbus::zvariant::Value::from(true))
        .await?;

    // Set pairable
    proxy
        .set(adapter_iface.clone(), "Pairable", &zbus::zvariant::Value::from(true))
        .await?;

    // Set powered
    proxy
        .set(adapter_iface.clone(), "Powered", &zbus::zvariant::Value::from(true))
        .await?;

    // Set device class to 0x002508 (gamepad) via bluetoothctl or hciconfig
    // BlueZ doesn't expose class setting via D-Bus directly, use hciconfig
    let _ = tokio::process::Command::new("hciconfig")
        .args(["hci0", "class", "0x002508"])
        .output()
        .await;

    // Set discoverable timeout to 0 (forever)
    proxy
        .set(
            adapter_iface.clone(),
            "DiscoverableTimeout",
            &zbus::zvariant::Value::from(0u32),
        )
        .await?;

    // Set pairable timeout to 0 (forever)
    proxy
        .set(
            adapter_iface,
            "PairableTimeout",
            &zbus::zvariant::Value::from(0u32),
        )
        .await?;

    info!("[BT] Adapter configured: discoverable, pairable, class 0x002508");
    Ok(())
}

/// Register the HID SDP profile with BlueZ.
pub async fn register_sdp_profile(connection: &Connection) -> anyhow::Result<()> {
    info!("[BT] Registering HID SDP profile...");

    let proxy = zbus::Proxy::new(
        connection,
        "org.bluez",
        "/org/bluez",
        "org.bluez.ProfileManager1",
    )
    .await?;

    let mut options = std::collections::HashMap::new();
    options.insert("Role", zbus::zvariant::Value::from("server"));
    options.insert("RequireAuthentication", zbus::zvariant::Value::from(false));
    options.insert("RequireAuthorization", zbus::zvariant::Value::from(false));
    options.insert("AutoConnect", zbus::zvariant::Value::from(true));
    options.insert("ServiceRecord", zbus::zvariant::Value::from(SDP_RECORD));

    let obj_path = zbus::zvariant::ObjectPath::from_static_str_unchecked("/org/bluez/nintendo_pi");
    let uuid = "00001124-0000-1000-8000-00805f9b34fb";

    let result: Result<(), zbus::Error> = proxy
        .call("RegisterProfile", &(obj_path, uuid, options))
        .await;

    match result {
        Ok(_) => info!("[BT] SDP profile registered successfully"),
        Err(e) => {
            // "Already Exists" is OK if we're restarting
            let msg = e.to_string();
            if msg.contains("Already Exists") || msg.contains("AlreadyExists") {
                warn!("[BT] SDP profile already registered (OK on restart)");
            } else {
                return Err(e.into());
            }
        }
    }

    Ok(())
}
