// Proveedores de IA que acta ofrece ya rellenos. Espejo de
// `crates/acta-llm/src/providers/presets.rs` (URLs y modelo de partida).

import type { ProviderConfig, ProviderKind, SecretRef, Settings } from "../../api";
import type { BrandId } from "../../shared/brand/brands";

export type PresetId = "claudeCode" | "codex" | "openCode" | "builtIn" | "ollama" | "lmStudio" | "nan" | "openRouter" | "openAi" | "anthropic" | "custom";

/** Id del proveedor del modelo local integrado (igual que `BUILTIN_PROVIDER_ID` en Rust). */
export const BUILTIN_PROVIDER_ID = "builtin";

export const NAN_BASE_URL = "https://api.nan.builders/v1";
/** Enlace de invitación a NaN Builders. Es intencionado: no se quita. */
export const NAN_REFERRAL_URL = "https://cloud.nan.builders/r/YZ524NPK";
/** Modelos de NaN que no consumen cuota (según nan.builders, 16-nov-2026). */
export const NAN_UNMETERED = ["gemma4", "qwen3.6"];

export interface Preset {
  id: PresetId;
  label: string;
  brand: BrandId | null;
  /** Una línea: qué es y por qué elegirlo. */
  blurb: string;
  /** Etiqueta corta sobre la tarjeta. */
  tag?: string;
  /** Raíz del identificador del proveedor que crea. */
  slug: string;
  needsKey: boolean;
  /** Corre en este Mac: el texto de la reunión no sale del equipo. */
  local?: boolean;
  baseUrl?: string;
  defaultModel?: string;
  /** Dónde se consigue la clave o el programa. */
  url?: string;
}

export const PRESETS: Record<PresetId, Preset> = {
  claudeCode: { id: "claudeCode", label: "Claude Code", brand: "claude", slug: "claude-cli", needsKey: false, blurb: "El Claude Code que ya tienes instalado, con tu suscripción.", tag: "Sin clave" },
  codex: { id: "codex", label: "Codex", brand: "openai", slug: "codex-cli", needsKey: false, blurb: "El Codex de OpenAI que ya tienes instalado, con tu suscripción.", tag: "Sin clave" },
  openCode: { id: "openCode", label: "OpenCode", brand: "opencode", slug: "opencode-cli", needsKey: false, blurb: "El OpenCode que ya tienes instalado, con tus proveedores configurados.", tag: "Sin clave" },
  builtIn: { id: "builtIn", label: "Modelo local integrado", brand: null, slug: BUILTIN_PROVIDER_ID, needsKey: false, local: true, blurb: "Un modelo que acta descarga y ejecuta en tu Mac. Nada sale del equipo.", tag: "En tu Mac" },
  ollama: { id: "ollama", label: "Ollama", brand: "ollama", slug: "ollama", needsKey: false, local: true, baseUrl: "http://127.0.0.1:11434/v1", blurb: "Modelos que corren en tu Mac. Nada sale del equipo.", tag: "En tu Mac", url: "https://ollama.com/download" },
  lmStudio: { id: "lmStudio", label: "LM Studio", brand: "lmstudio", slug: "lmstudio", needsKey: false, local: true, baseUrl: "http://127.0.0.1:1234/v1", blurb: "Modelos locales con una app para descargarlos y probarlos.", tag: "En tu Mac", url: "https://lmstudio.ai" },
  nan: { id: "nan", label: "NaN Builders", brand: "nan", slug: "nan", needsKey: true, baseUrl: NAN_BASE_URL, defaultModel: "gemma4", blurb: "Sin registros de prompts ni respuestas, y procesa en la UE.", tag: "Recomendado por privacidad", url: NAN_REFERRAL_URL },
  openRouter: { id: "openRouter", label: "OpenRouter", brand: "openrouter", slug: "openrouter", needsKey: true, baseUrl: "https://openrouter.ai/api/v1", blurb: "Una sola clave para cientos de modelos de distintos proveedores.", tag: "Clave de API", url: "https://openrouter.ai/keys" },
  openAi: { id: "openAi", label: "OpenAI", brand: "openai", slug: "openai", needsKey: true, baseUrl: "https://api.openai.com/v1", blurb: "La API de OpenAI con tu propia clave.", tag: "Clave de API", url: "https://platform.openai.com/api-keys" },
  anthropic: { id: "anthropic", label: "Anthropic", brand: "anthropic", slug: "anthropic", needsKey: true, blurb: "La API de Anthropic (Claude) con tu propia clave.", tag: "Clave de API", url: "https://console.anthropic.com/settings/keys" },
  custom: { id: "custom", label: "Otro servidor compatible", brand: null, slug: "custom", needsKey: false, baseUrl: "", blurb: "Cualquier servidor que hable el protocolo de OpenAI: pon su URL, la clave y el modelo." },
};

/** Orden en el que se ofrecen en la galería (el integrado tiene su propia sección). */
export const PRESET_ORDER: PresetId[] = ["claudeCode", "codex", "openCode", "ollama", "lmStudio", "nan", "openRouter", "openAi", "anthropic", "custom"];

const normUrl = (u: string): string => u.trim().replace(/\/+$/, "").toLowerCase().replace("//localhost", "//127.0.0.1");

/** El preset al que pertenece un proveedor guardado. */
export function presetOf(p: ProviderConfig): PresetId {
  const k = p.kind;
  if (k.type === "claudeCli") return "claudeCode";
  if (k.type === "codexCli") return "codex";
  if (k.type === "opencodeCli") return "openCode";
  if (k.type === "anthropic") return "anthropic";
  if (k.type === "builtIn") return "builtIn";
  const url = normUrl(k.baseUrl);
  return PRESET_ORDER.find((id) => PRESETS[id].baseUrl && normUrl(PRESETS[id].baseUrl) === url) ?? "custom";
}

export const brandOf = (p: ProviderConfig): BrandId | null => PRESETS[presetOf(p)].brand;

function freeId(slug: string, existing: ProviderConfig[]): string {
  if (!existing.some((p) => p.id === slug)) return slug;
  let n = 2;
  while (existing.some((p) => p.id === `${slug}-${n}`)) n += 1;
  return `${slug}-${n}`;
}

function kindFor(preset: Preset, id: string): ProviderKind {
  switch (preset.id) {
    case "claudeCode":
      return { type: "claudeCli", binaryPath: "", model: null, effort: null, extraEnv: {} };
    case "codex":
      return { type: "codexCli", binaryPath: "", model: null, extraEnv: {} };
    case "openCode":
      return { type: "opencodeCli", binaryPath: "", model: null, extraEnv: {} };
    case "anthropic":
      return { type: "anthropic", model: "", apiKey: { service: "acta", account: `provider:${id}` } };
    case "builtIn":
      return { type: "builtIn", modelId: "" };
    default:
      return { type: "openAiCompatible", baseUrl: preset.baseUrl ?? "", model: preset.defaultModel ?? "", apiKey: null };
  }
}

const LABELS: Partial<Record<PresetId, string>> = { claudeCode: "Claude Code (CLI)", codex: "Codex (CLI)", openCode: "OpenCode (CLI)", custom: "OpenAI compatible" };

export function createFromPreset(id: PresetId, existing: ProviderConfig[]): ProviderConfig {
  const preset = PRESETS[id];
  const providerId = freeId(preset.slug, existing);
  return { id: providerId, label: LABELS[id] ?? preset.label, enabled: true, timeoutS: 600, kind: kindFor(preset, providerId) };
}

export interface PresetPatch {
  model?: string;
  baseUrl?: string;
  binaryPath?: string;
  extraEnv?: Record<string, string>;
  /** Referencia al Llavero (solo servidores compatibles con OpenAI). */
  apiKey?: SecretRef | null;
}

function patched(p: ProviderConfig, patch: PresetPatch): ProviderConfig {
  const k = p.kind;
  if (k.type === "openAiCompatible") return { ...p, enabled: true, kind: { ...k, model: patch.model ?? k.model, baseUrl: patch.baseUrl ?? k.baseUrl, apiKey: patch.apiKey === undefined ? k.apiKey : patch.apiKey } };
  if (k.type === "anthropic") return { ...p, enabled: true, kind: { ...k, model: patch.model ?? k.model } };
  if (k.type === "builtIn") return { ...p, enabled: true, kind: { ...k, modelId: patch.model ?? k.modelId } };
  const cli = { binaryPath: patch.binaryPath ?? k.binaryPath, model: patch.model ?? k.model, extraEnv: { ...k.extraEnv, ...patch.extraEnv } };
  return { ...p, enabled: true, kind: { ...k, ...cli } };
}

/** Deja listo y activo el proveedor de un preset: reutiliza el que hubiera o lo crea. No muta `llm`. */
export function upsertPreset(llm: Settings["llm"], id: PresetId, patch: PresetPatch = {}): { llm: Settings["llm"]; provider: ProviderConfig } {
  const existing = id === "custom" ? undefined : llm.providers.find((p) => presetOf(p) === id);
  const provider = patched(existing ?? createFromPreset(id, llm.providers), patch);
  const providers = existing ? llm.providers.map((p) => (p.id === existing.id ? provider : p)) : [...llm.providers, provider];
  return { llm: { ...llm, providers }, provider };
}

/** El mismo proveedor para el acta, el chat y los nombres de grupo. */
export function assignAll(llm: Settings["llm"], providerId: string): Settings["llm"] {
  return { ...llm, minutesProvider: providerId, chatProvider: providerId, namingProvider: providerId };
}
