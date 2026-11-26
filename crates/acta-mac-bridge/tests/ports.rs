//! Adaptadores de puertos (speakers, screen, ASR, diarización, permisos,
//! modelos) contra el sidecar falso.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use acta_core::detection::{MeetingApp, Platform, SpeakerNameSource, SpeakerSignal};
use acta_core::ids::MeetingId;
use acta_core::permissions::{PermissionKind, PermissionStatus, PermissionsProbe};
use acta_core::recording::Track;
use acta_core::screen::{CaptureReason, ClipboardImporter, ScreenCaptureRequest, ScreenCapturer, ScreenEvent};
use acta_core::time::UnixMs;
use acta_core::transcription::{DiarizeOptions, Diarizer, LiveTranscriber, TranscribeRequest, Transcriber};
use acta_core::{EventStream, PortError};
use acta_mac_bridge::permissions::MacPermissions;
use acta_mac_bridge::screen::{MacClipboardImporter, MacScreenCapturer};
use acta_mac_bridge::speakers::MacSpeakerNames;
use acta_mac_bridge::transcription::{MacDiarizer, MacLiveTranscriber, MacModels, MacTranscriber, MacVoiceprints};
use futures::StreamExt;
use pretty_assertions::assert_eq;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::*;

async fn next<T>(s: &mut EventStream<T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), s.next())
        .await
        .expect("in time")
        .expect("open")
}

fn teams() -> MeetingApp {
    MeetingApp {
        platform: Platform::Teams,
        bundle_id: "com.microsoft.teams2".into(),
        pid: 100,
        audio_pids: vec![100, 101],
        name: "Microsoft Teams".into(),
        title: Some("Daily | Microsoft Teams".into()),
        url: None,
    }
}

// --------------------------------------------------------------------------
// speakers
// --------------------------------------------------------------------------

#[tokio::test]
async fn speakers_maps_roster_and_active_for_the_watched_pid() {
    let dir = tempfile::tempdir().unwrap();
    let cases = format!(
        "    speakers.watch) {}; {}; {}; {};;\n    speakers.unwatch) {};;",
        ok("{}"),
        event("speakers.active", r#"{"pid":999,"names":["Otro"]}"#),
        event(
            "speakers.roster",
            r#"{"pid":100,"participants":[{"name":"Ana","is_self":false,"muted":true},{"name":"Edu","is_self":true,"muted":null}]}"#
        ),
        event("speakers.active", r#"{"pid":100,"names":["Ana"]}"#),
        ok("{}")
    );
    let client = start(dir.path(), &cases).await;
    let names = MacSpeakerNames::new(client.clone());
    let mut s = names.watch(&teams()).await.unwrap();
    match next(&mut s).await {
        SpeakerSignal::Roster { participants, at } => {
            assert_eq!(at, UnixMs(1_700_000_000_000));
            assert_eq!(participants.len(), 2);
            assert!(participants[1].is_self);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        next(&mut s).await,
        SpeakerSignal::Active {
            names: vec!["Ana".into()],
            at: UnixMs(1_700_000_000_000)
        }
    );
    let req = requests(dir.path()).into_iter().find(|r| r["cmd"] == "speakers.watch").unwrap();
    assert_eq!(req["args"], serde_json::json!({"pid": 100, "bundle_id": "com.microsoft.teams2"}));
    names.unwatch().await.unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(2), s.next()).await.unwrap().is_none());
    client.shutdown().await.unwrap();
}

// --------------------------------------------------------------------------
// screen
// --------------------------------------------------------------------------

const FRAME: &str = r#"{"path":"/m/captures/000001000-slide_change.jpg","at_unix_ms":1700000001000,"reason":"slide_change","phash":"00ff00ff00ff00ff","width":1280,"height":720,"keyword":null}"#;

#[tokio::test]
async fn screen_start_maps_share_state_and_frames_and_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let snap = r#"{"frame":{"path":"/m/captures/000002000-keyword.jpg","at_unix_ms":1700000002000,"reason":"keyword","phash":"0000000000000001","width":640,"height":360,"keyword":"deadline"}}"#;
    let cases = format!(
        "    screen.start) {}; {}; {}; {}; {};;\n    screen.snapshot) {};;\n    screen.stop) {};;",
        ok("{}"),
        event("screen.share_state", r#"{"sharing":true,"presenter":"Ana","source":"ax"}"#),
        event("screen.share_state", r#"{"sharing":true,"presenter":"Ana","source":"frame"}"#),
        event("screen.frame", FRAME),
        event("screen.share_state", r#"{"sharing":false,"presenter":null,"source":"ax"}"#),
        ok(snap),
        ok("{}")
    );
    let client = start(dir.path(), &cases).await;
    let cap = MacScreenCapturer::new(client.clone());
    let request = ScreenCaptureRequest {
        meeting_id: MeetingId::from_u128(9),
        app: Some(teams()),
        out_dir: "/m/captures".into(),
        candidates_dir: "/m/captures/candidates".into(),
        fps: 1.0,
        phash_threshold: 10,
        stable_frames: 2,
        candidate_every_s: 30,
        ring_seconds: 20,
        max_width: 1920,
    };
    let mut s = cap.start(request).await.unwrap();
    assert_eq!(
        next(&mut s).await,
        ScreenEvent::ShareStarted {
            presenter: Some("Ana".into()),
            at: UnixMs(1_700_000_000_000)
        }
    );
    // El segundo sharing=true (otra fuente) no repite ShareStarted.
    match next(&mut s).await {
        ScreenEvent::FrameSaved { frame } => {
            assert_eq!(frame.reason, CaptureReason::SlideChange);
            assert_eq!(frame.at, UnixMs(1_700_000_001_000));
            assert_eq!(frame.phash, "00ff00ff00ff00ff");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        next(&mut s).await,
        ScreenEvent::ShareEnded {
            at: UnixMs(1_700_000_000_000)
        }
    );
    let frame = cap
        .snapshot(CaptureReason::Keyword, UnixMs(1_700_000_002_000), Some("deadline".into()))
        .await
        .unwrap()
        .expect("frame");
    assert_eq!(frame.keyword.as_deref(), Some("deadline"));
    assert_eq!((frame.width, frame.height), (640, 360));
    let reqs = requests(dir.path());
    let start_req = reqs.iter().find(|r| r["cmd"] == "screen.start").unwrap();
    assert_eq!(start_req["args"]["pid"], 100);
    assert_eq!(start_req["args"]["window_title_hint"], "Daily | Microsoft Teams");
    assert_eq!(start_req["args"]["format"], "jpeg");
    assert_eq!(start_req["args"]["session_id"], MeetingId::from_u128(9).to_string());
    let snap_req = reqs.iter().find(|r| r["cmd"] == "screen.snapshot").unwrap();
    assert_eq!(
        snap_req["args"],
        serde_json::json!({"reason": "keyword", "at_unix_ms": 1_700_000_002_000_i64, "keyword": "deadline"})
    );
    cap.stop().await.unwrap();
    client.shutdown().await.unwrap();
}

/// Grabación manual sin app detectada: se pide la pantalla principal
/// (`pid` 0) en vez de no capturar nada (19-nov-2026: 4 de 9 reuniones sin
/// ninguna captura porque no había app).
#[tokio::test]
async fn screen_start_without_an_app_asks_for_the_main_display() {
    let dir = tempfile::tempdir().unwrap();
    let cases = format!("    screen.start) {};;\n    screen.stop) {};;", ok("{}"), ok("{}"));
    let client = start(dir.path(), &cases).await;
    let cap = MacScreenCapturer::new(client.clone());
    let request = ScreenCaptureRequest {
        meeting_id: MeetingId::from_u128(9),
        app: None,
        out_dir: "/m/captures".into(),
        candidates_dir: "/m/captures/candidates".into(),
        fps: 1.0,
        phash_threshold: 10,
        stable_frames: 2,
        candidate_every_s: 30,
        ring_seconds: 20,
        max_width: 1920,
    };
    let _s = cap.start(request).await.unwrap();
    let reqs = requests(dir.path());
    let start_req = reqs.iter().find(|r| r["cmd"] == "screen.start").unwrap();
    assert_eq!(start_req["args"]["pid"], 0);
    assert_eq!(start_req["args"]["bundle_id"], "");
    assert!(start_req["args"]["window_title_hint"].is_null());
    cap.stop().await.unwrap();
    client.shutdown().await.unwrap();
}

/// Recortes del usuario (Cmd+Ctrl+Mayús+4) → capturas `manual`.
#[tokio::test]
async fn clipboard_images_become_manual_frames() {
    let dir = tempfile::tempdir().unwrap();
    let img = r#"{"path":"/m/captures/1700000003000-manual.jpg","at_unix_ms":1700000003000,"reason":"manual","phash":"0f0f0f0f0f0f0f0f","width":800,"height":600,"keyword":null}"#;
    let cases = format!(
        "    clipboard.start) {}; {}; {};;\n    clipboard.stop) {};;",
        ok("{}"),
        event("screen.frame", FRAME),
        event("clipboard.image", img),
        ok("{}")
    );
    let client = start(dir.path(), &cases).await;
    let clip = MacClipboardImporter::new(client.clone());
    let mut s = clip.start("/m/captures".into(), 1920).await.unwrap();
    let f = next(&mut s).await;
    assert_eq!(f.reason, CaptureReason::Manual, "los screen.frame no son del portapapeles");
    assert_eq!(f.at, UnixMs(1_700_000_003_000));
    assert_eq!((f.width, f.height), (800, 600));
    let reqs = requests(dir.path());
    let req = reqs.iter().find(|r| r["cmd"] == "clipboard.start").unwrap();
    assert_eq!(req["args"], serde_json::json!({"out_dir": "/m/captures", "max_width": 1920}));
    clip.stop().await.unwrap();
    assert!(reqs.iter().all(|r| r["cmd"] != "screen.start"));
    client.shutdown().await.unwrap();
}

// --------------------------------------------------------------------------
// ASR + diarización
// --------------------------------------------------------------------------

/// El falso escribe el fichero de salida en `out_path` (extraído de la línea).
fn job_cases() -> String {
    let asr = r#"{"model":"parakeet-tdt-0.6b-v3","language":"es","duration_ms":5000,"segments":[{"start_ms":0,"end_ms":1200,"text":"Hola a todos","confidence":0.93,"words":[{"start_ms":0,"end_ms":400,"text":"Hola"}]}]}"#;
    let diar = r#"{"embedding_model":"wespeaker-v2","turns":[{"label":"S1","start_ms":0,"end_ms":1000},{"label":"S2","start_ms":1000,"end_ms":2000}],"speakers":[{"label":"S1","embedding":[0.5,0.5],"total_ms":1000},{"label":"S2","embedding":[0.1,0.9],"total_ms":1000}]}"#;
    let out = r#"out=${line#*\"out_path\":\"}; out=${out%%\"*}; job=${line#*\"job_id\":\"}; job=${job%%\"*}"#;
    let prog = |stage: &str| {
        format!(r#"printf '{{"type":"event","ts":1,"event":"job.progress","data":{{"job_id":"%s","fraction":0.5,"stage":"{stage}"}}}}\n' "$job""#)
    };
    format!(
        r#"    asr.transcribe) {out}; {p1}; mkdir -p "$(dirname "$out")"; printf '%s' '{asr}' > "$out"; printf '{{"type":"response","id":%s,"ok":true,"result":{{"out_path":"%s","segments":1,"duration_ms":5000}}}}\n' "$id" "$out";;
    diarize.run) {out}; {p2}; mkdir -p "$(dirname "$out")"; printf '%s' '{diar}' > "$out"; printf '{{"type":"response","id":%s,"ok":true,"result":{{"out_path":"%s","speakers":2}}}}\n' "$id" "$out";;
    voiceprint.embed) {emb};;
    models.status) {models};;
    models.download) {ok}; {mp};;
    asr.live.start) {ok}; {partial};;
    asr.live.stop) {ok};;"#,
        p1 = prog("asr"),
        p2 = prog("diarize"),
        emb = ok(r#"{"embedding":[0.1,0.2,0.3],"model":"wespeaker-v2"}"#),
        models = ok(r#"{"models":[{"name":"asr","downloaded":true,"bytes":480000000},{"name":"diarizer","downloaded":false,"bytes":null}]}"#),
        mp = event("models.progress", r#"{"name":"diarizer","fraction":1.0,"done":true,"error":null}"#),
        partial = event("asr.partial", r#"{"track":"system","text":"esto es importante","at_unix_ms":1700000005000,"is_final":true}"#),
        ok = ok("{}"),
    )
}

#[tokio::test]
async fn transcriber_writes_asr_file_in_the_meeting_layout_and_reports_progress() {
    let dir = tempfile::tempdir().unwrap();
    let client = start(dir.path(), &job_cases()).await;
    let root = dir.path().join("data");
    let id = MeetingId::from_u128(5);
    let layout = acta_core::layout::DataLayout::new(&root);
    let wav = layout.track_wav(id, Track::System);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let progress: acta_core::transcription::ProgressFn = Arc::new(move |f| seen2.lock().unwrap().push(f));
    let asr = MacTranscriber::new(client.clone());
    let segments = asr
        .transcribe(
            TranscribeRequest {
                wav_path: wav.clone(),
                track: Track::System,
                language: Some("es".into()),
            },
            Some(progress),
        )
        .await
        .unwrap();
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].text, "Hola a todos");
    assert_eq!(segments[0].words[0].text, "Hola");
    assert!(layout.asr_json(id, Track::System).is_file());
    assert_eq!(*seen.lock().unwrap(), vec![0.5]);
    let req = requests(dir.path()).into_iter().find(|r| r["cmd"] == "asr.transcribe").unwrap();
    assert_eq!(req["args"]["model"], "parakeet-tdt-0.6b-v3");
    assert_eq!(req["args"]["language"], "es");
    assert_eq!(req["args"]["wav_path"], wav.display().to_string());
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn diarizer_reads_diarization_file() {
    let dir = tempfile::tempdir().unwrap();
    let client = start(dir.path(), &job_cases()).await;
    let layout = acta_core::layout::DataLayout::new(dir.path().join("data"));
    let id = MeetingId::from_u128(5);
    let d = MacDiarizer::new(client.clone());
    let res = d
        .diarize(
            &layout.track_wav(id, Track::System),
            DiarizeOptions {
                max_speakers: Some(6),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(res.embedding_model, "wespeaker-v2");
    assert_eq!(res.turns.len(), 2);
    assert_eq!(res.speakers[1].label, "S2");
    assert!(layout.diarization_json(id).is_file());
    let req = requests(dir.path()).into_iter().find(|r| r["cmd"] == "diarize.run").unwrap();
    assert_eq!(req["args"]["max_speakers"], 6);
    assert_eq!(req["args"]["num_speakers"], serde_json::Value::Null);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelled_job_maps_to_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let cases = format!("    asr.transcribe) {};;", err("cancelled", "job.cancel"));
    let client = start(dir.path(), &cases).await;
    let asr = MacTranscriber::new(client.clone());
    let e = asr
        .transcribe(
            TranscribeRequest {
                wav_path: dir.path().join("x.wav"),
                track: Track::Mic,
                language: None,
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(e, PortError::Cancelled);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropping_a_running_job_sends_job_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let cases = format!("    asr.transcribe) : ;;\n    job.cancel) {};;", ok("{}"));
    let client = start(dir.path(), &cases).await;
    let asr = MacTranscriber::new(client.clone());
    let fut = asr.transcribe(
        TranscribeRequest {
            wav_path: dir.path().join("x.wav"),
            track: Track::Mic,
            language: None,
        },
        None,
    );
    let _ = tokio::time::timeout(Duration::from_millis(200), fut).await;
    let found = eventually(2000, || requests(dir.path()).iter().any(|r| r["cmd"] == "job.cancel")).await;
    assert!(found);
    let reqs = requests(dir.path());
    let job = reqs.iter().find(|r| r["cmd"] == "asr.transcribe").unwrap()["args"]["job_id"].clone();
    let cancel = reqs.iter().find(|r| r["cmd"] == "job.cancel").unwrap();
    assert_eq!(cancel["args"]["job_id"], job);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn live_transcriber_streams_partials() {
    let dir = tempfile::tempdir().unwrap();
    let client = start(dir.path(), &job_cases()).await;
    let live = MacLiveTranscriber::new(client.clone());
    let mut s = live.start(&[Track::System], Some("es".into())).await.unwrap();
    let p = next(&mut s).await;
    assert_eq!(p.track, Track::System);
    assert_eq!(p.text, "esto es importante");
    assert_eq!(p.at, UnixMs(1_700_000_005_000));
    assert!(p.is_final);
    live.stop().await.unwrap();
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn voiceprints_and_models() {
    let dir = tempfile::tempdir().unwrap();
    let client = start(dir.path(), &job_cases()).await;
    let vp = MacVoiceprints::new(client.clone());
    let (embedding, model) = vp.embed(&dir.path().join("system.wav"), &[(1000, 4000)]).await.unwrap();
    assert_eq!(embedding, vec![0.1, 0.2, 0.3]);
    assert_eq!(model, "wespeaker-v2");
    let models = MacModels::new(client.clone());
    let status = models.status().await.unwrap();
    assert_eq!(status.len(), 2);
    assert!(status[0].downloaded);
    let mut progress = models.download("diarizer").await.unwrap();
    let p = next(&mut progress).await;
    assert_eq!(p.name, "diarizer");
    assert!(p.done);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), progress.next()).await.unwrap().is_none(),
        "download stream ends when done"
    );
    client.shutdown().await.unwrap();
}

// --------------------------------------------------------------------------
// permisos
// --------------------------------------------------------------------------

#[tokio::test]
async fn permissions_status_and_request() {
    let dir = tempfile::tempdir().unwrap();
    let cases = format!(
        "    permissions.status) {};;\n    permissions.request) {};;",
        ok(r#"{"permissions":[{"kind":"microphone","status":"granted","target":null},{"kind":"system_audio","status":"unknown","target":null},{"kind":"automation","status":"not_determined","target":"com.google.Chrome"}]}"#),
        ok(r#"{"status":"denied"}"#)
    );
    let client = start(dir.path(), &cases).await;
    let p = MacPermissions::new(client.clone());
    let st = p.status().await.unwrap();
    assert_eq!(st.len(), 3);
    assert_eq!(st[0].kind, PermissionKind::Microphone);
    assert_eq!(st[1].status, PermissionStatus::Unknown);
    assert_eq!(st[2].target.as_deref(), Some("com.google.Chrome"));
    assert_eq!(p.request(PermissionKind::ScreenRecording).await.unwrap(), PermissionStatus::Denied);
    let req = requests(dir.path()).into_iter().find(|r| r["cmd"] == "permissions.request").unwrap();
    assert_eq!(req["args"]["kind"], "screen_recording");
    client.shutdown().await.unwrap();
}
