import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { createMockApi } from "../../api";
import { freshSettings } from "../../api/mock";
import { ApiProvider } from "../../app/ApiContext";
import { App } from "../../app/App";
import { NAN_REFERRAL_URL } from "../providers/presets";
import { Onboarding } from "./Onboarding";

type Seed = Parameters<typeof createMockApi>[0];

/** Primer arranque: instalación nueva, sin nombre ni proveedor, y la bienvenida sin ver. */
async function firstRun(seed: Seed = {}, prepare?: (api: ReturnType<typeof createMockApi>["api"]) => void) {
  const mock = createMockApi({ settings: freshSettings(), ...seed });
  prepare?.(mock.api);
  render(<App api={mock.api} />);
  const dialog = await screen.findByRole("dialog", { name: "Bienvenida a acta" });
  return { ...mock, dialog };
}
const next = (name = "Continuar") => userEvent.click(screen.getByRole("button", { name }));
/** Avanza desde la bienvenida hasta el paso `n` (1 = Bienvenida, 2 = Sobre ti, 3 = Permisos, 4 = Transcripción, 5 = IA, 6 = Detección, 7 = Listo). */
async function goToStep(n: number) {
  if (n > 1) await next("Empezar");
  for (let i = 2; i < n; i += 1) await next();
}
const OLLAMA = {
  ollama: { running: true, baseUrl: "http://127.0.0.1:11434/v1", models: ["gemma3:12b", "llama3.2:latest"] },
  lmStudio: { running: false, baseUrl: "http://127.0.0.1:1234/v1", models: [] },
};

describe("bienvenida de primer arranque", () => {
  it("no aparece si ya se vio", async () => {
    render(<App api={createMockApi().api} />);
    await screen.findByRole("button", { name: /Daily del equipo/ });
    expect(screen.queryByRole("dialog", { name: "Bienvenida a acta" })).not.toBeInTheDocument();
  });

  it("explica qué hace acta y que todo se queda en tu Mac", async () => {
    const { dialog } = await firstRun();
    expect(within(dialog).getByRole("heading", { name: "Te damos la bienvenida a acta" })).toBeInTheDocument();
    expect(within(dialog).getByText(/Se queda en tu Mac/)).toBeInTheDocument();
    const steps = within(dialog).getByRole("list", { name: "Pasos" });
    expect(within(steps).getAllByRole("listitem").map((li) => li.textContent)).toEqual(["Bienvenida", "Sobre ti", "Permisos", "Transcripción", "Inteligencia artificial", "Detección", "Listo"]);
    expect(within(steps).getByText("Bienvenida").closest("li")).toHaveAttribute("aria-current", "step");
  });

  it("se puede saltar y no vuelve a salir", async () => {
    const { dialog, api, controls } = await firstRun();
    await userEvent.click(within(dialog).getByRole("button", { name: "Saltar" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Bienvenida a acta" })).not.toBeInTheDocument());
    expect(controls.state.settings.onboarding).toEqual({ completed: true });
    render(<App api={api} />);
    await screen.findAllByRole("button", { name: /Daily del equipo/ });
    expect(screen.queryByRole("dialog", { name: "Bienvenida a acta" })).not.toBeInTheDocument();
  });

  it("permisos: con dos navegadores abiertos, cada fila de Automatización dice para cuál es", async () => {
    const base = createMockApi().controls.state.permissions.filter((p) => p.kind !== "automation");
    const { dialog } = await firstRun({
      permissions: [
        ...base,
        { kind: "automation", status: "notDetermined", target: "com.google.Chrome" },
        { kind: "automation", status: "notDetermined", target: "com.brave.Browser" },
      ],
    });
    await goToStep(3);
    const rows = await within(dialog).findAllByRole("listitem", { name: /^Automatización/ });
    expect(rows.map((li) => li.getAttribute("aria-label"))).toEqual(["Automatización (Google Chrome)", "Automatización (Brave)"]);
    expect(within(rows[0]!).getByText(/Se pide para Google Chrome\./)).toBeInTheDocument();
    expect(within(rows[1]!).getByText(/Se pide para Brave\./)).toBeInTheDocument();
  });

  it("permisos: dice para qué es cada uno, su estado al momento y cómo concederlo", async () => {
    const { dialog, api, controls } = await firstRun();
    const request = vi.spyOn(api, "permissionsRequest");
    const open = vi.spyOn(api, "openSystemSettings");
    await goToStep(3);
    expect(within(dialog).getByRole("heading", { name: "Permisos de macOS" })).toBeInTheDocument();
    const mic = await within(dialog).findByRole("listitem", { name: "Micrófono" });
    expect(within(mic).getByText("Para grabar tu voz.")).toBeInTheDocument();
    expect(within(mic).getByText("Concedido")).toBeInTheDocument();
    expect(within(dialog).getAllByRole("listitem").map((li) => li.getAttribute("aria-label"))).toEqual(
      expect.arrayContaining(["Micrófono", "Audio del sistema", "Accesibilidad", "Automatización (Google Chrome)", "Grabación de pantalla"]),
    );
    const screenRec = within(dialog).getByRole("listitem", { name: "Grabación de pantalla" });
    expect(within(screenRec).getByText("Opcional")).toBeInTheDocument();
    const system = within(dialog).getByRole("listitem", { name: "Audio del sistema" });
    await userEvent.click(within(system).getByRole("button", { name: "Conceder" }));
    expect(request).toHaveBeenCalledWith("systemAudio");
    await userEvent.click(within(system).getByRole("button", { name: "Abrir Ajustes del Sistema" }));
    expect(open).toHaveBeenCalledWith("systemAudio");
    // Al volver de Ajustes del Sistema el estado se actualiza solo.
    controls.state.permissions = controls.state.permissions.map((p) => (p.kind === "systemAudio" ? { ...p, status: "granted" } : p));
    act(() => void window.dispatchEvent(new Event("focus")));
    await waitFor(() => expect(within(system).getByText("Concedido")).toBeInTheDocument());
  });

  it("transcripción: descarga los modelos que faltan y enseña el progreso", async () => {
    const { dialog, api, controls } = await firstRun();
    const download = vi.spyOn(api, "modelsDownload").mockResolvedValue();
    await goToStep(4);
    expect(within(dialog).getByRole("heading", { name: "Transcripción en tu Mac" })).toBeInTheDocument();
    await within(dialog).findByRole("listitem", { name: "Separar hablantes" });
    await userEvent.click(within(dialog).getByRole("button", { name: "Descargar todo" }));
    expect(download.mock.calls.map((c) => c[0])).toEqual(["diarizer", "vad"]);
    act(() => controls.emit("model:download", { name: "diarizer", fraction: 0.5, done: false, error: null }));
    expect(await within(dialog).findByRole("progressbar", { name: "Descargando Separar hablantes" })).toHaveAttribute("aria-valuenow", "50");
  });

  it("IA: reconoce el Claude Code de este Mac y lo deja configurado", async () => {
    const { dialog, controls } = await firstRun({}, (api) => {
      vi.spyOn(api, "llmCliDetect").mockResolvedValue({ claude: "/x/claude", codex: null, opencode: null, claudeEnv: { CLAUDE_CONFIG_DIR: "/h/.c" }, codexEnv: {}, opencodeEnv: {} });
    });
    await goToStep(5);
    expect(within(dialog).getByRole("heading", { name: "¿Quién escribe tus actas?" })).toBeInTheDocument();
    const group = within(dialog).getByRole("radiogroup", { name: "Proveedor de IA" });
    const claude = within(group).getByRole("radio", { name: /Claude Code/ });
    expect(await within(claude).findByText("Detectado en este Mac")).toBeInTheDocument();
    expect(within(within(group).getByRole("radio", { name: /Codex/ })).getByText("No está instalado")).toBeInTheDocument();
    // Detectado no es elegido: nada viene marcado.
    expect(claude).toHaveAttribute("aria-checked", "false");
    expect(within(group).queryByRole("radio", { checked: true })).not.toBeInTheDocument();
    await userEvent.click(claude);
    await next();
    await waitFor(() => expect(controls.state.settings.llm.providers[0]?.kind).toMatchObject({ binaryPath: "/x/claude", extraEnv: { CLAUDE_CONFIG_DIR: "/h/.c" } }));
    expect(controls.state.settings.llm).toMatchObject({ minutesProvider: "claude-cli", chatProvider: "claude-cli", namingProvider: "claude-cli" });
    expect(controls.state.settings.llm.providers.map((p) => p.id)).toEqual(["claude-cli"]);
  });

  it("IA: no hay nada activado de antemano y se puede seguir sin elegir", async () => {
    const { dialog, controls } = await firstRun({ local: OLLAMA });
    await goToStep(5);
    const group = within(dialog).getByRole("radiogroup", { name: "Proveedor de IA" });
    await within(within(group).getByRole("radio", { name: /Claude Code/ })).findByText("Detectado en este Mac");
    expect(within(group).queryByRole("radio", { checked: true })).not.toBeInTheDocument();
    // Lo detectado se enseña: Ollama está en marcha con dos modelos.
    expect(await within(within(group).getByRole("radio", { name: /Ollama o LM Studio/ })).findByText("Ollama en marcha · 2 modelos")).toBeInTheDocument();
    expect(within(dialog).getByText(/Sin proveedor hay transcripción, pero no acta ni chat/)).toBeInTheDocument();
    await next();
    expect(await within(dialog).findByRole("heading", { name: "Que empiece y pare sola" })).toBeInTheDocument();
    expect(controls.state.settings.llm).toEqual({ providers: [], minutesProvider: null, chatProvider: null, namingProvider: null });
  });

  it("IA: una elección a medias no atrapa, se puede seguir sin proveedor", async () => {
    const { dialog, controls } = await firstRun();
    await goToStep(5);
    await userEvent.click(within(dialog).getByRole("radio", { name: /NaN Builders/ }));
    expect(screen.getByRole("button", { name: "Continuar" })).toBeDisabled();
    await userEvent.click(within(dialog).getByRole("button", { name: "Seguir sin proveedor" }));
    expect(within(dialog).queryByRole("radio", { checked: true })).not.toBeInTheDocument();
    await next();
    await waitFor(() => expect(within(dialog).getByRole("heading", { name: "Que empiece y pare sola" })).toBeInTheDocument());
    expect(controls.state.settings.llm.providers).toEqual([]);
  });

  it("IA: un modelo local detectado en este Mac", async () => {
    const { dialog, controls } = await firstRun({ local: OLLAMA });
    await goToStep(5);
    await userEvent.click(within(dialog).getByRole("radio", { name: /Ollama o LM Studio/ }));
    expect(screen.getByRole("button", { name: "Continuar" })).toBeDisabled();
    const ollama = await within(dialog).findByRole("listitem", { name: "Ollama" });
    await userEvent.selectOptions(await within(ollama).findByRole("combobox", { name: "Modelo de Ollama" }), "llama3.2:latest");
    await userEvent.click(within(ollama).getByRole("button", { name: "Elegir Ollama" }));
    await next();
    await waitFor(() => expect(controls.state.settings.llm).toMatchObject({ minutesProvider: "ollama", chatProvider: "ollama", namingProvider: "ollama" }));
    expect(controls.state.settings.llm.providers.find((p) => p.id === "ollama")?.kind).toMatchObject({ model: "llama3.2:latest", baseUrl: "http://127.0.0.1:11434/v1" });
  });

  it("IA: sin servidor local, enseña cómo instalarlo", async () => {
    const { dialog } = await firstRun();
    await goToStep(5);
    await userEvent.click(within(dialog).getByRole("radio", { name: /Ollama o LM Studio/ }));
    const ollama = await within(dialog).findByRole("listitem", { name: "Ollama" });
    expect(await within(ollama).findByText("No está en marcha")).toBeInTheDocument();
    expect(within(ollama).getByRole("button", { name: "Cómo instalar Ollama" })).toBeInTheDocument();
  });

  it("IA: NaN Builders sugerido por privacidad, con la clave en el Llavero", async () => {
    const { dialog, api, controls } = await firstRun({ remoteModels: { "https://api.nan.builders/v1": ["deepseek-v4-flash", "gemma4", "qwen3.6"] } });
    const secret = vi.spyOn(api, "secretSet");
    await goToStep(5);
    const nan = within(dialog).getByRole("radio", { name: /NaN Builders/ });
    expect(within(nan).getByText("Recomendado por privacidad")).toBeInTheDocument();
    await userEvent.click(nan);
    expect(within(dialog).getByText(/sin registros de prompts ni respuestas/)).toBeInTheDocument();
    expect(within(dialog).getByText(/requiere el plan con API/)).toBeInTheDocument();
    await userEvent.click(within(dialog).getByRole("button", { name: "Crear cuenta" }));
    expect(controls.state.opened).toEqual([NAN_REFERRAL_URL]);
    expect(screen.getByRole("button", { name: "Continuar" })).toBeDisabled();
    await userEvent.type(within(dialog).getByLabelText("Clave de API de NaN Builders"), "sk-nan");
    await userEvent.click(within(dialog).getByRole("button", { name: "Guardar en el Llavero NaN Builders" }));
    await waitFor(() => expect(secret).toHaveBeenCalledWith("nan", "sk-nan"));
    expect(await within(dialog).findByRole("combobox", { name: "Modelo de NaN Builders" })).toHaveValue("gemma4");
    await next();
    await waitFor(() => expect(controls.state.settings.llm.minutesProvider).toBe("nan"));
    expect(controls.state.settings.llm.providers.find((p) => p.id === "nan")?.kind).toMatchObject({ baseUrl: "https://api.nan.builders/v1", model: "gemma4", apiKey: { service: "acta", account: "provider:nan" } });
    expect(JSON.stringify(controls.state.settings)).not.toContain("sk-nan");
  });

  it("IA: con mi propia clave (OpenRouter)", async () => {
    const { dialog, controls } = await firstRun({ remoteModels: { "https://openrouter.ai/api/v1": ["meta/llama-4"] } });
    await goToStep(5);
    await userEvent.click(within(dialog).getByRole("radio", { name: /Mi propia clave/ }));
    await userEvent.selectOptions(within(dialog).getByRole("combobox", { name: "Servicio" }), "openRouter");
    await userEvent.type(within(dialog).getByLabelText("Clave de API de OpenRouter"), "sk-or");
    await userEvent.click(within(dialog).getByRole("button", { name: "Guardar en el Llavero OpenRouter" }));
    expect(await within(dialog).findByRole("combobox", { name: "Modelo de OpenRouter" })).toHaveValue("meta/llama-4");
    await next();
    await waitFor(() => expect(controls.state.settings.llm.minutesProvider).toBe("openrouter"));
  });

  it("IA: prueba la conexión con lo elegido", async () => {
    const { dialog, api } = await firstRun();
    const test = vi.spyOn(api, "llmProviderTest");
    await goToStep(5);
    const claude = await within(dialog).findByRole("radio", { name: /Claude Code/ });
    await within(claude).findByText("Detectado en este Mac");
    await userEvent.click(claude);
    await userEvent.click(within(dialog).getByRole("button", { name: "Probar conexión" }));
    expect(await within(dialog).findByText(/OK \(mock\)/)).toBeInTheDocument();
    expect(test).toHaveBeenCalledWith("claude-cli");
  });

  it("detección y autoparada: valores sensatos que se pueden cambiar", async () => {
    const { dialog, controls } = await firstRun();
    await goToStep(6);
    expect(within(dialog).getByRole("heading", { name: "Que empiece y pare sola" })).toBeInTheDocument();
    const detect = within(dialog).getByRole("checkbox", { name: "Avisarme cuando empiece una reunión" });
    expect(detect).toBeChecked();
    expect(within(dialog).getByLabelText("Minutos de silencio antes de preguntar")).toHaveValue(3);
    expect(within(dialog).getByLabelText("Minutos sin respuesta antes de parar")).toHaveValue(2);
    await userEvent.click(detect);
    await userEvent.click(within(dialog).getByRole("button", { name: "Aumentar: Minutos de silencio antes de preguntar" }));
    await next();
    await waitFor(() => expect(controls.state.settings.detection.enabled).toBe(false));
    expect(controls.state.settings.autoStop.silencePromptAfterMs).toBe(240_000);
  });

  it("al terminar resume cómo ha quedado y no vuelve a salir", async () => {
    const { dialog, controls } = await firstRun();
    await goToStep(5);
    await userEvent.click(await within(dialog).findByRole("radio", { name: /Claude Code/ }));
    await next();
    await next();
    expect(await within(dialog).findByRole("heading", { name: "Todo listo" })).toBeInTheDocument();
    const summary = within(dialog).getByRole("list", { name: "Resumen" });
    expect(within(summary).getByText(/Actas con Claude Code/)).toBeInTheDocument();
    expect(within(summary).getByText(/Falta el permiso de audio del sistema/)).toBeInTheDocument();
    await next("Empezar a usar acta");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Bienvenida a acta" })).not.toBeInTheDocument());
    expect(controls.state.settings.onboarding).toEqual({ completed: true });
  });

  it("sin elegir proveedor, el resumen dice claro que habrá transcripción pero no acta", async () => {
    const { dialog, controls } = await firstRun();
    await goToStep(7);
    const summary = await within(dialog).findByRole("list", { name: "Resumen" });
    expect(within(summary).getByText("Sin proveedor de IA: habrá transcripción, pero no acta ni chat")).toBeInTheDocument();
    await next("Empezar a usar acta");
    await waitFor(() => expect(controls.state.settings.onboarding).toEqual({ completed: true }));
    expect(controls.state.settings.llm).toEqual({ providers: [], minutesProvider: null, chatProvider: null, namingProvider: null });
  });

  it("saltar la bienvenida no deja ningún proveedor ni nombre puestos", async () => {
    const { dialog, controls } = await firstRun();
    await userEvent.click(within(dialog).getByRole("button", { name: "Saltar" }));
    await waitFor(() => expect(controls.state.settings.onboarding).toEqual({ completed: true }));
    expect(controls.state.settings.llm).toEqual({ providers: [], minutesProvider: null, chatProvider: null, namingProvider: null });
    expect(controls.state.settings.user).toEqual({ displayName: "", aliases: [] });
    expect(controls.state.settings.transcription.language).toBe("auto");
  });

  describe("Sobre ti", () => {
    it("propone tu nombre de la cuenta de macOS si aún no hay ninguno, y dice de dónde sale", async () => {
      const { dialog, controls } = await firstRun();
      await goToStep(2);
      expect(within(dialog).getByRole("heading", { name: "Sobre ti" })).toBeInTheDocument();
      expect(within(dialog).getByText(/para saber qué tareas y menciones son tuyas/)).toBeInTheDocument();
      expect(within(dialog).getByText(/no sale de él, salvo dentro del texto de la reunión que se envía al proveedor de IA que tú elijas/)).toBeInTheDocument();
      const name = within(dialog).getByLabelText("Tu nombre");
      expect(name).toHaveValue("Álex Romero");
      expect(within(dialog).getByText("Lo hemos tomado de tu cuenta de macOS")).toBeInTheDocument();
      // Solo es una propuesta: hasta que no se continúa no se guarda nada.
      expect(controls.state.settings.user.displayName).toBe("");
      await userEvent.type(name, " Gil");
      expect(within(dialog).queryByText("Lo hemos tomado de tu cuenta de macOS")).not.toBeInTheDocument();
      await next();
      await waitFor(() => expect(controls.state.settings.user.displayName).toBe("Álex Romero Gil"));
    });

    it("si ya tenías nombre no lo pisa con el de macOS", async () => {
      const { dialog } = await firstRun({ settings: { ...freshSettings(), user: { displayName: "Alejandra", aliases: ["Ale"] } } });
      await goToStep(2);
      expect(within(dialog).getByLabelText("Tu nombre")).toHaveValue("Alejandra");
      expect(within(dialog).queryByText("Lo hemos tomado de tu cuenta de macOS")).not.toBeInTheDocument();
      expect(within(dialog).getByRole("button", { name: "Quitar «Ale»" })).toBeInTheDocument();
    });

    it("también te llaman: fichas que se añaden y se quitan, con tu nombre de pila a un toque", async () => {
      const { dialog, controls } = await firstRun();
      await goToStep(2);
      await userEvent.click(within(dialog).getByRole("button", { name: "Añadir «Álex»" }));
      expect(within(dialog).queryByRole("button", { name: "Añadir «Álex»" })).not.toBeInTheDocument();
      const aliases = within(dialog).getByLabelText("También te llaman");
      await userEvent.type(aliases, "Ale{Enter}Romero,");
      await userEvent.click(within(dialog).getByRole("button", { name: "Quitar «Ale»" }));
      await next();
      await waitFor(() => expect(controls.state.settings.user).toEqual({ displayName: "Álex Romero", aliases: ["Álex", "Romero"] }));
    });

    it("se puede seguir sin nombre, con un aviso de lo que se pierde", async () => {
      const { dialog, controls } = await firstRun({ systemProfile: { fullName: null, preferredLanguages: [], meetingLanguage: "auto" } });
      await goToStep(2);
      expect(within(dialog).getByLabelText("Tu nombre")).toHaveValue("");
      expect(within(dialog).getByText(/Sin tu nombre, acta no podrá saber qué tareas son tuyas/)).toBeInTheDocument();
      expect(within(dialog).queryByRole("button", { name: /^Añadir «/ })).not.toBeInTheDocument();
      await next();
      expect(await within(dialog).findByRole("heading", { name: "Permisos de macOS" })).toBeInTheDocument();
      expect(controls.state.settings.user).toEqual({ displayName: "", aliases: [] });
    });

    it("idioma de las reuniones: propone el del Mac, explica para qué sirve y se guarda", async () => {
      const { dialog, controls } = await firstRun();
      await goToStep(2);
      const language = within(dialog).getByLabelText("Idioma de las reuniones");
      expect(language).toHaveValue("es");
      expect(within(language).getAllByRole("option").map((o) => o.textContent)).toEqual(["Automático (detectar)", "Español", "English", "Català", "Français", "Deutsch", "Italiano", "Português"]);
      expect(within(dialog).getByText("Lo hemos tomado de los idiomas de tu Mac")).toBeInTheDocument();
      expect(within(dialog).getByText(/Dirige la transcripción y el idioma del acta/)).toBeInTheDocument();
      expect(within(dialog).getByText(/La interfaz de acta está, por ahora, en español/)).toBeInTheDocument();
      expect(controls.state.settings.transcription.language).toBe("auto");
      await userEvent.selectOptions(language, "ca");
      await next();
      await waitFor(() => expect(controls.state.settings.transcription.language).toBe("ca"));
    });

    it("el idioma que ya habías elegido no se cambia por el del Mac", async () => {
      const fresh = freshSettings();
      const { dialog } = await firstRun({ settings: { ...fresh, transcription: { ...fresh.transcription, language: "en" } } });
      await goToStep(2);
      expect(within(dialog).getByLabelText("Idioma de las reuniones")).toHaveValue("en");
      expect(within(dialog).queryByText("Lo hemos tomado de los idiomas de tu Mac")).not.toBeInTheDocument();
    });

    it("al volver a ver la bienvenida, «automático» es una elección y se respeta", async () => {
      render(<App api={createMockApi().api} />);
      await userEvent.click(within(screen.getByRole("navigation", { name: "Secciones" })).getByRole("button", { name: "Ajustes" }));
      await userEvent.click(await screen.findByRole("button", { name: "Volver a ver la bienvenida" }));
      const dialog = await screen.findByRole("dialog", { name: "Bienvenida a acta" });
      await userEvent.click(within(dialog).getByRole("button", { name: "Empezar" }));
      expect(within(dialog).getByLabelText("Idioma de las reuniones")).toHaveValue("auto");
      expect(within(dialog).getByLabelText("Tu nombre")).toHaveValue("Álex Romero");
      expect(within(dialog).queryByText("Lo hemos tomado de tu cuenta de macOS")).not.toBeInTheDocument();
    });

    it("si macOS no dice quién eres, la bienvenida sigue funcionando", async () => {
      const { dialog } = await firstRun({}, (api) => {
        vi.spyOn(api, "systemProfile").mockRejectedValue({ code: "io", message: "sin perfil" });
      });
      await goToStep(2);
      expect(within(dialog).getByLabelText("Tu nombre")).toHaveValue("");
      expect(within(dialog).getByLabelText("Idioma de las reuniones")).toHaveValue("auto");
    });
  });

  it("si los ajustes no se pueden leer, lo dice y deja reintentar en vez de no aparecer", async () => {
    const mock = createMockApi({ settings: freshSettings() });
    vi.spyOn(mock.api, "settingsGet").mockRejectedValueOnce({ code: "io", message: "disco lleno" });
    const onClose = vi.fn();
    render(
      <ApiProvider api={mock.api}>
        <Onboarding onClose={onClose} />
      </ApiProvider>,
    );
    const dialog = await screen.findByRole("dialog", { name: "Bienvenida a acta" });
    expect(within(dialog).getByRole("alert")).toHaveTextContent("No se han podido leer los ajustes: disco lleno");
    await userEvent.click(within(dialog).getByRole("button", { name: "Reintentar" }));
    expect(await screen.findByRole("heading", { name: "Te damos la bienvenida a acta" })).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
  });

  it("permisos: si pedir uno falla, lo dice", async () => {
    const { dialog, api } = await firstRun();
    vi.spyOn(api, "permissionsRequest").mockRejectedValue({ code: "io", message: "el ayudante no responde" });
    await goToStep(3);
    const system = await within(dialog).findByRole("listitem", { name: "Audio del sistema" });
    await userEvent.click(within(system).getByRole("button", { name: "Conceder" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("No se ha podido pedir el permiso: el ayudante no responde");
  });

  it("transcripción: si no se puede saber qué modelos hay, lo dice y deja reintentar", async () => {
    const { dialog } = await firstRun({}, (api) => {
      vi.spyOn(api, "modelsStatus").mockRejectedValueOnce({ code: "io", message: "sin carpeta de modelos" });
    });
    await goToStep(4);
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("sin carpeta de modelos");
    await userEvent.click(within(dialog).getByRole("button", { name: "Reintentar" }));
    expect(await within(dialog).findByRole("listitem", { name: "Separar hablantes" })).toBeInTheDocument();
  });

  it("se puede volver atrás", async () => {
    const { dialog } = await firstRun();
    await goToStep(4);
    await userEvent.click(within(dialog).getByRole("button", { name: "Atrás" }));
    expect(within(dialog).getByRole("heading", { name: "Permisos de macOS" })).toBeInTheDocument();
  });

  it("se puede volver a ver desde Ajustes › General", async () => {
    render(<App api={createMockApi().api} />);
    await userEvent.click(within(screen.getByRole("navigation", { name: "Secciones" })).getByRole("button", { name: "Ajustes" }));
    await userEvent.click(await screen.findByRole("button", { name: "Volver a ver la bienvenida" }));
    expect(await screen.findByRole("dialog", { name: "Bienvenida a acta" })).toBeInTheDocument();
  });
});
