//! Supervisión del sidecar (docs/01-arquitectura.md §4.4) contra un sidecar
//! falso en `/bin/sh`. Lo importante: NUNCA quedan zombis ni huérfanos.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use acta_core::PortError;
use acta_mac_bridge::client::{SidecarClient, SidecarMessage};
use acta_mac_bridge::protocol::{Command, Event, PermissionsStatusResult};
use pretty_assertions::assert_eq;
use std::time::Duration;
use support::*;

#[tokio::test]
async fn start_does_hello_and_exposes_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let client = start(dir.path(), "").await;
    let hello = client.hello_info().expect("hello cached");
    assert_eq!(hello.protocol_version, 1);
    assert_eq!(hello.capabilities, vec!["process_tap".to_string()]);
    let reqs = requests(dir.path());
    assert_eq!(reqs[0]["cmd"], "hello");
    assert_eq!(reqs[0]["args"]["protocol_version"], 1);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn call_returns_typed_result() {
    let dir = tempfile::tempdir().unwrap();
    let cases = format!(
        r#"    permissions.status) {};;"#,
        ok(r#"{"permissions":[{"kind":"microphone","status":"granted","target":null}]}"#)
    );
    let client = start(dir.path(), &cases).await;
    let res: PermissionsStatusResult = client
        .call_typed(Command::PermissionsStatus, None)
        .await
        .unwrap();
    assert_eq!(res.permissions.len(), 1);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn error_codes_map_to_port_errors() {
    let dir = tempfile::tempdir().unwrap();
    let cases = format!(
        "    capture.stop) {};;\n    detect.stop) {};;",
        err("permission_denied", "tcc"),
        err("cancelled", "x")
    );
    let client = start(dir.path(), &cases).await;
    let e = client.call(Command::CaptureStop, None).await.unwrap_err();
    assert!(matches!(e, PortError::PermissionDenied(m) if m == "tcc"));
    let e = client.call(Command::DetectStop, None).await.unwrap_err();
    assert_eq!(e, PortError::Cancelled);
    // Comando que el falso no conoce -> unsupported.
    let e = client.call(Command::ScreenStop, None).await.unwrap_err();
    assert!(matches!(e, PortError::Unavailable(_)), "{e:?}");
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn responses_out_of_order_are_matched_by_id() {
    let dir = tempfile::tempdir().unwrap();
    // `capture.pause` responde tarde (en segundo plano); `capture.resume` al momento.
    let cases = format!(
        "    capture.pause) ( sleep 0.4; {} ) & ;;\n    capture.resume) {};;",
        ok(r#"{"which":"pause"}"#),
        ok(r#"{"which":"resume"}"#)
    );
    let client = start(dir.path(), &cases).await;
    let (a, b) = tokio::join!(
        client.call(Command::CapturePause, None),
        client.call(Command::CaptureResume, None)
    );
    assert_eq!(a.unwrap()["which"], "pause");
    assert_eq!(b.unwrap()["which"], "resume");
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn events_are_broadcast_and_garbage_lines_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let cases = format!(
        "    detect.stop) echo 'not json'; head -c 1100000 /dev/zero | tr '\\0' a; echo; {}; {};;",
        event("log", r#"{"level":"info","message":"hi"}"#),
        ok("{}")
    );
    let client = start(dir.path(), &cases).await;
    let mut rx = client.subscribe();
    client.call(Command::DetectStop, None).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    match msg {
        SidecarMessage::Event(env) => {
            assert_eq!(env.ts, 1_700_000_000_000);
            assert!(matches!(env.event, Event::Log(l) if l.message == "hi"));
        }
        other => panic!("unexpected {other:?}"),
    }
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn unanswered_command_times_out() {
    let dir = tempfile::tempdir().unwrap();
    let client = start(dir.path(), "    capture.stop) : ;;").await;
    let e = client
        .call(Command::CaptureStop, Some(Duration::from_millis(150)))
        .await
        .unwrap_err();
    assert!(matches!(e, PortError::Timeout(_)), "{e:?}");
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn protocol_mismatch_aborts_start_and_reaps_child() {
    let dir = tempfile::tempdir().unwrap();
    let hello = format!(
        "{};;",
        ok(r#"{"sidecar_version":"x","protocol_version":2,"macos_version":"26","capabilities":[]}"#)
    );
    let bin = fake_sidecar_with_hello(dir.path(), &hello, "");
    let e = SidecarClient::start(config(bin)).await.expect_err("must fail");
    assert!(matches!(e, PortError::Unavailable(_)), "{e:?}");
    let pid: u32 = std::fs::read_to_string(dir.path().join("spawns"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(!pid_exists(pid).await, "child must be reaped (no zombie)");
}

#[tokio::test]
async fn missing_binary_is_unavailable() {
    let cfg = acta_mac_bridge::client::SidecarConfig {
        binary: "/nonexistent/acta-mac".into(),
        ..config("unused".into())
    };
    let e = SidecarClient::start(cfg)
        .await
        .expect_err("must fail");
    assert!(matches!(e, PortError::Unavailable(_)), "{e:?}");
}

#[tokio::test]
async fn shutdown_reaps_child() {
    let dir = tempfile::tempdir().unwrap();
    let client = start(dir.path(), "").await;
    let pid = client.pid().expect("pid");
    assert!(pid_exists(pid).await);
    client.shutdown().await.unwrap();
    assert!(!pid_exists(pid).await, "no zombie after shutdown");
    // Tras cerrar, las llamadas fallan sin colgarse.
    let e = client.call(Command::CaptureStop, None).await.unwrap_err();
    assert!(matches!(e, PortError::Unavailable(_)), "{e:?}");
}

#[tokio::test]
async fn shutdown_kills_a_sidecar_that_ignores_it() {
    let dir = tempfile::tempdir().unwrap();
    // Este falso ignora `shutdown` y el EOF: se queda colgado durmiendo.
    let hello = format!("{};;", ok(HELLO_OK));
    let cases = "    shutdown) trap '' TERM; sleep 30;;";
    let bin = fake_sidecar_with_hello(dir.path(), &hello, cases);
    let client = SidecarClient::start(config(bin)).await.unwrap();
    let pid = client.pid().unwrap();
    let t0 = std::time::Instant::now();
    client.shutdown().await.unwrap();
    assert!(t0.elapsed() < Duration::from_secs(3));
    assert!(!pid_exists(pid).await, "killed + waited");
}

#[tokio::test]
async fn dropping_every_handle_reaps_child() {
    let dir = tempfile::tempdir().unwrap();
    let client = start(dir.path(), "").await;
    let pid = client.pid().unwrap();
    drop(client);
    let mut gone = false;
    for _ in 0..100 {
        if !pid_exists(pid).await {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert!(gone, "child must exit and be reaped when the client is dropped");
}

#[tokio::test]
async fn crashed_sidecar_is_restarted_and_hello_repeated() {
    let dir = tempfile::tempdir().unwrap();
    let cases = format!("    capture.resume) {};;\n    capture.pause) exit 3;;", ok("{}"));
    let client = start(dir.path(), &cases).await;
    let first = client.pid().unwrap();
    let mut rx = client.subscribe();
    let e = client.call(Command::CapturePause, None).await.unwrap_err();
    assert!(matches!(e, PortError::Unavailable(_)), "{e:?}");
    let msg = loop {
        match tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap() {
            SidecarMessage::Event(_) => continue,
            other => break other,
        }
    };
    assert!(matches!(msg, SidecarMessage::Restarted { restarts: 1, .. }), "{msg:?}");
    assert_eq!(spawns(dir.path()), 2);
    assert!(!pid_exists(first).await, "dead child reaped");
    client.call(Command::CaptureResume, None).await.unwrap();
    let hellos = requests(dir.path()).iter().filter(|r| r["cmd"] == "hello").count();
    assert_eq!(hellos, 2);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn restart_budget_is_bounded_then_gives_up() {
    let dir = tempfile::tempdir().unwrap();
    // Responde a hello y muere al poco, siempre.
    let hello = format!("{}; sleep 0.05; exit 1;;", ok(HELLO_OK));
    let bin = fake_sidecar_with_hello(dir.path(), &hello, "");
    let mut cfg = config(bin);
    cfg.max_restarts = 2;
    let client = SidecarClient::start(cfg).await.unwrap();
    let mut rx = client.subscribe();
    let gave_up = loop {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap() {
            SidecarMessage::GaveUp { .. } => break true,
            _ => continue,
        }
    };
    assert!(gave_up);
    // 1 arranque inicial + 2 reinicios; nunca un bucle infinito.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(spawns(dir.path()), 3);
    let e = client.call(Command::CaptureStop, None).await.unwrap_err();
    assert!(matches!(e, PortError::Unavailable(_)), "{e:?}");
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn stderr_goes_to_log_file() {
    let dir = tempfile::tempdir().unwrap();
    let cases = format!("    detect.stop) echo 'sidecar says hi' >&2; {};;", ok("{}"));
    let mut cfg = config(fake_sidecar(dir.path(), &cases));
    let log = dir.path().join("logs").join("acta-mac.log");
    cfg.log_file = Some(log.clone());
    let client = SidecarClient::start(cfg).await.unwrap();
    client.call(Command::DetectStop, None).await.unwrap();
    let found = eventually(2000, || {
        std::fs::read_to_string(&log)
            .map(|s| s.contains("sidecar says hi"))
            .unwrap_or(false)
    })
    .await;
    assert!(found);
    client.shutdown().await.unwrap();
}

#[test]
fn meeting_title_and_calendar_commands_use_the_wire_names() {
    use acta_mac_bridge::protocol::*;
    let v = serde_json::to_value(Request { id: 1, command: Command::MeetingTitles(MeetingTitlesArgs { pid: 42, bundle_id: "com.microsoft.teams2".into() }) }).unwrap();
    assert_eq!(v, serde_json::json!({"id": 1, "cmd": "meeting.titles", "args": {"pid": 42, "bundle_id": "com.microsoft.teams2"}}));
    let v = serde_json::to_value(Request { id: 2, command: Command::CalendarEvents(CalendarEventsArgs { from_unix_ms: 1, to_unix_ms: 2, request_access: true }) }).unwrap();
    assert_eq!(v["cmd"], "calendar.events");
    assert_eq!(v["args"]["request_access"], true);
    let r: CalendarEventsResult = serde_json::from_value(serde_json::json!({"access": "granted", "events": [{"title": "Daily", "start_unix_ms": 1, "end_unix_ms": 2, "all_day": false}]})).unwrap();
    assert_eq!(r.events[0].title, "Daily");
}

#[test]
fn system_profile_and_installed_apps_commands_use_the_wire_names() {
    use acta_mac_bridge::protocol::*;
    let v = serde_json::to_value(Request { id: 1, command: Command::SystemProfile }).unwrap();
    assert_eq!(v, serde_json::json!({"id": 1, "cmd": "system.profile"}));
    let r: SystemProfileResult = serde_json::from_value(serde_json::json!({"full_name": "Ana García", "preferred_languages": ["es-ES", "en-GB"]})).unwrap();
    assert_eq!((r.full_name.as_deref(), r.preferred_languages.len()), (Some("Ana García"), 2));
    // Sin nombre (no se pudo leer): la clave puede faltar.
    let r: SystemProfileResult = serde_json::from_value(serde_json::json!({"preferred_languages": []})).unwrap();
    assert_eq!(r.full_name, None);

    let v = serde_json::to_value(Request { id: 2, command: Command::AppsInstalled(AppsInstalledArgs { icons_dir: "/data/cache/app-icons".into(), icon_px: 128 }) }).unwrap();
    assert_eq!(v, serde_json::json!({"id": 2, "cmd": "apps.installed", "args": {"icons_dir": "/data/cache/app-icons", "icon_px": 128}}));
    let r: AppsInstalledResult = serde_json::from_value(serde_json::json!({"apps": [
        {"bundle_id": "com.apple.Safari", "name": "Safari", "path": "/Applications/Safari.app", "running": true, "icon_path": "/data/cache/app-icons/com.apple.Safari.png"},
        {"bundle_id": "com.rara", "name": "Rara", "path": "/Applications/Rara.app", "running": false}
    ]})).unwrap();
    assert_eq!(r.apps.len(), 2);
    assert_eq!((r.apps[0].running, r.apps[1].icon_path.as_deref()), (true, None));
}
