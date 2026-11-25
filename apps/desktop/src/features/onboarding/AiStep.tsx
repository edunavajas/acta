import { Cpu, KeyRound, Laptop, RotateCw } from "lucide-react";
import { useState, type ReactNode } from "react";
import type { CliDetection, LocalDetection, ProviderTestResult, SecretRef } from "../../api";
import { BrandLogo } from "../../shared/brand/BrandLogo";
import type { BrandId } from "../../shared/brand/brands";
import { Badge, Button, Select, Spinner, StatusDot } from "../../shared/ui/ui";
import { BuiltInModels, type BuiltInState } from "../providers/BuiltInModels";
import { ApiKeyField, ModelField } from "../providers/fields";
import { LocalModels } from "../providers/LocalModels";
import { NanIntro } from "../providers/NanIntro";
import { NAN_UNMETERED, PRESETS } from "../providers/presets";
import { Field } from "../settings/fields";
import type { AiChoice, AiState, OwnService } from "./aiChoice";
import { StepHead } from "./steps";

interface Props {
  ai: AiState;
  onAi(next: AiState): void;
  /** `undefined` mientras se busca en el Mac. */
  cli: CliDetection | null | undefined;
  onDetectCli(): void;
  local: LocalDetection | null;
  /** No se pudo buscar Ollama ni LM Studio. */
  localError?: string | null;
  /** Modelo local integrado: catálogo, descargas y pruebas. */
  builtIn: BuiltInState;
  onRefreshLocal(): void;
  onOpenUrl(url: string): void;
  onCreateNanAccount(): void;
  /** Guarda la clave del proveedor elegido en el Llavero. */
  onSecret(value: string): Promise<SecretRef>;
  listModels(baseUrl: string, withKey: boolean): Promise<string[]>;
  problem: string | null;
  test: ProviderTestResult | "pending" | null;
  onTest(): void;
}

const OWN: { key: OwnService; label: string }[] = [
  { key: "openAi", label: "OpenAI" },
  { key: "anthropic", label: "Anthropic" },
  { key: "openRouter", label: "OpenRouter" },
  { key: "custom", label: "Otro servidor compatible" },
];

function Option({ id, current, onPick, logo, title, tag, children }: { id: AiChoice; current: AiChoice | null; onPick(c: AiChoice): void; logo: ReactNode; title: string; tag?: ReactNode; children: ReactNode }) {
  return (
    <button type="button" role="radio" aria-checked={current === id} className="ai-option" onClick={() => onPick(id)}>
      {logo}
      <span className="ai-option-text">
        <span className="ai-option-title">
          {title}
          {tag}
        </span>
        <span className="ai-option-sub">{children}</span>
      </span>
      <span className="ai-radio" aria-hidden />
    </button>
  );
}

function CliStatus({ path, pending }: { path: string | null | undefined; pending: boolean }) {
  if (pending)
    return (
      <>
        <Spinner /> Buscando en este Mac…
      </>
    );
  return (
    <>
      <StatusDot tone={path ? "ok" : "neutral"} />
      {path ? "Detectado en este Mac" : "No está instalado"}
    </>
  );
}

/** Qué servidores locales hay en marcha en este Mac, y con cuántos modelos. */
function LocalStatus({ local }: { local: LocalDetection | null }) {
  if (!local)
    return (
      <>
        <Spinner /> Buscando en este Mac…
      </>
    );
  const running = [local.ollama.running ? "Ollama" : null, local.lmStudio.running ? "LM Studio" : null].filter((n): n is string => n !== null);
  const models = (local.ollama.running ? local.ollama.models.length : 0) + (local.lmStudio.running ? local.lmStudio.models.length : 0);
  if (running.length === 0)
    return (
      <>
        <StatusDot tone="neutral" />
        Ninguno en marcha
      </>
    );
  return (
    <>
      <StatusDot tone={models > 0 ? "ok" : "warn"} />
      {running.join(" y ")} en marcha · {models === 0 ? "sin modelos" : `${models} ${models === 1 ? "modelo" : "modelos"}`}
    </>
  );
}

const iconTile = (icon: ReactNode) => (
  <span className="brand-tile brand-md brand-generic" aria-hidden>
    {icon}
  </span>
);

/** Paso «Inteligencia artificial»: quién escribe las actas. */
export function AiStep({ ai, onAi, cli, onDetectCli, local, localError, builtIn, onRefreshLocal, onOpenUrl, onCreateNanAccount, onSecret, listModels, problem, test, onTest }: Props) {
  const [reloadKey, setReloadKey] = useState(0);
  const pending = cli === undefined;
  const pick = (choice: AiChoice) => onAi({ ...ai, choice });
  const own = ai.own;
  const ownPreset = PRESETS[own.service];
  const ownBrand: BrandId | null = ownPreset.brand;
  const ownUrl = own.service === "custom" ? own.baseUrl : (ownPreset.baseUrl ?? "");

  return (
    <div className="ob-step">
      <StepHead title="¿Quién escribe tus actas?" sub="La transcripción ya es local. Para el resumen y el chat hace falta un modelo de lenguaje. Esto es lo que hay en tu Mac y lo que puedes conectar: no hay nada activado de antemano, decides tú. Solo recibe el texto de la reunión, nunca el audio." />
      <div role="radiogroup" aria-label="Proveedor de IA" className="ai-options">
        <Option id="claudeCode" current={ai.choice} onPick={pick} logo={<BrandLogo brand="claude" />} title="Claude Code">
          <CliStatus path={cli?.claude} pending={pending} />
        </Option>
        <Option id="codex" current={ai.choice} onPick={pick} logo={<BrandLogo brand="openai" />} title="Codex">
          <CliStatus path={cli?.codex} pending={pending} />
        </Option>
        <Option id="openCode" current={ai.choice} onPick={pick} logo={<BrandLogo brand="opencode" />} title="OpenCode">
          <CliStatus path={cli?.opencode} pending={pending} />
        </Option>
        <Option id="builtIn" current={ai.choice} onPick={pick} logo={iconTile(<Cpu strokeWidth={1.9} />)} title="Modelo integrado" tag={<Badge tone="accent">Sin instalar nada</Badge>}>
          acta descarga un modelo y lo ejecuta en tu Mac.
        </Option>
        <Option id="local" current={ai.choice} onPick={pick} logo={iconTile(<Laptop strokeWidth={1.9} />)} title="Ollama o LM Studio" tag={<Badge>Todo en tu Mac</Badge>}>
          <LocalStatus local={local} />
        </Option>
        <Option id="nan" current={ai.choice} onPick={pick} logo={<BrandLogo brand="nan" />} title="NaN Builders" tag={<Badge tone="ok">{PRESETS.nan.tag}</Badge>}>
          Sin registros y en la UE.
        </Option>
        <Option id="own" current={ai.choice} onPick={pick} logo={iconTile(<KeyRound strokeWidth={1.9} />)} title="Mi propia clave">
          OpenAI, Anthropic, OpenRouter u otro servidor.
        </Option>
      </div>

      {ai.choice === "claudeCode" || ai.choice === "codex" || ai.choice === "openCode" ? (
        <div className="ai-detail card">
          <p>
            {ai.choice === "claudeCode"
              ? "acta usa el Claude Code que ya tienes, con tu suscripción."
              : ai.choice === "codex"
                ? "acta usa el Codex que ya tienes, con tu suscripción."
                : "acta usa el OpenCode que ya tienes, con los proveedores configurados."}{" "}
            No lee ni copia tus credenciales: lo ejecuta como lo harías tú en la terminal.
          </p>
          <div>
            <Button small capsule variant="ghost" disabled={pending} icon={<RotateCw size={12} strokeWidth={2.2} aria-hidden />} onClick={onDetectCli}>
              Volver a buscar
            </Button>
          </div>
        </div>
      ) : null}

      {ai.choice === "builtIn" ? (
        <div className="ai-detail">
          <BuiltInModels state={builtIn} verb="Elegir" recommendedFirst inUse={ai.builtIn?.modelId ?? null} onUse={(modelId) => onAi({ ...ai, builtIn: { modelId } })} />
        </div>
      ) : null}

      {ai.choice === "local" ? (
        <div className="ai-detail">
          <LocalModels
            detection={local}
            error={localError}
            verb="Elegir"
            inUse={ai.local ? { [ai.local.kind]: ai.local.model } : {}}
            onUse={(kind, model, baseUrl) => onAi({ ...ai, local: { kind, model, baseUrl } })}
            onRefresh={onRefreshLocal}
            onOpenUrl={onOpenUrl}
          />
        </div>
      ) : null}

      {ai.choice === "nan" ? (
        <div className="ai-detail card">
          <NanIntro heading={false} onCreateAccount={onCreateNanAccount} />
          <ApiKeyField
            label="NaN Builders"
            ref_={ai.nan.apiKey}
            onSave={async (value) => {
              onAi({ ...ai, nan: { ...ai.nan, apiKey: await onSecret(value) } });
              setReloadKey((n) => n + 1);
            }}
          />
          {ai.nan.apiKey ? (
            <ModelField label="NaN Builders" value={ai.nan.model} reloadKey={reloadKey} prefer={NAN_UNMETERED} hint="Gemma 4 y Qwen 3.6 no consumen cuota." onChange={(model) => onAi({ ...ai, nan: { ...ai.nan, model } })} load={() => listModels(PRESETS.nan.baseUrl ?? "", true)} />
          ) : null}
        </div>
      ) : null}

      {ai.choice === "own" ? (
        <div className="ai-detail card">
          <div className="ai-own-head">
            {ownBrand ? <BrandLogo brand={ownBrand} size="sm" /> : null}
            <Field label="Servicio">
              <Select label="Servicio" value={own.service} onChange={(e) => onAi({ ...ai, own: { service: e.target.value as OwnService, apiKey: null, model: "", baseUrl: "" } })}>
                {OWN.map((o) => (
                  <option key={o.key} value={o.key}>
                    {o.label}
                  </option>
                ))}
              </Select>
            </Field>
          </div>
          {own.service === "custom" ? (
            <Field label="URL base" hint="La dirección de la API, normalmente acabada en /v1.">
              <input className="input-mono" aria-label="URL base del servidor" placeholder="https://servidor/v1" spellCheck={false} value={own.baseUrl} onChange={(e) => onAi({ ...ai, own: { ...own, baseUrl: e.target.value } })} />
            </Field>
          ) : null}
          <ApiKeyField
            label={ownPreset.id === "custom" ? "tu servidor" : ownPreset.label}
            ref_={own.apiKey}
            onSave={async (value) => {
              onAi({ ...ai, own: { ...own, apiKey: await onSecret(value) } });
              if (own.service !== "anthropic") setReloadKey((n) => n + 1);
            }}
          />
          {own.service === "anthropic" ? (
            <Field label="Modelo" hint="El identificador del modelo, tal cual aparece en la consola de Anthropic.">
              <input aria-label="Modelo de Anthropic" spellCheck={false} value={own.model} onChange={(e) => onAi({ ...ai, own: { ...own, model: e.target.value } })} />
            </Field>
          ) : (
            <ModelField label={ownPreset.id === "custom" ? "tu servidor" : ownPreset.label} value={own.model} reloadKey={reloadKey} onChange={(model) => onAi({ ...ai, own: { ...own, model } })} load={() => listModels(ownUrl, !!own.apiKey)} />
          )}
        </div>
      ) : null}

      <div className="ob-foot-note">
        {problem ? (
          <>
            <span className="ai-problem">{problem}</span>
            <Button small capsule variant="ghost" title="Lo eliges más tarde en Ajustes › Proveedores de IA" onClick={() => onAi({ ...ai, choice: null })}>
              Seguir sin proveedor
            </Button>
          </>
        ) : ai.choice ? (
          <>
            <span className="ai-test">
              {test === "pending" ? (
                <>
                  <Spinner /> Probando…
                </>
              ) : test ? (
                <span role="status" className={`test-result ${test.ok ? "ok" : "error"}`}>
                  <StatusDot tone={test.ok ? "ok" : "danger"} />
                  {test.message}
                </span>
              ) : (
                "Puedes comprobar que responde antes de seguir."
              )}
            </span>
            <Button small capsule disabled={test === "pending"} onClick={onTest}>
              Probar conexión
            </Button>
          </>
        ) : (
          <span>Puedes decidirlo más tarde en Ajustes › Proveedores de IA. Sin proveedor hay transcripción, pero no acta ni chat.</span>
        )}
      </div>
    </div>
  );
}
