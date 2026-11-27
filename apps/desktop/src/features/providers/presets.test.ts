import { describe, expect, it } from "vitest";
import type { ProviderConfig, Settings } from "../../api";
import { defaultSettings } from "../../api/mock";
import { NAN_BASE_URL, NAN_REFERRAL_URL, PRESETS, assignAll, brandOf, createFromPreset, presetOf, upsertPreset } from "./presets";

const llm = (): Settings["llm"] => defaultSettings().llm;

describe("presets de proveedores", () => {
  it("NaN Builders viene con su URL, un modelo que no gasta cuota y el enlace de invitación", () => {
    const p = createFromPreset("nan", []);
    expect(p).toMatchObject({ id: "nan", label: "NaN Builders", enabled: true });
    expect(p.kind).toEqual({ type: "openAiCompatible", baseUrl: "https://api.nan.builders/v1", model: "gemma4", apiKey: null });
    expect(NAN_BASE_URL).toBe("https://api.nan.builders/v1");
    expect(NAN_REFERRAL_URL).toBe("https://cloud.nan.builders/r/YZ524NPK");
    expect(PRESETS.nan.tag).toBe("Recomendado por privacidad");
  });

  it("los locales no piden clave y apuntan a este Mac", () => {
    for (const id of ["ollama", "lmStudio"] as const) {
      const p = createFromPreset(id, []);
      expect(PRESETS[id].needsKey).toBe(false);
      expect(p.kind.type === "openAiCompatible" && p.kind.baseUrl).toMatch(/^http:\/\/127\.0\.0\.1:\d+\/v1$/);
      expect(p.kind.type === "openAiCompatible" && p.kind.apiKey).toBeNull();
    }
  });

  it("OpenRouter, OpenAI y Anthropic son presets con clave propia", () => {
    expect(createFromPreset("openRouter", []).kind).toMatchObject({ type: "openAiCompatible", baseUrl: "https://openrouter.ai/api/v1" });
    expect(createFromPreset("openAi", []).kind).toMatchObject({ type: "openAiCompatible", baseUrl: "https://api.openai.com/v1" });
    const a = createFromPreset("anthropic", []);
    expect(a.kind).toMatchObject({ type: "anthropic", apiKey: { service: "acta", account: "provider:anthropic" } });
    expect(createFromPreset("custom", []).kind).toMatchObject({ type: "openAiCompatible", baseUrl: "" });
  });

  it("no repite identificadores", () => {
    const first = createFromPreset("openRouter", []);
    const second = createFromPreset("openRouter", [first]);
    expect([first.id, second.id]).toEqual(["openrouter", "openrouter-2"]);
  });

  it("reconoce de qué preset es un proveedor ya guardado", () => {
    const of = (kind: ProviderConfig["kind"]) => presetOf({ id: "x", label: "X", enabled: true, timeoutS: 600, kind });
    expect(of({ type: "claudeCli", binaryPath: "", model: null, effort: null, extraEnv: {} })).toBe("claudeCode");
    expect(of({ type: "codexCli", binaryPath: "", model: null, extraEnv: {} })).toBe("codex");
    expect(of({ type: "cursorCli", binaryPath: "", model: null, extraEnv: {} })).toBe("cursor");
    expect(of({ type: "openAiCompatible", baseUrl: "http://localhost:11434/v1/", model: "m", apiKey: null })).toBe("ollama");
    expect(of({ type: "openAiCompatible", baseUrl: "http://127.0.0.1:1234/v1", model: "m", apiKey: null })).toBe("lmStudio");
    expect(of({ type: "openAiCompatible", baseUrl: "https://API.nan.builders/v1", model: "m", apiKey: null })).toBe("nan");
    expect(of({ type: "openAiCompatible", baseUrl: "https://example.com/v1", model: "m", apiKey: null })).toBe("custom");
    expect(of({ type: "anthropic", model: "m", apiKey: { service: "acta", account: "a" } })).toBe("anthropic");
  });

  it("cada proveedor tiene su logo; el compatible genérico, ninguno", () => {
    expect(brandOf(createFromPreset("claudeCode", []))).toBe("claude");
    expect(brandOf(createFromPreset("codex", []))).toBe("openai");
    expect(brandOf(createFromPreset("cursor", []))).toBe("cursor");
    expect(brandOf(createFromPreset("nan", []))).toBe("nan");
    expect(brandOf(createFromPreset("lmStudio", []))).toBe("lmstudio");
    expect(brandOf(createFromPreset("custom", []))).toBeNull();
  });

  it("usar un preset reutiliza el proveedor que ya había y lo activa", () => {
    const before = llm();
    before.providers.push({ id: "ollama", label: "Ollama (local)", enabled: false, timeoutS: 600, kind: { type: "openAiCompatible", baseUrl: "http://localhost:11434/v1", model: "gemma3:12b", apiKey: null } });
    const { llm: after, provider } = upsertPreset(before, "ollama", { model: "llama3.2:latest", baseUrl: "http://127.0.0.1:11434/v1" });
    expect(after.providers).toHaveLength(2);
    expect(provider).toMatchObject({ id: "ollama", enabled: true });
    expect(provider.kind).toMatchObject({ model: "llama3.2:latest", baseUrl: "http://127.0.0.1:11434/v1" });
    expect(before.providers[1]!.enabled).toBe(false);
  });

  it("usar un preset nuevo lo añade", () => {
    const { llm: after, provider } = upsertPreset(llm(), "nan");
    expect(after.providers.map((p) => p.id)).toEqual(["claude-cli", "nan"]);
    expect(provider.id).toBe("nan");
  });

  it("asigna un proveedor a todas las tareas", () => {
    expect(assignAll(llm(), "nan")).toMatchObject({ minutesProvider: "nan", chatProvider: "nan", namingProvider: "nan" });
  });
});
