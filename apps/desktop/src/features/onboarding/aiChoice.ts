// Lógica pura del paso «Inteligencia artificial» de la bienvenida: de una
// elección a los ajustes de proveedores. Sin efectos: se prueba sola.

import type { CliDetection, SecretRef, Settings } from "../../api";
import type { LocalKind } from "../providers/LocalModels";
import { PRESETS, assignAll, presetOf, upsertPreset, type PresetId } from "../providers/presets";

export type AiChoice = "claudeCode" | "codex" | "openCode" | "builtIn" | "local" | "nan" | "own";
export type OwnService = "openAi" | "anthropic" | "openRouter" | "custom";
type Llm = Settings["llm"];

export interface AiState {
  choice: AiChoice | null;
  /** Modelo integrado elegido (ya descargado). */
  builtIn: { modelId: string } | null;
  local: { kind: LocalKind; model: string; baseUrl: string } | null;
  nan: { apiKey: SecretRef | null; model: string };
  own: { service: OwnService; apiKey: SecretRef | null; model: string; baseUrl: string };
}

const CHOICE_OF: Record<PresetId, AiChoice> = {
  claudeCode: "claudeCode",
  codex: "codex",
  openCode: "openCode",
  builtIn: "builtIn",
  ollama: "local",
  lmStudio: "local",
  nan: "nan",
  openRouter: "own",
  openAi: "own",
  anthropic: "own",
  custom: "own",
};

/** Arranca en lo que ya escribe las actas, si hay algo. */
export function initialAi(llm: Llm): AiState {
  const current = llm.providers.find((p) => p.enabled && p.id === llm.minutesProvider);
  const preset = current ? presetOf(current) : null;
  const k = current?.kind;
  const state: AiState = {
    choice: preset ? CHOICE_OF[preset] : null,
    builtIn: k?.type === "builtIn" && k.modelId ? { modelId: k.modelId } : null,
    local: null,
    nan: { apiKey: null, model: PRESETS.nan.defaultModel ?? "" },
    own: { service: "openAi", apiKey: null, model: "", baseUrl: "" },
  };
  if (k?.type === "openAiCompatible") {
    if (preset === "ollama" || preset === "lmStudio") state.local = { kind: preset, model: k.model, baseUrl: k.baseUrl };
    else if (preset === "nan") state.nan = { apiKey: k.apiKey, model: k.model };
    else if (preset === "openAi" || preset === "openRouter" || preset === "custom") state.own = { service: preset, apiKey: k.apiKey, model: k.model, baseUrl: k.baseUrl };
  } else if (k?.type === "anthropic") state.own = { service: "anthropic", apiKey: k.apiKey, model: k.model, baseUrl: "" };
  return state;
}

function presetFor(ai: AiState): PresetId | null {
  switch (ai.choice) {
    case "claudeCode":
    case "codex":
    case "openCode":
    case "builtIn":
    case "nan":
      return ai.choice;
    case "local":
      return ai.local?.kind ?? null;
    case "own":
      return ai.own.service;
    default:
      return null;
  }
}

/** Identificador del proveedor que saldrá de la elección: con él se guarda su clave en el Llavero. */
export function secretIdFor(llm: Llm, ai: AiState): string | null {
  const preset = presetFor(ai);
  return preset ? upsertPreset(llm, preset).provider.id : null;
}

/** Los ajustes de IA que resultan de la elección. No muta `llm`. */
export function applyAi(llm: Llm, ai: AiState, cli: CliDetection | null): Llm {
  const preset = presetFor(ai);
  if (!preset) return llm;
  let next: ReturnType<typeof upsertPreset>;
  if (preset === "claudeCode") next = upsertPreset(llm, preset, cli?.claude ? { binaryPath: cli.claude, extraEnv: cli.claudeEnv } : {});
  else if (preset === "codex") next = upsertPreset(llm, preset, cli?.codex ? { binaryPath: cli.codex, extraEnv: cli.codexEnv } : {});
  else if (preset === "openCode") next = upsertPreset(llm, preset, cli?.opencode ? { binaryPath: cli.opencode, extraEnv: cli.opencodeEnv } : {});
  else if (ai.choice === "builtIn") next = upsertPreset(llm, preset, ai.builtIn ? { model: ai.builtIn.modelId } : {});
  else if (ai.choice === "local" && ai.local) next = upsertPreset(llm, preset, { model: ai.local.model, baseUrl: ai.local.baseUrl });
  else if (ai.choice === "nan") next = upsertPreset(llm, preset, { model: ai.nan.model, apiKey: ai.nan.apiKey });
  else next = upsertPreset(llm, preset, { model: ai.own.model, apiKey: ai.own.apiKey, ...(ai.own.service === "custom" ? { baseUrl: ai.own.baseUrl } : {}) });
  return assignAll(next.llm, next.provider.id);
}

/** Qué falta para que la elección funcione; `null` si se puede continuar. */
export function aiProblem(ai: AiState, cli: CliDetection | null, cliPending: boolean): string | null {
  switch (ai.choice) {
    case null:
      return null;
    case "claudeCode":
      return cliPending || cli?.claude ? null : "Claude Code no está instalado en este Mac. Elige otra opción o instálalo y vuelve a buscar.";
    case "codex":
      return cliPending || cli?.codex ? null : "Codex no está instalado en este Mac. Elige otra opción o instálalo y vuelve a buscar.";
    case "openCode":
      return cliPending || cli?.opencode ? null : "OpenCode no está instalado en este Mac. Elige otra opción o instálalo y vuelve a buscar.";
    case "builtIn":
      return ai.builtIn ? null : "Descarga un modelo y pulsa «Elegir» para continuar.";
    case "local":
      return ai.local?.model ? null : "Elige un modelo de los que tienes en tu Mac.";
    case "nan":
      if (!ai.nan.apiKey) return "Guarda la clave de API de NaN Builders para continuar.";
      return ai.nan.model ? null : "Elige un modelo.";
    case "own":
      if (ai.own.service === "custom" && !ai.own.baseUrl.trim()) return "Escribe la URL base del servidor.";
      if (ai.own.service !== "custom" && !ai.own.apiKey) return "Guarda tu clave de API para continuar.";
      return ai.own.model.trim() ? null : "Indica qué modelo usar.";
  }
}
