// Captura WASAPI del sidecar de Windows. TODO el `unsafe` del binario vive en
// este módulo: la API de Windows es unsafe y el workspace niega `unsafe_code`,
// así que se confina aquí (AGENTS.md §7.1) con un comentario `// SAFETY:` por
// bloque. A diferencia del experimento T14, aquí no se escriben WAV: cada
// pista convierte sus paquetes a 16 kHz mono s16, los ensambla en frames de
// 100 ms (`assemble`) y los emite como `audio.frame` por el canal de sesión.
//
// API verificada contra la fuente del crate `windows` 0.62.2
// (Windows/Win32/Media/Audio/mod.rs) y contra el SDK 10.0.26100.0
// (`audioclientactivationparams.h`): el struct plano
// `AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS { TargetProcessId, ProcessLoopbackMode }`
// coincide con el ABI real del SO.
//
// HALLAZGO CENTRAL del experimento: `ActivateAudioInterfaceAsync` se apropia
// del blob de `AUDIOCLIENT_ACTIVATION_PARAMS` y lo libera con `CoTaskMemFree`.
// Un blob en el stack o en un Box de Rust corrompe el heap (0xC0000374) cuando
// el sistema lo libera, así que aquí el blob vive en `CoTaskMemAlloc` y no se
// libera en el proceso (el sistema lo libera; medido en esta máquina).
#![allow(unsafe_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient,
    IMMDeviceEnumerator, MMDeviceEnumerator, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    WAVEFORMATEXTENSIBLE_0, eCapture, eConsole, eRender,
};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::System::Com::{
    BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemAlloc,
    CoTaskMemFree,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{Interface, IUnknown, Ref, implement};
/// mmreg.h: WAVE_FORMAT_IEEE_FLOAT = 3 (evita activar Win32_Media_Multimedia).
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
/// ksmedia.h: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT (evita activar Win32_Media_Multimedia).
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: windows::core::GUID =
    windows::core::GUID::from_u128(0x0000_0003_0000_0010_8000_00aa_0038_9b71);

use crate::assemble::{FrameAssembler, OUTPUT_SAMPLE_RATE, PcmFrame};
use crate::protocol::{
    self, AudioFrameEvent, AudioOverrunEvent, AudioTrackStateEvent, CaptureStartArgs, EventOut,
    Outgoing, TrackName, TrackStateName,
};
use crate::session::{LaunchError, SpawnedTracks, TrackControls, TrackLauncher};

/// 16 kHz, mono, s16: el contrato de `acta-audio` (AGENTS.md §7.1).
const SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone, Copy)]
pub enum Source {
    /// Micrófono por defecto (captura de dispositivo WASAPI normal).
    Mic,
    /// Lo que reproduce el proceso indicado y sus procesos hijos.
    ProcessLoopback { pid: u32 },
}

/// Versión real del SO (major.minor.build): sin manifiesto, `GetVersionExW`
/// miente y devolvería 6.2. El contenido alimenta el campo `macos_version`
/// del `hello`; el nombre del campo lo fija el contrato del cliente.
pub fn windows_version() -> String {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..OSVERSIONINFOW::default()
    };
    // SAFETY: llamada de ntdll con un struct de salida propio inicializado;
    // no toca memoria ajena y solo escribe en `info`.
    let status = unsafe { RtlGetVersion(&mut info) };
    if status.0 >= 0 {
        format!("{}.{}.{}", info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber)
    } else {
        "unknown".into()
    }
}

fn init_com() -> Result<()> {
    // SAFETY: llamada COM estándar sin parámetros. Cada hilo de captura
    // inicializa su propio apartment; RPC_E_CHANGED_MODE solo aparece si el
    // anfitrión ya inicializó COM en otro modo, que es compatible.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_err() && hr != RPC_E_CHANGED_MODE {
        bail!("CoInitializeEx falló: {hr}");
    }
    Ok(())
}

/// Captura de dispositivo normal: enumerador de MMDevice + dispositivo por defecto.
fn activate_mic() -> Result<IAudioClient> {
    // SAFETY: CoCreateInstance con el CLSID estándar `MMDeviceEnumerator`.
    // El apartment ya está inicializado por `init_com`.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
    // SAFETY: llamada COM del enumerador; devuelve un puntero de interfaz.
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) }?;
    // SAFETY: Activate de un objeto ya construido; el micrófono no necesita
    // parámetros de activación. Conversión explícita del error para no fiar la
    // coerción del `?` entre tipos de error distintos.
    unsafe { device.Activate::<IAudioClient>(CLSCTX_ALL, None) }
        .map_err(|error| anyhow::anyhow!("Activate del micrófono: {error}"))
}

/// Id del dispositivo de captura por defecto, para el campo `mic_device` del
/// resultado de `capture.start`. Fallar deja `mic_device` vacío, como en mac.
fn mic_device_id() -> Result<String> {
    // SAFETY: CoCreateInstance del enumerador estándar; el apartment ya está
    // inicializado por `init_com`.
    let device = unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        enumerator.GetDefaultAudioEndpoint(eCapture, eConsole)?
    };
    // SAFETY: firma COM de IMMDevice::GetId; devuelve un PWSTR reservado con
    // CoTaskMem que aquí se convierte a String y se libera.
    unsafe {
        let id = device.GetId()?;
        let text = id.to_string().unwrap_or_default();
        CoTaskMemFree(Some(id.as_ptr().cast()));
        Ok(text)
    }
}

/// Loopback de proceso (Windows 10 2004+): `ActivateAudioInterfaceAsync` con el
/// dispositivo virtual `VAD\Process_Loopback` y los parámetros de activación en
/// un PROPVARIANT VT_BLOB.
fn activate_process_loopback(pid: u32) -> Result<IAudioClient> {
    // El blob lo libera el sistema con CoTaskMemFree (hallazgo central), así
    // que se reserva con su propio asignador y no se libera aquí.
    // SAFETY: CoTaskMemAlloc con el tamaño del struct; el puntero nulo se
    // comprueba justo después y la memoria queda inicializada por ptr::write.
    let params: *mut AUDIOCLIENT_ACTIVATION_PARAMS = unsafe {
        CoTaskMemAlloc(std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>()).cast()
    };
    if params.is_null() {
        bail!("CoTaskMemAlloc sin memoria");
    }
    // SAFETY: escritura única en memoria recién reservada del mismo tamaño.
    unsafe {
        std::ptr::write(
            params,
            AUDIOCLIENT_ACTIVATION_PARAMS {
                ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
                Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                    ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                        // INCLUDE captura también los hijos del proceso, que es
                        // lo que necesitan las apps de reunión con procesos hijos.
                        TargetProcessId: pid,
                        ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                    },
                },
            },
        );
    }

    let mut prop_variant = PROPVARIANT::default();
    // SAFETY: se escribe un PROPVARIANT VT_BLOB cuyo `pBlobData` apunta al blob
    // de CoTaskMem. `ActivateAudioInterfaceAsync` se apropia de ese blob (véase
    // el hallazgo del encabezado) y el PROPVARIANT solo se usa en esta llamada.
    // El campo del union es `ManuallyDrop`, así que hay que derivarlo a mano.
    unsafe {
        (*prop_variant.Anonymous.Anonymous).vt = VT_BLOB;
        (*prop_variant.Anonymous.Anonymous).Anonymous.blob = BLOB {
            cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            pBlobData: params.cast(),
        };
    }

    // La activación avisa por el handler; el subproceso COM que lo invoca es
    // ajeno a este hilo, así que la señalización debe ser Sync (Mutex+Condvar).
    // `implement` genera un objeto COM ágil (IAgileObject+IMarshal por defecto
    // en windows-rs) y `done` es Arc<Completion>, Sync, así que el acceso
    // desde el subproceso de COM es correcto.
    let done = Arc::new(Completion::default());
    let handler: IActivateAudioInterfaceCompletionHandler =
        ActivationHandler { done: done.clone() }.into();

    // SAFETY: primera y única llamada de activación; `VIRTUAL_AUDIO_DEVICE_
    // PROCESS_LOOPBACK` es una cadena estática y el blob lo posee el sistema.
    let operation: IActivateAudioInterfaceAsyncOperation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&prop_variant),
            &handler,
        )
    }?;

    if !done.wait_for_completion(Duration::from_secs(20)) {
        bail!("la activación del loopback no terminó en 20 s");
    }

    let mut activation_result = windows::core::HRESULT(0);
    let mut activated: Option<IUnknown> = None;
    // SAFETY: firma COM de IActivateAudioInterfaceAsyncOperation; se recuperan
    // el HRESULT y la interfaz pedida (IAudioClient). Conversión explícita del
    // error para no fiar la coerción del `?` entre tipos de error distintos.
    unsafe { operation.GetActivateResult(&mut activation_result, &mut activated) }
        .map_err(|error| anyhow::anyhow!("GetActivateResult: {error}"))?;
    activation_result
        .ok()
        .map_err(|error| anyhow::anyhow!("activación del loopback: {error}"))?;
    let client = activated
        .context("la activación terminó sin devolver la interfaz")?
        .cast::<IAudioClient>()?;
    Ok(client)
}

/// Cliente de audio del dispositivo de render por defecto (para obtener su mix
/// format, que el dispositivo virtual de loopback no sabe dar).
fn activate_render_device() -> Result<IAudioClient> {
    // SAFETY: CoCreateInstance con el CLSID estándar `MMDeviceEnumerator`.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
    // SAFETY: llamada COM del enumerador; devuelve un puntero de interfaz.
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }?;
    // SAFETY: Activate de un objeto ya construido, sin parámetros de activación.
    unsafe { device.Activate::<IAudioClient>(CLSCTX_ALL, None) }
        .map_err(|error| anyhow::anyhow!("Activate del render: {error}"))
}

struct Completion {
    state: Mutex<bool>,
    progress: Condvar,
}

impl Completion {
    fn wait_for_completion(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*state {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            if remaining.is_zero() {
                return false;
            }
            let (next, _) = self
                .progress
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
        }
        true
    }

    fn notify_completed(&self) {
        if let Ok(mut state) = self.state.lock() {
            *state = true;
            self.progress.notify_all();
        }
    }
}

impl Default for Completion {
    fn default() -> Self {
        Self {
            state: Mutex::new(false),
            progress: Condvar::new(),
        }
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationHandler {
    done: Arc<Completion>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        _activate_operation: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        // El resultado real (HRESULT + interfaz) se lee después con
        // GetActivateResult desde el hilo de captura: aquí solo avisamos.
        self.done.notify_completed();
        Ok(())
    }
}

// Aserción silenciosa: el handler se toca desde un subproceso de COM, así que
// su tipo interior debe ser Sync (más estricto de lo que el compilador
// comprueba sin esta línea).
const _: () = {
    const fn assert_sync<T: Sync>() {}
    assert_sync::<ActivationHandler>();
};

/// Formato de salida del micrófono: 16 kHz, mono, s16 extensible. El motor
/// convierte gracias a AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM.
fn extensible_16k_mono_s16() -> WAVEFORMATEXTENSIBLE {
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: 1,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * 2,
            nBlockAlign: 2,
            wBitsPerSample: 16,
            cbSize: (std::mem::size_of::<WAVEFORMATEXTENSIBLE>()
                - std::mem::size_of::<WAVEFORMATEX>()) as u16,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: 16 },
        dwChannelMask: 0x4, // SPEAKER_FRONT_CENTER: mono
        SubFormat: KSDATAFORMAT_SUBTYPE_PCM,
    }
}

/// Captura ya arrancada: cliente de audio + formato de origen. El micrófono
/// sale del motor ya en 16 kHz mono s16 (`direct: true`); el loopback exige el
/// mix format del dispositivo de render y se convierte por paquete.
struct TrackStream {
    client: IAudioClient,
    capture_client: IAudioCaptureClient,
    src_rate: u32,
    channels: u16,
    /// Bytes por marco de origen (2 en el micrófono s16).
    bytes_per_frame: usize,
    float_data: bool,
    /// Micrófono: sin reprocesar, es s16 mono 16 kHz tal cual.
    direct: bool,
}

/// Activa, inicializa y arranca la captura del `Source`.
fn stream_source(source: Source) -> Result<TrackStream> {
    init_com()?;
    match source {
        Source::Mic => {
            let client = activate_mic()?;
            let format = extensible_16k_mono_s16();
            // SAFETY: firma COM de IAudioClient::Initialize. AUTOCONVERTPCM pide al
            // motor que convierta al formato dado; sin RPC hnsPeriodicity debe ser 0.
            unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                    0,
                    0,
                    std::ptr::addr_of!(format).cast(),
                    None,
                )?;
                // SAFETY: firma COM; arranca la captura del cliente ya inicializado.
                client.Start()?;
                // SAFETY: GetService pide la interfaz IAudioCaptureClient del mismo objeto.
            }
            let capture_client: IAudioCaptureClient = unsafe { client.GetService() }?;
            Ok(TrackStream {
                client,
                capture_client,
                src_rate: SAMPLE_RATE,
                channels: 1,
                bytes_per_frame: 2,
                float_data: false,
                direct: true,
            })
        }
        Source::ProcessLoopback { pid } => {
            let client = activate_process_loopback(pid)?;
            // El dispositivo virtual exige el formato exacto de su sesión y no
            // implementa GetMixFormat (E_NOTIMPL, medido), así que el formato
            // se toma del mix format del dispositivo de render por defecto
            // (como hace la muestra ApplicationLoopback).
            // SAFETY: GetMixFormat del dispositivo de render entrega un puntero
            // CoTaskMem; se leen los campos y el puntero queda vivo hasta que
            // `Initialize` lo copia (véase la liberación justo después).
            let (src_rate, channels, bytes_per_frame, float_data, mix_ptr) = unsafe {
                let renderer = activate_render_device()?;
                let mix: *mut WAVEFORMATEX = renderer
                    .GetMixFormat()
                    .map_err(|error| anyhow::anyhow!("GetMixFormat del render: {error}"))?;
                let base = *mix;
                let extensible = base.wFormatTag == WAVE_FORMAT_EXTENSIBLE as u16 && base.cbSize >= 22;
                let float_data = if extensible {
                    // SAFETY: Campo al final de un struct packed: lectura alineada a lo que hay.
                    core::ptr::addr_of!((*mix.cast::<WAVEFORMATEXTENSIBLE>()).SubFormat)
                        .read_unaligned()
                        == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
                } else {
                    base.wFormatTag == WAVE_FORMAT_IEEE_FLOAT
                };
                (
                    base.nSamplesPerSec,
                    base.nChannels,
                    base.nBlockAlign,
                    float_data,
                    mix,
                )
            };
            eprintln!(
                "acta-win: loopback pid {pid}: mix format del render: {} Hz × {} canales ({} bytes por marco, float: {})",
                src_rate, channels, bytes_per_frame, float_data
            );

            // SAFETY: Initialize con el formato del render. AUDCLNT_STREAMFLAGS_LOOPBACK
            // es obligatorio con el dispositivo virtual: sin él, Initialize devuelve
            // AUDCLNT_E_INVALID_STREAM_FLAG (0x88890021, medido en esta máquina).
            let initialized = unsafe {
                client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 0, 0, mix_ptr, None)
            };
            // SAFETY: el sistema copia el formato durante Initialize, así que el buffer
            // de CoTaskMem solo se puede liberar DESPUÉS. Liberarlo antes es un
            // use-after-free: Initialize lee memoria liberada (medido: 0x80070057).
            unsafe { CoTaskMemFree(Some(mix_ptr.cast())) };
            initialized.map_err(|error| anyhow::anyhow!("Initialize del loopback: {error}"))?;
            // SAFETY: firma COM; arranca la captura del cliente ya inicializado.
            unsafe { client.Start() }.map_err(|error| anyhow::anyhow!("Start del loopback: {error}"))?;
            // SAFETY: GetService pide la interfaz IAudioCaptureClient del mismo objeto.
            let capture_client: IAudioCaptureClient = unsafe { client.GetService() }
                .map_err(|error| anyhow::anyhow!("GetService del loopback: {error}"))?;
            Ok(TrackStream {
                client,
                capture_client,
                src_rate,
                channels,
                bytes_per_frame: bytes_per_frame as usize,
                float_data,
                direct: false,
            })
        }
    }
}

/// Un lado de pista: ensamblador + canal de salida, compartidos por los hilos
/// de captura y el control de parada. `system` puede tener una hebra de
/// loopback por PID y todas alimentan EL MISMO ensamblador: la decisión del
/// port (informe T14b) es concatenarlas en lugar de sumarlas, porque el caso
/// real es un proceso de reunión con su árbol de hijos; la mezcla real por PID
/// queda pendiente y se documenta, nunca se calla.
pub struct TrackIo {
    out: SyncSender<Outgoing>,
    track: TrackName,
    assembler: Mutex<FrameAssembler>,
    /// Frames de `audio.frame` perdidos porque el escritor se atrasó; se
    /// reportan juntos en el próximo `audio.overrun` que consiga pasar.
    dropped: AtomicU64,
    failed: AtomicBool,
}

#[derive(Debug, Clone, Copy)]
enum Delivery {
    Sent,
    Full,
    Closed,
}

/// Etiqueta de la pista para los mensajes de stderr.
fn track_label(track: TrackName) -> &'static str {
    match track {
        TrackName::Mic => "mic",
        TrackName::System => "system",
    }
}

impl TrackIo {
    fn new(out: SyncSender<Outgoing>, track: TrackName, frame_ms: u32) -> Arc<Self> {
        Arc::new(Self {
            out,
            track,
            assembler: Mutex::new(FrameAssembler::new(OUTPUT_SAMPLE_RATE, frame_ms)),
            dropped: AtomicU64::new(0),
            failed: AtomicBool::new(false),
        })
    }

    fn try_send(&self, event: EventOut) -> Delivery {
        match self.out.try_send(protocol::event(event)) {
            Ok(()) => Delivery::Sent,
            Err(TrySendError::Full(_)) => Delivery::Full,
            Err(TrySendError::Disconnected(_)) => Delivery::Closed,
        }
    }

    /// Encola muestras emitidas como frames. Devuelve `false` si el cliente ya
    /// no lee (canal cerrado): el hilo de captura debe parar.
    fn push(&self, samples: &[i16]) -> bool {
        let mut assembler = self
            .assembler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for frame in assembler.push(samples) {
            if !self.send_frame(frame) {
                return false;
            }
        }
        true
    }

    fn send_frame(&self, frame: PcmFrame) -> bool {
        let event = EventOut::AudioFrame(AudioFrameEvent {
            track: self.track,
            seq: frame.seq,
            pts_us: frame.pts_us,
            sample_rate: OUTPUT_SAMPLE_RATE,
            channels: 1,
            format: "s16le".into(),
            data: frame.base64(),
        });
        match self.try_send(event) {
            Delivery::Sent => {
                // Si antes se perdieron frames, se reportan en cuanto el canal
                // vuelve a tener sitio (mac: audio.overrun). Si el propio
                // overrun no cabe, se devuelve al recuento: no se pierde en
                // silencio y sale con la próxima entrega o en el cierre.
                let dropped = self.dropped.swap(0, Ordering::Relaxed);
                if dropped > 0 {
                    self.report_dropped(dropped);
                }
                true
            }
            Delivery::Full => {
                // No bloquea la captura: el frame se pierde y se cuenta.
                self.dropped.fetch_add(1, Ordering::Relaxed);
                true
            }
            Delivery::Closed => false,
        }
    }

    /// Emite `audio.overrun` con el recuento dado. Best effort: si el canal no
    /// admite el evento (lleno o cerrado al terminar el capture), el recuento
    /// vuelve al contador (si queda hilo vivo) y queda constancia por stderr:
    /// el cliente no debe perderlo sin rastro.
    fn report_dropped(&self, dropped: u64) {
        match self.try_send(EventOut::AudioOverrun(AudioOverrunEvent {
            track: self.track,
            dropped_frames: dropped,
        })) {
            Delivery::Sent => {}
            Delivery::Full => {
                self.dropped.fetch_add(dropped, Ordering::Relaxed);
                eprintln!(
                    "acta-win: pista {}: el canal esta lleno: {dropped} frames perdidos esperan al proximo audio.overrun",
                    track_label(self.track)
                );
            }
            Delivery::Closed => {
                eprintln!(
                    "acta-win: pista {}: capture cerrado sin poder reportar {dropped} frames perdidos",
                    track_label(self.track)
                );
            }
        }
    }

    /// Vacía el recuento de frames perdidos en los CAMINOS TERMINALES (parada
    /// y fallo de la pista): antes solo se vaciaba con la próxima entrega con
    /// éxito, que en la parada o el fallo puede no llegar jamás, y omitir el
    /// `audio.overrun` era mentirle al cliente. Se llama ANTES del estado
    /// final de la pista.
    fn flush_dropped(&self) {
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            self.report_dropped(dropped);
        }
    }

    /// Pausa: olvida el parcial sin emitir. La línea temporal no avanza, así
    /// que reanudar sigue donde quedó el audio emitido (mac: `discard`).
    fn discard(&self) {
        self.assembler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .discard();
    }

    /// Parada: cierra el parcial, emite `audio.track_state` stopped. mac
    /// emite el stopped desde `CaptureService.stop`, una vez paradas las
    /// pistas: aquí lo hace `controls` tras el join de los hilos.
    fn finish(&self) {
        let mut assembler = self
            .assembler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(frame) = assembler.flush() {
            drop(assembler);
            let _ = self.send_frame(frame);
        }
        self.flush_dropped();
        if !self.failed.swap(true, Ordering::Relaxed) {
            let _ = self.try_send(EventOut::AudioTrackState(AudioTrackStateEvent {
                track: self.track,
                state: TrackStateName::Stopped,
                message: None,
            }));
        }
    }

    /// Fallo de la pista: se reporta, la sesión sigue viva (mac: failed).
    fn fail(&self, message: String) {
        self.flush_dropped();
        self.failed.store(true, Ordering::Relaxed);
        let _ = self.try_send(EventOut::AudioTrackState(AudioTrackStateEvent {
            track: self.track,
            state: TrackStateName::Failed,
            message: Some(message),
        }));
    }
}

/// Bucle de una pista: drena paquetes WASAPI, convierte a 16 kHz mono s16 y
/// emite frames por el canal de sesión. Sale por `stop`, por pausa asesinada
/// (no: la pausa solo descarta) o por canal cerrado.
fn track_loop(stream: TrackStream, io: &TrackIo, paused: &AtomicBool, stop: &AtomicBool) -> Result<()> {
    // Sondeo cada 5 ms, medido en el experimento T14: el buffer compartido da
    // varios cientos de ms de margen y este ciclo drena todo lo pendiente en
    // cada vuelta, así que no hay overrun por sondeo. `SetEventHandle` queda
    // como mejora pendiente: no cambia nada observable del protocolo.
    let poll = Duration::from_millis(5);
    // Parada: plazo máximo para drenar lo que queda en el buffer compartido
    // de WASAPI (mac drena hasta GetNextPacketSize()==0). Acotado porque si
    // el proceso sigue sonando el buffer nunca se agota: pasado el plazo se
    // descarta el resto y queda constancia por stderr, nunca en silencio.
    let drain_limit = Duration::from_millis(1_000);
    let mut packet_bytes: Vec<u8> = Vec::new();
    let mut was_paused = paused.load(Ordering::Relaxed);
    let mut drain_deadline: Option<Instant> = None;
    loop {
        if let Some(deadline) = drain_deadline {
            if Instant::now() >= deadline {
                eprintln!(
                    "acta-win: pista {}: el drenado de la parada superó 1 s: se descarta lo que quede en el buffer",
                    track_label(io.track)
                );
                break;
            }
        } else if stop.load(Ordering::Relaxed) {
            // Orden de parada: drenar lo pendiente ANTES de cerrar la pista.
            // Cortar aquí directamente tiraba los paquetes que quedaban en el
            // buffer de WASAPI (cientos de ms) y la cola de la reunión se
            // trunciaba sin error.
            drain_deadline = Some(Instant::now() + drain_limit);
        }
        let paused_now = paused.load(Ordering::Relaxed);
        if paused_now && !was_paused {
            io.discard();
        }
        was_paused = paused_now;
        // SAFETY: firma COM de IAudioCaptureClient::GetNextPacketSize.
        let packet = unsafe { stream.capture_client.GetNextPacketSize() }?;
        if packet == 0 {
            if drain_deadline.is_some() {
                break; // drenado completo: nada pendiente en el buffer
            }
            thread::sleep(poll);
            continue;
        }
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut frames = 0u32;
        let mut flags = 0u32;
        // SAFETY: firma COM; `data` apunta a datos válidos para `frames`
        // marcos hasta el ReleaseBuffer, así que se copia antes de liberar.
        unsafe { stream.capture_client.GetBuffer(&mut data, &mut frames, &mut flags, None, None)? };
        let len = frames as usize * stream.bytes_per_frame;
        // SAFETY: el buffer contiene al menos `len` bytes válidos.
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(data, len) };
        packet_bytes.clear();
        packet_bytes.extend_from_slice(bytes);
        // SAFETY: libera el buffer leído arriba con el mismo número de frames.
        unsafe { stream.capture_client.ReleaseBuffer(frames)? };
        if paused_now {
            continue; // drenado y tirado: la pausa no emite audio
        }
        let samples = samples_from_packet(
            &packet_bytes,
            stream.src_rate,
            stream.channels,
            stream.float_data,
            stream.direct,
        );
        if samples.is_empty() {
            continue;
        }
        if !io.push(&samples) {
            break; // el cliente se fue: parar la pista
        }
    }
    // SAFETY: firma COM; detiene la captura al salir del bucle.
    unsafe { stream.client.Stop()? };
    Ok(())
}

/// Paquete de origen → muestras mono de 16 kHz. El micrófono ya sale del motor
/// en s16 mono 16 kHz (`direct`): no se toca, para no perder la muestra -32768
/// en el redaño del conversor. El loopback va por el conversor.
fn samples_from_packet(
    bytes: &[u8],
    src_rate: u32,
    channels: u16,
    float_data: bool,
    direct: bool,
) -> Vec<i16> {
    if direct {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|chunk| i16::from_le_bytes(*chunk))
            .collect()
    } else {
        convert_to_16k_mono_s16(bytes, src_rate, channels, float_data)
    }
}

fn track_main(source: Source, io: Arc<TrackIo>, paused: Arc<AtomicBool>, stop: Arc<AtomicBool>) {
    // Activar + arrancar y luego el bucle: cualquier caída de una pista no
    // tumba la sesión, se reporta como en mac (audio.track_state failed).
    let result = stream_source(source).and_then(|stream| track_loop(stream, &io, &paused, &stop));
    match result {
        Ok(()) => {}
        Err(error) => {
            // El fallo de una pista no tumba la sesión: se reporta como en mac.
            io.fail(error.to_string());
        }
    }
}

/// Lanzador real: una hebra por pista (micrófono, y loopback por cada PID del
/// `system`), todas con el ensamblador de su pista compartido. Devuelve el
/// control de pausa/parada en `SpawnedTracks`.
pub struct WinLauncher;

impl TrackLauncher for WinLauncher {
    fn launch(
        &mut self,
        args: &CaptureStartArgs,
        out: &SyncSender<Outgoing>,
    ) -> Result<SpawnedTracks, LaunchError> {
        let launch_error = |error: anyhow::Error| LaunchError {
            code: protocol::ErrorCode::Internal,
            message: error.to_string(),
        };
        init_com().map_err(launch_error)?;
        let mic_device = if args.mic.enabled {
            Some(mic_device_id().map_err(launch_error)?)
        } else {
            None
        };
        let paused = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let mut handles: Vec<thread::JoinHandle<()>> = Vec::new();
        let mut tracks: Vec<Arc<TrackIo>> = Vec::new();
        // Si un spawn falla con pistas ya corriendo, hay que pararlas y
        // esperarlas antes de responder (nunca se dejan hilos sueltos).
        let spawned = (|| -> Result<(), LaunchError> {
            let mut spawn = |name: String, source: Source, io: &Arc<TrackIo>| -> Result<(), LaunchError> {
                let handle = thread::Builder::new()
                    .name(name)
                    .spawn({
                        let io = io.clone();
                        let paused = paused.clone();
                        let stop = stop.clone();
                        move || track_main(source, io, paused, stop)
                    })
                    .map_err(|error| LaunchError {
                        code: protocol::ErrorCode::Internal,
                        message: format!("spawn: {error}"),
                    })?;
                handles.push(handle);
                Ok(())
            };
            if args.mic.enabled {
                let io = TrackIo::new(out.clone(), TrackName::Mic, args.frame_ms);
                spawn("acta-win-mic".into(), Source::Mic, &io)?;
                tracks.push(io);
            }
            if args.system.enabled {
                let io = TrackIo::new(out.clone(), TrackName::System, args.frame_ms);
                for pid in &args.system.pids {
                    let loopback_pid = u32::try_from(*pid).map_err(|_| LaunchError {
                        code: protocol::ErrorCode::InvalidArgs,
                        message: format!("pid {pid} fuera de rango"),
                    })?;
                    spawn(
                        format!("acta-win-loopback-{loopback_pid}"),
                        Source::ProcessLoopback { pid: loopback_pid },
                        &io,
                    )?;
                }
                tracks.push(io);
            }
            Ok(())
        })();
        if let Err(error) = spawned {
            stop.store(true, Ordering::Relaxed);
            for handle in handles.drain(..) {
                let _ = handle.join();
            }
            return Err(error);
        }
        Ok(SpawnedTracks {
            mic_device,
            system_pids: if args.system.enabled { args.system.pids.clone() } else { vec![] },
            controls: Box::new(RealControls { stop, paused, handles, tracks }),
        })
    }
}

struct RealControls {
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    handles: Vec<thread::JoinHandle<()>>,
    tracks: Vec<Arc<TrackIo>>,
}

impl TrackControls for RealControls {
    fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    fn stop_and_join(mut self: Box<Self>) {
        self.stop.store(true, Ordering::Relaxed);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
        // mac: flush parcial + stopped por pista, desde el servicio y tras las
        // pistas (el orden events-antes-de-respuesta lo impone el join).
        for track in &self.tracks {
            track.finish();
        }
    }
}

/// s16 o f32, N canales → mono de 16 kHz en s16. Igual que en el experimento:
/// 1) media de canales en f32 normalizado a [-1, 1]; 2) reescalado por
///    interpolación lineal a 16 kHz; 3) s16 con redondeo al entero más próximo.
fn convert_to_16k_mono_s16(input: &[u8], src_rate: u32, channels: u16, float_data: bool) -> Vec<i16> {
    let bytes_per_sample = if float_data { 4usize } else { 2usize };
    let frame_bytes = bytes_per_sample * channels.max(1) as usize;
    let frames = input.len() / frame_bytes;
    let mut mono: Vec<f32> = Vec::with_capacity(frames);
    for frame in 0..frames {
        let base = frame * frame_bytes;
        let mut sum = 0.0f32;
        for channel in 0..channels as usize {
            let offset = base + channel * bytes_per_sample;
            let value = if float_data {
                let bytes = [
                    input[offset],
                    input[offset + 1],
                    input[offset + 2],
                    input[offset + 3],
                ];
                f32::from_le_bytes(bytes)
            } else {
                i16::from_le_bytes([input[offset], input[offset + 1]]) as f32 / 32_768.0
            };
            sum += value;
        }
        mono.push(sum / channels.max(1) as f32);
    }

    let back = 32_767.0f32;
    let clamp = |value: f32| -> i16 { value.round().clamp(-32_768.0, back) as i16 };
    if src_rate == SAMPLE_RATE {
        return mono.iter().map(|&value| clamp(value * back)).collect();
    }
    let output_frames = (u64::try_from(frames).unwrap_or(0) * u64::from(SAMPLE_RATE) / u64::from(src_rate)) as usize;
    let step = f64::from(src_rate) / f64::from(SAMPLE_RATE);
    (0..output_frames)
        .map(|index| {
            let position = index as f64 * step;
            let whole = position.floor() as usize;
            let fraction = (position - whole as f64) as f32;
            let first = mono[whole.min(frames.saturating_sub(1))];
            let next = mono[(whole + 1).min(frames.saturating_sub(1))];
            let value = first + fraction * (next - first);
            clamp(value * back)
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod convert_tests {
    use super::*;

    fn s16(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    fn f32(samples: &[f32]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn mono_s16_at_16k_passes_through() {
        let input = s16(&[0, 100, -100, 50]);
        let output = convert_to_16k_mono_s16(&input, 16_000, 1, false);
        assert_eq!(output, vec![0, 100, -100, 50]);
    }

    #[test]
    fn stereo_s16_downmixes_to_mono() {
        let input = s16(&[100, -100, 200, 0]);
        let output = convert_to_16k_mono_s16(&input, 16_000, 2, false);
        assert_eq!(output, vec![0, 100]);
    }

    #[test]
    fn f32_stereo_downmixes_to_mono_in_range() {
        let input = f32(&[0.5, -0.5, 1.0, 0.0]);
        let output = convert_to_16k_mono_s16(&input, 16_000, 2, true);
        // 0.5 × 32 767 = 16 383.5 → redondeo del conversor al entero más próximo.
        assert_eq!(output, vec![0, 16_384]);
    }

    #[test]
    fn interpolates_from_24k_to_16k() {
        // Tres frames a 24 kHz → dos a 16 kHz; el segundo cae en posición 1.5
        // (media entre 800/32768 y 1600/32768).
        let input = s16(&[0, 800, 1600]);
        let output = convert_to_16k_mono_s16(&input, 24_000, 1, false);
        assert_eq!(output.len(), 2);
        assert_eq!(output[0], 0);
        assert_eq!(output[1], 1_200);
    }

    #[test]
    fn empty_input_stays_empty() {
        let output = convert_to_16k_mono_s16(&[], 48_000, 2, true);
        assert!(output.is_empty());
    }

    #[test]
    fn truncated_frame_is_dropped() {
        let input = s16(&[100]).into_iter().take(1).collect::<Vec<u8>>(); // 1 de 2 bytes
        let output = convert_to_16k_mono_s16(&input, 16_000, 1, false);
        assert!(output.is_empty());
    }

    #[test]
    fn mic_bytes_go_directly_to_s16() {
        // La ruta `direct` del micrófono no pasa por el conversor: los bytes
        // s16le del motor salen sin recorrer el normalizado (preserva -32768).
        let stream_bytes = s16(&[0, -32_768, 32_767, 123]);
        assert_eq!(
            samples_from_packet(&stream_bytes, SAMPLE_RATE, 1, false, true),
            vec![0, -32_768, 32_767, 123]
        );
        // Y la del loopback sí pasa por el conversor (misma media que antes).
        assert_eq!(
            samples_from_packet(&s16(&[100, -100]), 16_000, 2, false, false),
            vec![0]
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod track_io_tests {
    use super::*;

    /// Pista con canal acotado a 4 eventos y ensamblador de 100 ms.
    fn io() -> (Arc<TrackIo>, std::sync::mpsc::Receiver<Outgoing>) {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Outgoing>(4);
        let io = TrackIo::new(tx, TrackName::System, 100);
        (io, rx)
    }

    /// Hundir el canal para forzar entregas Full (cuentan como frames
    /// perdidos) y después vaciarlo, dejando sitio para los caminos terminales.
    fn fill_then_empty(io: &TrackIo, rx: &std::sync::mpsc::Receiver<Outgoing>) {
        // Cuatro fills (Sent, ocupan el canal) + uno más (Full → dropped = 1).
        for _ in 0..5 {
            io.push(&vec![0; 1_600]);
        }
        while rx.try_recv().is_ok() {}
    }

    fn event_data(outgoing: Outgoing) -> EventOut {
        match outgoing {
            Outgoing::Event(line) => line.event,
            Outgoing::Response(_) => panic!("respuesta inesperada"),
        }
    }

    /// Recibe el siguiente evento con plazo; un evento que no llega nunca es
    /// fallo del test, no un bloqueo.
    fn next_event(rx: &std::sync::mpsc::Receiver<Outgoing>) -> EventOut {
        event_data(rx.recv_timeout(std::time::Duration::from_secs(2)).expect("evento en plazo"))
    }

    /// Un fallo terminal no puede arrastrar el recuento de frames perdidos:
    /// el `audio.overrun` sale antes del estado `failed` de la pista.
    #[test]
    fn fail_reports_pending_dropped_frames_before_the_failed_state() {
        let (io, rx) = io();
        fill_then_empty(&io, &rx);
        io.fail("no data".into());
        let first = next_event(&rx);
        let second = next_event(&rx);
        match first {
            EventOut::AudioOverrun(o) => assert_eq!(o.dropped_frames, 1),
            other => panic!("primero debía ir el overrun, llegó {other:?}"),
        }
        match second {
            EventOut::AudioTrackState(s) => {
                assert_eq!(s.state, TrackStateName::Failed);
            }
            other => panic!("después debía ir el estado failed, llegó {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "sobran eventos tras el fallo");
    }

    /// Y lo mismo en la parada cuando no hay parcial que enviar: sin frames
    /// nuevos que «traigan» el overrun, el contador igual se vacía antes del
    /// estado `stopped`.
    #[test]
    fn finish_reports_pending_dropped_frames_before_the_stopped_state() {
        let (io, rx) = io();
        fill_then_empty(&io, &rx);
        io.finish();
        let first = next_event(&rx);
        let second = next_event(&rx);
        match first {
            EventOut::AudioOverrun(o) => assert_eq!(o.dropped_frames, 1),
            other => panic!("primero debía ir el overrun, llegó {other:?}"),
        }
        match second {
            EventOut::AudioTrackState(s) => {
                assert_eq!(s.state, TrackStateName::Stopped);
            }
            other => panic!("después debía ir el estado stopped, llegó {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "sobran eventos tras la parada");
    }

    /// Comportamiento ya fijado (se toca con `send_frame`, el mismo código que
    /// cambian los caminos terminales): el recuento de frames perdidos sale
    /// como `audio.overrun` en cuanto la entrega vuelve a tener sitio.
    #[test]
    fn dropped_frames_are_reported_when_delivery_recovers() {
        let (io, rx) = io();
        for _ in 0..4 {
            assert!(io.push(&vec![0; 1_600])); // Sent (ocupa el canal)
        }
        assert!(io.push(&vec![0; 1_600])); // Full → dropped = 1
        assert!(io.push(&vec![0; 1_600])); // Full → dropped = 2
        // Devolución con sitio: el frame entra y el recuento pendiente sale
        // detrás de él como overrun.
        for _ in 0..4 {
            assert!(
                rx.recv_timeout(std::time::Duration::from_secs(2)).is_ok(),
                "frames seq 0-3 en el canal"
            );
        }
        assert!(io.push(&vec![0; 1_600])); // Sent buf 0→1, luego overrun buf 2
        match next_event(&rx) {
            EventOut::AudioFrame(_) => {} // frame seq 6 (los perdidos, 4 y 5, no entran)
            other => panic!("se esperaba el frame seq 6, llegó {other:?}"),
        }
        let overrun = next_event(&rx);
        match overrun {
            EventOut::AudioOverrun(o) => assert_eq!(o.dropped_frames, 2),
            other => panic!("se esperaba el overrun diferido, llegó {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }
}
