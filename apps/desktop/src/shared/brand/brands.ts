// Logotipos de terceros, empaquetados con la app (ver assets/logos/NOTICE.md).
// Nunca se cargan de internet.

import type { Platform } from "../../api";
import anthropic from "../../assets/logos/anthropic.svg";
import claude from "../../assets/logos/claude.svg";
import googlemeet from "../../assets/logos/googlemeet.svg";
import lmstudio from "../../assets/logos/lmstudio.svg";
import microsoftteams from "../../assets/logos/microsoftteams.svg";
import nanbuilders from "../../assets/logos/nanbuilders.png";
import ollama from "../../assets/logos/ollama.svg";
import openai from "../../assets/logos/openai.svg";
import opencode from "../../assets/logos/opencode.svg";
import openrouter from "../../assets/logos/openrouter.svg";
import slack from "../../assets/logos/slack.svg";
import zoom from "../../assets/logos/zoom.svg";

export type BrandId = "teams" | "meet" | "zoom" | "slack" | "claude" | "anthropic" | "openai" | "opencode" | "ollama" | "lmstudio" | "openrouter" | "nan";

export interface Brand {
  name: string;
  /** Archivo empaquetado. */
  src: string;
  /** Color del mosaico. */
  color: string;
  /** Color del símbolo sobre el mosaico (blanco si no se dice). */
  ink?: string;
  /** Imagen a todo color: se pinta tal cual, sin recortarla como símbolo. */
  image?: boolean;
  /** Cuánto ocupa el símbolo dentro del mosaico (0-1). */
  scale?: number;
}

export const BRANDS: Record<BrandId, Brand> = {
  teams: { name: "Microsoft Teams", src: microsoftteams, color: "#5059c9" },
  meet: { name: "Google Meet", src: googlemeet, color: "#00897b" },
  zoom: { name: "Zoom", src: zoom, color: "#0b5cff", scale: 0.72 },
  slack: { name: "Slack", src: slack, color: "#4a154b" },
  claude: { name: "Claude", src: claude, color: "#d97757" },
  anthropic: { name: "Anthropic", src: anthropic, color: "#191919", ink: "#faf9f5" },
  openai: { name: "OpenAI", src: openai, color: "#0d0d0d" },
  opencode: { name: "OpenCode", src: opencode, color: "#0c0c0c", image: true, scale: 0.62 },
  ollama: { name: "Ollama", src: ollama, color: "#ffffff", ink: "#111111", scale: 0.66 },
  lmstudio: { name: "LM Studio", src: lmstudio, color: "#ffffff", ink: "#5b48e8", scale: 0.74 },
  openrouter: { name: "OpenRouter", src: openrouter, color: "#6467f2" },
  nan: { name: "NaN Builders", src: nanbuilders, color: "#0c0e1a", image: true, scale: 0.86 },
};

const PLATFORM_BRAND: Record<Platform, BrandId | null> = {
  teams: "teams",
  meet: "meet",
  zoom: "zoom",
  slack: "slack",
  other: null,
};

/** La marca de una plataforma de reuniones, si la conocemos. */
export const platformBrand = (p: Platform): BrandId | null => PLATFORM_BRAND[p];
