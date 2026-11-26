//! Contra el sidecar Swift REAL (solo comandos de lectura: sin captura ni
//! diálogos TCC). Necesita `swift build --disable-get-task-allow-entitlement`
//! en sidecars/acta-mac. Ejecutar: `cargo test -p acta-mac-bridge -- --ignored`.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use acta_mac_bridge::client::{SidecarClient, SidecarConfig, locate_sidecar};
use acta_mac_bridge::protocol::{AppsListResult, Command, DevicesListResult, ModelsStatusResult};
use std::time::Duration;

#[tokio::test]
#[ignore = "needs the built Swift sidecar"]
async fn real_sidecar_hello_inventory_and_clean_exit() {
    let binary = locate_sidecar(None).expect("build sidecars/acta-mac first");
    let client = SidecarClient::start(SidecarConfig {
        binary,
        hello_timeout: Duration::from_secs(30),
        ..SidecarConfig::default()
    })
    .await
    .unwrap();
    let hello = client.hello_info().unwrap();
    assert_eq!(hello.protocol_version, 1);
    assert!(hello.capabilities.iter().any(|c| c == "process_tap"));
    let devices: DevicesListResult = client.call_typed(Command::DevicesList, None).await.unwrap();
    assert!(!devices.outputs.is_empty());
    let _apps: AppsListResult = client.call_typed(Command::AppsList, None).await.unwrap();
    let models: ModelsStatusResult = client.call_typed(Command::ModelsStatus, None).await.unwrap();
    assert_eq!(models.models.len(), 3);
    let pid = client.pid().unwrap();
    client.shutdown().await.unwrap();
    assert!(!support::pid_exists(pid).await, "no zombie, no orphan");
}
