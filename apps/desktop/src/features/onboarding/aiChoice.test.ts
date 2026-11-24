import { describe, expect, it } from "vitest";
import type { CliDetection } from "../../api";
import { defaultSettings, freshSettings } from "../../api/mock";
import { aiProblem, applyAi, initialAi, secretIdFor, type AiState } from "./aiChoice";

const llm = () => defaultSettings().llm;
const cli = (over: Partial<CliDetection> = {}): CliDetection => ({ claude: "/opt/homebrew/bin/claude", codex: null, opencode: null, claudeEnv: {}, codexEnv: {}, opencodeEnv: {}, ...over });
const ai = (over: Partial<AiState>): AiState => ({ ...initialAi(llm()), ...over });

describe("elección de IA en la bienvenida", () => {
  it("parte de lo que ya esté configurado", () => {
    expect(initialAi(llm()).choice).toBe("claudeCode");
    const none = { ...llm(), minutesProvider: null };
    expect(initialAi(none).choice).toBeNull();
  });

  it("en una instalación nueva no hay nada elegido, y solo se crea lo que se elige", () => {
    const fresh = freshSettings().llm;
    expect(initialAi(fresh).choice).toBeNull();
    expect(applyAi(fresh, initialAi(fresh), cli({ codex: "/x/codex" }))).toEqual(fresh);
    const out = applyAi(fresh, { ...initialAi(fresh), choice: "local", local: { kind: "ollama", model: "llama3.2:latest", baseUrl: "http://127.0.0.1:11434/v1" } }, cli());
    expect(out.providers).toHaveLength(1);
    expect(out.providers[0]).toMatchObject({ id: "ollama", enabled: true, kind: { model: "llama3.2:latest" } });
    expect(out).toMatchObject({ minutesProvider: "ollama", chatProvider: "ollama", namingProvider: "ollama" });
  });

  it("Claude Code: usa la ruta y las variables detectadas y se queda con todas las tareas", () => {
    const out = applyAi(llm(), ai({ choice: "claudeCode" }), cli({ claude: "/x/claude", claudeEnv: { CLAUDE_CONFIG_DIR: "/h/.c" } }));
    expect(out.providers).toHaveLength(1);
    expect(out.providers[0]!.kind).toMatchObject({ type: "claudeCli", binaryPath: "/x/claude", extraEnv: { CLAUDE_CONFIG_DIR: "/h/.c" } });
    expect(out).toMatchObject({ minutesProvider: "claude-cli", chatProvider: "claude-cli", namingProvider: "claude-cli" });
  });

  it("Codex: lo añade si no estaba", () => {
    const out = applyAi(llm(), ai({ choice: "codex" }), cli({ codex: "/x/codex" }));
    expect(out.providers.map((p) => p.id)).toEqual(["claude-cli", "codex-cli"]);
    expect(out.minutesProvider).toBe("codex-cli");
  });

  it("OpenCode: lo añade si no estaba y asigna las tareas", () => {
    const out = applyAi(llm(), ai({ choice: "openCode" }), cli({ opencode: "/x/opencode", opencodeModel: "github-copilot/gemini-4-neon" }));
    expect(out.providers.map((p) => p.id)).toEqual(["claude-cli", "opencode-cli"]);
    expect(out.providers[1]!.kind).toMatchObject({ type: "opencodeCli", binaryPath: "/x/opencode", model: "github-copilot/gemini-4-neon" });
    expect(out.minutesProvider).toBe("opencode-cli");
  });

  it("modelo local: el servidor y el modelo elegidos", () => {
    const out = applyAi(llm(), ai({ choice: "local", local: { kind: "lmStudio", model: "google/gemma-3-12b", baseUrl: "http://127.0.0.1:1234/v1" } }), null);
    expect(out.providers[1]).toMatchObject({ id: "lmstudio", enabled: true, kind: { baseUrl: "http://127.0.0.1:1234/v1", model: "google/gemma-3-12b", apiKey: null } });
    expect(out.chatProvider).toBe("lmstudio");
  });

  it("NaN Builders: la referencia al Llavero, nunca la clave", () => {
    const ref = { service: "acta", account: "provider:nan" };
    const out = applyAi(llm(), ai({ choice: "nan", nan: { apiKey: ref, model: "qwen3.6" } }), null);
    expect(out.providers[1]).toMatchObject({ id: "nan", kind: { baseUrl: "https://api.nan.builders/v1", model: "qwen3.6", apiKey: ref } });
    expect(out.namingProvider).toBe("nan");
    expect(secretIdFor(llm(), ai({ choice: "nan" }))).toBe("nan");
  });

  it("clave propia: OpenRouter, Anthropic o un servidor cualquiera", () => {
    const ref = { service: "acta", account: "provider:custom" };
    const custom = applyAi(llm(), ai({ choice: "own", own: { service: "custom", apiKey: ref, model: "m", baseUrl: "https://llm.example.com/v1" } }), null);
    expect(custom.providers[1]).toMatchObject({ id: "custom", kind: { baseUrl: "https://llm.example.com/v1", model: "m", apiKey: ref } });
    const anth = applyAi(llm(), ai({ choice: "own", own: { service: "anthropic", apiKey: { service: "acta", account: "provider:anthropic" }, model: "claude-x", baseUrl: "" } }), null);
    expect(anth.providers[1]).toMatchObject({ id: "anthropic", kind: { type: "anthropic", model: "claude-x" } });
    expect(secretIdFor(llm(), ai({ choice: "own", own: { service: "openRouter", apiKey: null, model: "", baseUrl: "" } }))).toBe("openrouter");
  });

  it("sin elegir nada no toca los ajustes", () => {
    const before = llm();
    expect(applyAi(before, ai({ choice: null }), cli())).toEqual(before);
  });

  it("dice qué falta para poder continuar", () => {
    expect(aiProblem(ai({ choice: null }), null, true)).toBeNull();
    expect(aiProblem(ai({ choice: "claudeCode" }), null, true)).toBeNull();
    expect(aiProblem(ai({ choice: "claudeCode" }), cli({ claude: null }), false)).toMatch(/Claude Code no está instalado/);
    expect(aiProblem(ai({ choice: "codex" }), cli(), false)).toMatch(/Codex no está instalado/);
    expect(aiProblem(ai({ choice: "local" }), null, false)).toMatch(/Elige un modelo/);
    expect(aiProblem(ai({ choice: "nan" }), null, false)).toMatch(/Guarda la clave/);
    expect(aiProblem(ai({ choice: "nan", nan: { apiKey: { service: "a", account: "b" }, model: "gemma4" } }), null, false)).toBeNull();
    expect(aiProblem(ai({ choice: "own", own: { service: "custom", apiKey: null, model: "m", baseUrl: "" } }), null, false)).toMatch(/URL/);
    expect(aiProblem(ai({ choice: "own", own: { service: "openAi", apiKey: { service: "a", account: "b" }, model: "", baseUrl: "" } }), null, false)).toMatch(/modelo/);
  });
});
