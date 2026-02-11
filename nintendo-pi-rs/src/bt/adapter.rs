//! Bluetooth adapter configuration for Pro Controller emulation.
//!
//! Configures the HCI adapter via D-Bus (alias, discoverable, pairable)
//! and sets the device class via hciconfig.

use tracing::info;
use zbus::names::InterfaceName;
use zbus::Connection;

/// Configure the Bluetooth adapter for Pro Controller emulation.
///
/// Note: device class is NOT set here — call `set_device_class()` after
/// all D-Bus operations (including SDP registration) because D-Bus calls
/// can reset the HCI device class.
pub async fn configure_adapter(connection: &Connection) -> anyhow::Result<()> {
    info!("[BT] Configuring Bluetooth adapter...");

    let proxy = zbus::fdo::PropertiesProxy::builder(connection)
        .destination("org.bluez")?
        .path("/org/bluez/hci0")?
        .build()
        .await?;

    let adapter_iface = InterfaceName::from_static_str_unchecked("org.bluez.Adapter1");

    // Set alias
    proxy
        .set(
            adapter_iface.clone(),
            "Alias",
            &zbus::zvariant::Value::from("Pro Controller"),
        )
        .await?;
    info!("[BT] Adapter alias set to 'Pro Controller'");

    // Set discoverable
    proxy
        .set(
            adapter_iface.clone(),
            "Discoverable",
            &zbus::zvariant::Value::from(true),
        )
        .await?;

    // Set pairable
    proxy
        .set(
            adapter_iface.clone(),
            "Pairable",
            &zbus::zvariant::Value::from(true),
        )
        .await?;

    // Set powered
    proxy
        .set(
            adapter_iface.clone(),
            "Powered",
            &zbus::zvariant::Value::from(true),
        )
        .await?;

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

    info!("[BT] Adapter configured: discoverable, pairable");
    Ok(())
}

/// Set the Bluetooth adapter name and device class.
///
/// Must be called AFTER all D-Bus property changes and SDP registration,
/// as those operations can reset the HCI device class and name.
/// The D-Bus `Alias` property only affects local display — `hciconfig name`
/// sets the actual name the Switch sees during BR/EDR inquiry.
pub async fn set_device_class() -> anyhow::Result<()> {
    // Let D-Bus operations settle before touching HCI settings
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Set the actual BT adapter name (what remote devices see during inquiry)
    let output = tokio::process::Command::new("hciconfig")
        .args(["hci0", "name", "Pro Controller"])
        .output()
        .await?;
    if !output.status.success() {
        anyhow::bail!(
            "[BT] Failed to set adapter name: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Set device class to gamepad — MUST be last, nothing after this
    let output = tokio::process::Command::new("hciconfig")
        .args(["hci0", "class", "0x002508"])
        .output()
        .await?;
    if !output.status.success() {
        anyhow::bail!(
            "[BT] Failed to set device class: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    info!("[BT] Adapter name='Pro Controller', class=0x002508 (gamepad)");
    Ok(())
}
