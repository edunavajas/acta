#![allow(clippy::unwrap_used, clippy::expect_used)]
//! La configuración de Windows debe fusionarse sin claves de macOS y con el
//! scope del asset protocol apuntando a la raíz de datos real.
//!
//! Es el equivalente en Windows de `tests/wave5.rs`, que fija el scope de
//! `tauri.conf.json` en macOS. Aquí no se puede comprobar la sustitución de
//! `$DATA` en ejecución, así que se fija el contrato tal y como está escrito:
//! `$DATA` lo resuelve Tauri a `dirs::data_dir()`, que es la misma función que
//! usa `system::data_root` para el layout de datos.

use serde_json::Value;

const WINDOWS_CONF: &str = include_str!("../tauri.windows.conf.json");

/// Claves de ventana que solo existen en macOS y que no deben reaparecer.
const MACOS_ONLY: [&str; 6] = [
    "titleBarStyle",
    "trafficLightPosition",
    "windowEffects",
    "hiddenTitle",
    "acceptFirstMouse",
    "visibleOnAllWorkspaces",
];

fn conf() -> Value {
    serde_json::from_str(WINDOWS_CONF).expect("tauri.windows.conf.json no es JSON válido")
}

#[test]
fn the_asset_protocol_scope_covers_the_data_dir() {
    let conf = conf();
    let scope = conf["app"]["security"]["assetProtocol"]["scope"]
        .as_array()
        .expect("falta app.security.assetProtocol.scope");
    assert_eq!(scope.len(), 1, "el scope se reemplaza entero: debe declararse completo");
    assert_eq!(scope[0].as_str(), Some("$DATA/acta/**"));
}

#[test]
fn the_windows_are_redeclared_whole_and_without_macos_keys() {
    let conf = conf();
    let windows = conf["app"]["windows"].as_array().expect("falta app.windows");
    // El merge es RFC 7396 y los arrays se reemplazan enteros: si esto se
    // quedara vacío, Windows se quedaría sin ventana principal ni overlay.
    assert_eq!(windows.len(), 2, "se esperan la ventana principal y el overlay");
    let labels: Vec<&str> = windows.iter().filter_map(|w| w["label"].as_str()).collect();
    assert_eq!(labels, ["main", "overlay"]);
    for window in windows {
        for key in MACOS_ONLY {
            assert!(window.get(key).is_none(), "`{key}` es de macOS y no va en la config de Windows");
        }
    }
}

#[test]
fn the_bundle_declares_windows_targets() {
    let conf = conf();
    let targets: Vec<&str> = conf["bundle"]["targets"]
        .as_array()
        .expect("falta bundle.targets")
        .iter()
        .filter_map(|t| t.as_str())
        .collect();
    assert_eq!(targets, ["msi", "nsis"]);
}
