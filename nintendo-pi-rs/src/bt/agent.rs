//! BlueZ pairing agent — auto-accepts all pairing requests.
//!
//! Required for the Switch to pair with the Pi on first connection.
//! Without this, BlueZ rejects pairing and the Switch never connects.

use tracing::{info, warn};
use zbus::zvariant::ObjectPath;
use zbus::Connection;

/// BlueZ pairing agent — auto-accepts all pairing requests.
struct BtAgent;

#[zbus::interface(name = "org.bluez.Agent1")]
impl BtAgent {
    fn release(&self) {}

    fn request_confirmation(
        &self,
        _device: ObjectPath<'_>,
        _passkey: u32,
    ) -> zbus::fdo::Result<()> {
        Ok(())
    }

    fn request_authorization(&self, _device: ObjectPath<'_>) -> zbus::fdo::Result<()> {
        Ok(())
    }

    fn authorize_service(&self, _device: ObjectPath<'_>, _uuid: &str) -> zbus::fdo::Result<()> {
        Ok(())
    }

    fn cancel(&self) {}
}

/// Register a NoInputNoOutput pairing agent with BlueZ.
///
/// This auto-accepts all pairing requests, which is required for the Switch
/// to pair with the Pi on first connection. Without this, BlueZ rejects
/// pairing and the L2CAP connection never completes.
pub async fn register_agent(connection: &Connection) -> anyhow::Result<()> {
    info!("[BT] Registering pairing agent...");

    connection
        .object_server()
        .at("/org/bluez/nintendo_pi/agent", BtAgent)
        .await?;

    let proxy = zbus::Proxy::new(
        connection,
        "org.bluez",
        "/org/bluez",
        "org.bluez.AgentManager1",
    )
    .await?;

    let agent_path = ObjectPath::from_static_str_unchecked("/org/bluez/nintendo_pi/agent");

    let result: Result<(), zbus::Error> = proxy
        .call("RegisterAgent", &(&agent_path, "NoInputNoOutput"))
        .await;
    match result {
        Ok(_) => {}
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("Already Exists") || msg.contains("AlreadyExists") {
                warn!("[BT] Agent already registered (OK on restart)");
            } else {
                return Err(e.into());
            }
        }
    }

    let _: Result<(), zbus::Error> = proxy.call("RequestDefaultAgent", &(&agent_path,)).await;

    info!("[BT] Pairing agent registered (NoInputNoOutput)");
    Ok(())
}
