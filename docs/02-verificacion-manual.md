# Verificación manual

Checklist para una `.app` **sin firmar** instalada con `scripts/install-mac.sh`. Cada punto trae su resultado esperado tras la flecha.

## Siempre

- [ ] `scripts/verify.sh` pasa → sin errores.
- [ ] `cd sidecars/acta-mac && swift test --disable-get-task-allow-entitlement` → sin errores.
- [ ] Pruebas E2E reales (ver `docs/04-informe-pruebas.md`) → sin errores.
- [ ] 10 min grabando y `ps -axo ppid=,stat= | awk -v p="$(pgrep -x acta)" '$1==p && $2 ~ /Z/' | wc -l` → **0** zombis.
- [ ] `ps -ax | rg acta-mac` → exactamente **1** proceso, también tras varias reuniones.
- [ ] `kill -9` a la app → `acta-mac` desaparece en < 3 s.

## 1. Permisos (TCC) con app sin firmar

- [ ] Primer arranque: Micrófono, Audio del sistema → aviso, concedes, la app graba.
- [ ] Accesibilidad (manual) → los nombres de Teams aparecen.
- [ ] Automatización → aviso por navegador, Meet se detecta.
- [ ] Grabación de pantalla (Ajustes › Pantalla) → concedido solo si lo activas.
- [ ] Cerrar y abrir la app → los permisos persisten, sin avisos.
- [ ] **Reiniciar el Mac** → siguen concedidos.
- [ ] **Recompilar e instalar de nuevo** → anota si macOS vuelve a pedir cada permiso (micro, sistema, accesibilidad, automatización, pantalla). Si sí, es esperable (cdhash nuevo); regístralo.
- [ ] Denegar micro → aviso en la píldora en < 10 s (pista toda a ceros = TCC denegado).
- [ ] Denegar Audio del sistema → aviso equivalente para la pista "otros".

## 2. Captura de audio

- [ ] Teams: procesos auxiliares (helpers) incluidos en el tap → se oye a los demás en `system.wav`.
- [ ] **Prueba de la palmada** con altavoz: la palmada aparece en el mismo instante en `mic` y `system`.
- [ ] 5 min: un `seg-*.wav` cada 30 s por pista.
- [ ] AirPods: conectar a mitad de llamada → el audio sigue, evento `audio.device_changed`.
- [ ] AirPods: desconectar a mitad → vuelve al micro/altavoz del Mac sin cortarse.
- [ ] Pausa y reanudar → sin relleno de silencio en el audio, timeline continuo.
- [ ] Duración máxima configurada → la grabación se para al llegar.
- [ ] Poco disco → aviso de disco bajo.

## 3. Detección

- [ ] Entrar en reunión de Teams → píldora **"¿Grabar?"** expandida.
- [ ] Rechazar → no vuelve a preguntar por esa reunión.
- [ ] Ignorar → caduca por timeout y se oculta.
- [ ] Google Meet en **Chrome** → detectado.
- [ ] Google Meet en **Arc** → detectado.
- [ ] Google Meet en **Edge** → detectado.
- [ ] Google Meet en **Brave** → detectado.
- [ ] Google Meet en **Safari** → detectado.
- [ ] Dictar con Typeless, Orca y el dictado de macOS → **nunca** dispara detección.
- [ ] Ajustes › lista de apps excluidas: añadir una app → deja de disparar.
- [ ] Colgar → **"¿Parar?"** con cuenta atrás.
- [ ] Sin responder y con silencio → autoparada.
- [ ] Un solo aviso por reunión (sin duplicados).

## 4. Calibración AX de Teams

Lanza con `ACTA_AX_DUMP_DIR=<dir>` para volcar el árbol de accesibilidad. Usa `open` para que el proceso responsable de TCC siga siendo acta.app (si lanzas el binario desde la Terminal, los permisos se atribuyen a la Terminal):

```bash
mkdir -p ~/Desktop/ax-dump
open -a ~/Applications/acta.app --env ACTA_AX_DUMP_DIR="$HOME/Desktop/ax-dump"
```

- [ ] Teams en **español**: volcado con roster, hablante activo y presentador → ajustar patrones.
- [ ] Teams en **inglés**: lo mismo.
- [ ] Roster correcto → coincide con los participantes reales.
- [ ] Hablante activo → cambia al hablar otra persona.
- [ ] Presentador → se detecta cuando alguien comparte.
- [ ] Ventana de reunión → se detecta abrir y cerrar.

## 5. Robustez

- [ ] `kill -9` a la app a mitad de grabación y reabrir → la reunión se recupera y se procesa (≤ 1 s perdido).
- [ ] `kill` al proceso `acta-mac` en grabación → reinicio automático y aviso `sidecarRestarted`.
- [ ] Procesar una reunión no bloquea iniciar otra grabación.

## 6. Pantalla compartida (opt-in)

- [ ] Opt-in desactivado → no se captura nada.
- [ ] Cambio de diapositiva → se guarda una captura.
- [ ] Decir "importante" o "deadline" con la previsualización en vivo activa → captura por palabra clave.
- [ ] Las capturas aparecen enlazadas a temas de la escaleta y en la timeline.
- [ ] Borrar una captura → desaparece.

## 7. Modelos y procesado

- [ ] Ajustes › Modelos: descargar `asr`, `diarizer`, `vad` → progreso y estado "descargado".
- [ ] Reunión de **10 min**: tiempo, memoria y calidad de ASR/diarización razonables. Anota cifras.
- [ ] Reunión de **4 h**: termina sin agotar memoria; anota tiempo y pico de RAM.
- [ ] Tras procesar → `ps -ax | rg acta-mac` sigue en 1.
- [ ] Previsualización en vivo → aparece texto parcial mientras se graba.

## 8. Hablantes

- [ ] Solape con el hablante activo de Teams → nombres asignados.
- [ ] Sugerencia por huella de voz en una reunión nueva.
- [ ] Renombrar y marcar "recordar" → se aplica en reuniones futuras.

## 9. Actas, búsqueda y chat

- [ ] Acta con **Claude CLI** (proveedor por defecto) → resumen, decisiones y tareas.
- [ ] Acta con **Codex** → ídem.
- [ ] Acta con **Cursor** (`cursor-agent`) → ídem. «Reintentar con…» lista los modelos de tu cuenta.
- [ ] Acta con **Ollama** → ídem.
- [ ] Ajustes › Proveedores › Probar → OK con ruta `/opt/homebrew/bin/claude` y `CLAUDE_CONFIG_DIR`.
- [ ] Búsqueda → encuentra por texto del transcript.
- [ ] Chat → responde con citas que enlazan al momento.
- [ ] Sugerencias de grupos → aceptar y descartar funcionan.

## 10. Overlay y menú

- [ ] El overlay se ve sobre apps en **pantalla completa**.
- [ ] Se ve en **todos los Spaces**.
- [ ] **Nunca** roba el foco al escribir en otra app.
- [ ] Menú de la barra › **Grabar ahora** → inicia una grabación manual.
- [ ] El menú de la barra sigue el estado: en reposo solo «Grabar ahora»; grabando, «Grabando · mm:ss», «Pausar» y «Parar grabación».
- [ ] Con AirPods puestos, la voz de los demás se transcribe bien (no al doble de velocidad).
