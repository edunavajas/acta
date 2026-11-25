**Español** · [English](README.en.md)

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/brand/acta-horizontal-reversed.svg">
    <img src="docs/brand/acta-horizontal.svg" alt="acta" width="320">
  </picture>
</p>

<p align="center">
  <strong>Tú atiendes la reunión. acta escribe el acta.</strong><br>
  Grabador de reuniones para Mac que transcribe en tu máquina y te devuelve el resumen, las decisiones y <em>tus</em> tareas.
</p>

<p align="center">
  macOS 14.2+ · Apple Silicon · Tauri v2 (Rust + React) · sin cuenta, sin servidor propio
</p>

<p align="center">
  <a href="https://github.com/edunavajas/acta/releases/download/v0.1.0/acta-demo.mp4">
    <img src="docs/brand/video-poster.jpg" alt="Vídeo de presentación de acta: tú y los demás en la misma reunión, cada uno en su pista" width="860">
  </a><br>
  <a href="https://github.com/edunavajas/acta/releases/download/v0.1.0/acta-demo.mp4">▶ Ver el vídeo (2 min)</a>
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/oscuro/reunion-resumen.png">
    <img src="docs/screenshots/claro/reunion-resumen.png" alt="Una reunión en acta: resumen, decisiones y tareas con su minuto en el audio" width="860">
  </picture>
</p>

> Las capturas usan datos de demostración.

## Qué es

Muchas herramientas de actas de reunión mandan el audio a la nube de otra empresa. acta hace lo contrario:

- **Graba dos pistas separadas**: tu micrófono y el audio del sistema (los demás). Así sabe qué dijiste tú sin adivinar, y solo hay que diferenciar a los otros.
- **Es a prueba de cierres inesperados**: el audio se escribe por segmentos mientras graba. Si la app o el Mac mueren, la reunión se recupera al volver a abrir.
- **Transcribe y separa hablantes en tu Mac**, con modelos que corren en el Neural Engine. El audio no sale de tu disco.
- **Redacta el acta con el modelo que elijas**: uno local (nada sale de tu máquina) o uno en la nube con tu propia clave.

## Qué hace

| | |
|---|---|
| **Detecta la llamada** | Avisa con una pastilla flotante cuando empieza una reunión de Teams o Meet, y propone parar cuando termina. Si no respondes, para sola tras un rato de silencio. |
| **Transcript con hablantes** | Tú, más los demás con nombre: los saca de la interfaz de Teams y recuerda las voces entre reuniones (siempre confirmadas por ti). |
| **Acta** | Resumen, decisiones y action items. Separa **tus tareas** de las de los demás. |
| **Escaleta y línea de tiempo** | La reunión como árbol de temas, con salto al minuto exacto del audio. |
| **Capturas** | Imágenes de la pantalla compartida, opcionales y desactivadas por defecto. |
| **Grupos** | Agrupa reuniones a mano o deja que acta te sugiera grupos parecidos. |
| **Chat y búsqueda** | Pregunta sobre una reunión o sobre un grupo entero; búsqueda por texto y semántica en local. |

## Privacidad: qué sale de tu Mac y qué no

| Dato | ¿Sale de tu Mac? |
|---|---|
| Audio | **No.** Se queda en `~/Library/Application Support/acta`. |
| Transcripción y separación de hablantes | **No.** Se hacen en local. |
| Texto de la transcripción (para redactar el acta y el chat) | **Depende del proveedor.** Con un modelo local (modelo integrado, Ollama, LM Studio) no sale. Con Anthropic, OpenAI, OpenRouter u otro servicio en la nube, el texto se envía a ese servicio. |
| Descarga de modelos | Sí, la primera vez (modelos de voz, de búsqueda y el modelo local integrado, si lo usas). Después, la transcripción funciona sin conexión. |

## Instalación

### Homebrew (recomendado)

```bash
brew tap edunavajas/acta https://github.com/edunavajas/acta
brew install --cask edunavajas/acta/acta
```

Para actualizar: `brew update && brew upgrade --cask acta`.

### Descarga directa

Baja `acta-<versión>-macos-arm64.zip` de la [última release](https://github.com/edunavajas/acta/releases/latest), descomprímelo y arrastra `acta.app` a Aplicaciones.

### Primera apertura (Gatekeeper)

acta no está firmada ni notarizada, porque el proyecto no tiene cuenta de Apple Developer. Por eso macOS la bloquea la primera vez que la abres si la has descargado (también con Homebrew). Solo hay que hacerlo una vez:

- Ajustes del Sistema › Privacidad y seguridad › **Abrir igualmente**, o
- en terminal: `xattr -dr com.apple.quarantine /Applications/acta.app`

### Compilar desde el código

Si prefieres compilarla tú, un solo script lo hace en unos minutos.

**Requisitos**

| Qué | Versión |
|---|---|
| macOS | 14.2 o superior, Apple Silicon |
| Xcode Command Line Tools | con Swift 6 (`swift --version`) |
| Rust | 1.91 o superior (`rustup`) |
| Node y pnpm | pnpm 11 |

**Pasos**

```bash
git clone https://github.com/edunavajas/acta.git
cd acta
scripts/install-mac.sh            # instala en ~/Applications/acta.app
# scripts/install-mac.sh --system # o en /Applications/acta.app
```

Para **actualizar**: `git pull` y volver a ejecutar el script. Como la compilas tú, macOS no la pone en cuarentena y debería abrir directamente.

Detalle del script, permisos y por qué no se firma: [docs/03-instalacion.md](docs/03-instalacion.md). Cómo se publica una release: [docs/RELEASING.md](docs/RELEASING.md).

## Primer arranque

1. **Concede los permisos** cuando macOS los pida: micrófono y audio del sistema. La **Accesibilidad** (nombres de Teams) se concede a mano en Ajustes del Sistema.
2. **Descarga los modelos** en Ajustes › Modelos (voz, separación de hablantes y detección de voz).
3. **Elige quién redacta el acta** en Ajustes › Proveedores:
   - modelo local integrado, [Ollama](https://ollama.com) o [LM Studio](https://lmstudio.ai), para no enviar nada fuera;
   - [Anthropic](https://www.anthropic.com), [OpenAI](https://openai.com), [OpenRouter](https://openrouter.ai) o cualquier servidor compatible con la API de OpenAI, con tu clave;
   - el CLI de Claude Code, Codex u OpenCode que ya tengas instalado. acta lo ejecuta tal cual, sin tocar sus credenciales: comprueba que los términos de tu proveedor permiten ese uso.

## Cómo está hecho

Arquitectura hexagonal en Rust: el dominio no hace E/S y todo lo que toca el sistema entra por puertos.

| Carpeta | Qué es |
|---|---|
| `crates/acta-core` | Dominio, puertos y máquina de estados (sin E/S) |
| `crates/acta-audio` | Escritura WAV por segmentos, recuperación, autoparada, supervisor y procesado |
| `crates/acta-store` | SQLite + FTS5 + sqlite-vec, búsqueda híbrida, grupos |
| `crates/acta-llm` | Proveedores de LLM, generación del acta y chat |
| `crates/acta-mac-bridge` | Cliente Rust del sidecar Swift |
| `sidecars/acta-mac` | Sidecar Swift: captura de audio, micrófono, Accessibility, ScreenCaptureKit, FluidAudio |
| `apps/desktop` | App Tauri v2 (React + TypeScript): ventana principal y pastilla flotante |

Para comprobar que todo compila y pasa los tests:

```sh
scripts/verify.sh
```

Lanza los guards, `cargo check`, clippy, los tests de Rust, `swift build`, `tsc` y `vitest`.

## Documentación

- [docs/00-analisis.md](docs/00-analisis.md): análisis y decisión de base
- [docs/01-arquitectura.md](docs/01-arquitectura.md): arquitectura, contratos y plan de implementación
- [docs/02-verificacion-manual.md](docs/02-verificacion-manual.md): pruebas manuales
- [docs/03-instalacion.md](docs/03-instalacion.md): instalación, permisos y configuración
- [docs/RELEASING.md](docs/RELEASING.md): cómo se publica una release y el cask de Homebrew
- [docs/referencia-meetily-zombis.md](docs/referencia-meetily-zombis.md): el bug de procesos zombi que nunca debemos repetir
- [docs/brand/GUIA-DE-USO.md](docs/brand/GUIA-DE-USO.md): guía de uso del logo

## Estado

Versión **0.1.0**, en desarrollo. Solo macOS por ahora; Windows está previsto más adelante.

## Licencia

acta es software libre bajo la [GNU GPL v3.0 o posterior](LICENSE). Puedes usarlo, estudiarlo, modificarlo y redistribuirlo. Si distribuyes una versión modificada, tienes que publicarla bajo la misma licencia, con su código fuente.

Los modelos de voz y de lenguaje que acta descarga tienen sus propias licencias.

Los logotipos de terceros que aparecen en la app pertenecen a sus dueños: [NOTICE](apps/desktop/src/assets/logos/NOTICE.md).
