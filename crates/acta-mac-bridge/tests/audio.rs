//! `MacAudioSource` contra el sidecar falso.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use acta_core::ids::MeetingId;
use acta_core::recording::{
    AudioEvent, AudioFormat, AudioSource, CaptureRequest, MicTarget, SystemTarget, Track, TrackState,
};
use acta_core::time::UnixMs;
use acta_mac_bridge::audio::MacAudioSource;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use std::time::Duration;
use support::*;

/// `capture.start` responde con `started_at` = 1700000000000 + nº de arranque
/// y emite un frame [1, -2] (base64 "AQD+/w=="). `capture.retarget` mata al
/// sidecar (para probar el reinicio).
fn audio_cases() -> String {
    let frame = event(
        "audio.frame",
        r#"{"track":"system","seq":0,"pts_us":0,"sample_rate":16000,"channels":1,"format":"s16le","data":"AQD+/w=="}"#,
    );
    let changed = event(
        "audio.device_changed",
        r#"{"track":"mic","device_uid":"airpods","device_name":"AirPods Pro","reason":"default_input_changed"}"#,
    );
    let state = event("audio.track_state", r#"{"track":"system","state":"stalled","message":"no data"}"#);
    let overrun = event("audio.overrun", r#"{"track":"mic","dropped_frames":3}"#);
    format!(
        r#"    capture.start) n=$(wc -l < "$DIR/spawns" | tr -d ' ')
      printf '{{"type":"response","id":%s,"ok":true,"result":{{"started_at_unix_ms":170000000000%s,"mic_device":"MacBook Pro Microphone","system_pids":[100,101]}}}}\n' "$id" "$n"
      {frame};;
    capture.pause) {ok};;
    capture.resume) {ok};;
    capture.stop) {ok};;
    devices.list) {changed}; {state}; {overrun}; {ok};;
    capture.retarget) exit 9;;"#,
        ok = ok("{}")
    )
}

fn request() -> CaptureRequest {
    CaptureRequest {
        meeting_id: MeetingId::from_u128(7),
        format: AudioFormat::WORKING,
        frame_ms: 100,
        mic: MicTarget {
            enabled: true,
            device_uid: None,
        },
        system: SystemTarget {
            enabled: true,
            pids: vec![100],
            bundle_ids: vec!["com.microsoft.teams2".into()],
        },
    }
}

async fn next(stream: &mut acta_core::EventStream<AudioEvent>) -> AudioEvent {
    tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("event in time")
        .expect("stream open")
}

#[tokio::test]
async fn start_sends_args_and_decodes_frames() {
    let dir = tempfile::tempdir().unwrap();
    let client = start(dir.path(), &audio_cases()).await;
    let source = MacAudioSource::new(client.clone());
    let (started, mut stream) = source.start(request()).await.unwrap();
    assert_eq!(started.started_at, UnixMs(1_700_000_000_001));
    assert_eq!(started.mic_device.as_deref(), Some("MacBook Pro Microphone"));
    assert_eq!(started.system_pids, vec![100, 101]);
    match next(&mut stream).await {
        AudioEvent::Frame(f) => {
            assert_eq!(f.track, Track::System);
            assert_eq!(f.samples, vec![1, -2]);
            assert_eq!(f.sample_rate, 16_000);
        }
        other => panic!("{other:?}"),
    }
    let req = requests(dir.path())
        .into_iter()
        .find(|r| r["cmd"] == "capture.start")
        .unwrap();
    assert_eq!(req["args"]["session_id"], MeetingId::from_u128(7).to_string());
    assert_eq!(req["args"]["sample_rate"], 16000);
    assert_eq!(req["args"]["frame_ms"], 100);
    assert_eq!(req["args"]["mic"]["device_uid"], serde_json::Value::Null);
    assert_eq!(req["args"]["system"]["pids"], serde_json::json!([100]));
    source.stop().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), stream.next()).await.unwrap().is_none(),
        "stream ends after stop()"
    );
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn maps_device_state_and_overrun_events() {
    let dir = tempfile::tempdir().unwrap();
    let client = start(dir.path(), &audio_cases()).await;
    let source = MacAudioSource::new(client.clone());
    let (_, mut stream) = source.start(request()).await.unwrap();
    let _frame = next(&mut stream).await;
    client
        .call(acta_mac_bridge::protocol::Command::DevicesList, None)
        .await
        .unwrap();
    assert_eq!(
        next(&mut stream).await,
        AudioEvent::DeviceChanged {
            track: Track::Mic,
            device_name: Some("AirPods Pro".into()),
            reason: "default_input_changed".into()
        }
    );
    assert_eq!(
        next(&mut stream).await,
        AudioEvent::TrackState {
            track: Track::System,
            state: TrackState::Stalled,
            message: Some("no data".into())
        }
    );
    assert_eq!(
        next(&mut stream).await,
        AudioEvent::Overrun {
            track: Track::Mic,
            dropped_frames: 3
        }
    );
    source.stop().await.unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn sidecar_crash_restarts_capture_and_emits_restarted() {
    let dir = tempfile::tempdir().unwrap();
    let client = start(dir.path(), &audio_cases()).await;
    let source = MacAudioSource::new(client.clone());
    let (_, mut stream) = source.start(request()).await.unwrap();
    assert!(matches!(next(&mut stream).await, AudioEvent::Frame(_)));
    source.pause().await.unwrap();
    // El falso muere con capture.retarget.
    let _ = source
        .retarget(SystemTarget {
            enabled: true,
            pids: vec![100, 102],
            bundle_ids: vec![],
        })
        .await;
    assert_eq!(
        next(&mut stream).await,
        AudioEvent::Restarted {
            restarted_at: UnixMs(1_700_000_000_002)
        }
    );
    assert!(matches!(next(&mut stream).await, AudioEvent::Frame(_)));
    // Tras reiniciar se repite capture.start con los PIDs nuevos y, como
    // estaba en pausa, también capture.pause.
    let ok = eventually(2000, || {
        let cmds: Vec<String> = requests(dir.path())
            .iter()
            .map(|r| r["cmd"].as_str().unwrap_or("").to_string())
            .collect();
        cmds.iter().filter(|c| *c == "capture.start").count() == 2
            && cmds.iter().filter(|c| *c == "capture.pause").count() == 2
    })
    .await;
    assert!(ok, "{:?}", requests(dir.path()));
    let restart = requests(dir.path())
        .into_iter()
        .filter(|r| r["cmd"] == "capture.start")
        .nth(1)
        .unwrap();
    assert_eq!(restart["args"]["system"]["pids"], serde_json::json!([100, 102]));
    source.stop().await.unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn giving_up_fails_both_tracks_and_ends_stream() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(fake_sidecar(dir.path(), &audio_cases()));
    cfg.max_restarts = 0;
    let client = acta_mac_bridge::client::SidecarClient::start(cfg).await.unwrap();
    let source = MacAudioSource::new(client.clone());
    let (_, mut stream) = source.start(request()).await.unwrap();
    let _frame = next(&mut stream).await;
    let _ = source
        .retarget(SystemTarget {
            enabled: true,
            pids: vec![],
            bundle_ids: vec![],
        })
        .await;
    let mut failed = Vec::new();
    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
        if let AudioEvent::TrackState {
            track,
            state: TrackState::Failed,
            ..
        } = ev
        {
            failed.push(track);
        }
    }
    failed.sort();
    assert_eq!(failed, vec![Track::Mic, Track::System], "stream ended after both failed");
    client.shutdown().await.unwrap();
}
