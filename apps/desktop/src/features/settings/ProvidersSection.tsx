import { CircleAlert, Cpu, Plus, ScanSearch, Server, Trash2, X } from "lucide-react";
import { useEffect, useState } from "react";
import type { ProviderConfig, ProviderKind, ProviderTestResult, SecretRef, Settings } from "../../api";
import { useApi } from "../../app/ApiContext";
import { BuiltInModels, useBuiltInModels } from "../providers/BuiltInModels";
import { ApiKeyField, ModelField } from "../providers/fields";
import { LocalModels, useLocalDetection, type LocalKind } from "../providers/LocalModels";
import { NanIntro } from "../providers/NanIntro";
import { PresetGallery } from "../providers/PresetGallery";
import { NAN_REFERRAL_URL, NAN_UNMETERED, PRESETS, PRESET_ORDER, assignAll, brandOf, createFromPreset, presetOf, upsertPreset, type PresetId } from "../providers/presets";
import { BrandLogo } from "../../shared/brand/BrandLogo";
import { Button, Disclosure, IconButton, Select, Spinner, StatusDot, Stepper, Switch, errorMessage } from "../../shared/ui/ui";
import { Field, Group, Row, Section } from "./fields";

const EFFORTS: { v: string; l: string }[] = [
  { v: "", l: "Por defecto" },
  { v: "low", l: "Bajo" },
  { v: "medium", l: "Medio" },
  { v: "high", l: "Alto" },
  { v: "xhigh", l: "Muy alto" },
  { v: "max", l: "Máximo" },
];

type EnvRow = [string, string];
const envToRows = (env: Record<string, string>): EnvRow[] => Object.entries(env);
const rowsToEnv = (rows: EnvRow[]): Record<string, string> => Object.fromEntries(rows.filter(([k]) => k.trim() !== "").map(([k, v]) => [k.trim(), v]));

/** Filas clave/valor. Las filas sin nombre viven solo en local hasta que se rellenan. */
function EnvEditor({ label, env, onChange }: { label: string; env: Record<string, string>; onChange(env: Record<string, string>): void }) {
  const [rows, setRows] = useState<EnvRow[]>(() => envToRows(env));
  const serialized = JSON.stringify(env);
  useEffect(() => {
    setRows((current) => (JSON.stringify(rowsToEnv(current)) === serialized ? current : envToRows(env)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [serialized]);
  const commit = (next: EnvRow[]) => {
    setRows(next);
    onChange(rowsToEnv(next));
  };
  return (
    <Field label="Variables de entorno" hint="Solo si tu instalación las necesita (un proxy, otra carpeta de configuración…). «Detectar CLI instalados» copia las que use tu terminal. Las credenciales del CLI no se tocan nunca.">
      {rows.length > 0 ? (
        <ul className="plain env-rows">
          {rows.map(([k, v], i) => (
            <li key={i} className="field-line">
              <input className="input-mono" aria-label={`Nombre de la variable ${i + 1} de ${label}`} placeholder="NOMBRE" spellCheck={false} value={k} onChange={(e) => commit(rows.map((r, j) => (j === i ? [e.target.value, r[1]] : r)))} />
              <input className="input-mono" aria-label={`Valor de la variable ${i + 1} de ${label}`} placeholder="valor" spellCheck={false} value={v} onChange={(e) => commit(rows.map((r, j) => (j === i ? [r[0], e.target.value] : r)))} />
              <IconButton small label={`Quitar variable ${k || i + 1} de ${label}`} onClick={() => commit(rows.filter((_, j) => j !== i))}>
                <X size={13} strokeWidth={2.2} aria-hidden />
              </IconButton>
            </li>
          ))}
        </ul>
      ) : null}
      <div>
        <Button small icon={<Plus size={12} strokeWidth={2.4} aria-hidden />} aria-label={`Añadir variable a ${label}`} onClick={() => commit([...rows, ["", ""]])}>
          Añadir variable
        </Button>
      </div>
    </Field>
  );
}

export type DetectState = "pending" | { ok: boolean; text: string } | null;

interface Props {
  llm: Settings["llm"];
  tests: Record<string, ProviderTestResult | "pending" | undefined>;
  onChange(llm: Settings["llm"]): void;
  onTest(id: string): void;
  onDetect(): void;
  /** Resultado de la última búsqueda de CLI: se enseña junto al botón. */
  detecting?: DetectState;
  /** Abre un enlace en el navegador; si falla, se dice aquí. */
  onOpenUrl?(url: string): Promise<void>;
  onSecretSet(providerId: string, value: string): Promise<SecretRef>;
  onSecretDelete(providerId: string): Promise<void>;
}

/** Presets de los que solo tiene sentido tener uno. */
const SINGLE: PresetId[] = ["claudeCode", "codex", "openCode", "builtIn", "ollama", "lmStudio", "nan"];

function ProviderCard({ p, result, onChange, onRemove, onTest, onSecretSet, onSecretDelete, onOpenUrl }: { p: ProviderConfig; result: ProviderTestResult | "pending" | undefined; onChange(p: ProviderConfig): void; onRemove(): void; onTest(): void; onSecretSet: Props["onSecretSet"]; onSecretDelete: Props["onSecretDelete"]; onOpenUrl(url: string): void }) {
  const api = useApi();
  const [reloadKey, setReloadKey] = useState(0);
  const k = p.kind;
  const preset = PRESETS[presetOf(p)];
  const brand = brandOf(p);
  const setKind = (kind: ProviderKind) => onChange({ ...p, kind });
  const isCli = k.type === "claudeCli" || k.type === "codexCli" || k.type === "opencodeCli";
  const located = isCli && k.binaryPath.trim() !== "";
  const isCustom = preset.id === "custom";
  const urlField =
    k.type === "openAiCompatible" ? (
      <Field label="URL base" hint={isCustom ? "La dirección de la API, normalmente acabada en /v1." : "Viene ya puesta. Cámbiala solo si tu servidor está en otro sitio."}>
        <input className="input-mono" aria-label={`URL base de ${p.label}`} placeholder="https://servidor/v1" spellCheck={false} value={k.baseUrl} onChange={(e) => setKind({ ...k, baseUrl: e.target.value })} />
      </Field>
    ) : null;
  return (
    <div className="provider card" role="group" aria-label={p.label}>
      <div className="provider-head">
        {brand ? (
          <BrandLogo brand={brand} size="lg" />
        ) : (
          <span className="brand-tile brand-lg brand-generic" aria-hidden>
            {k.type === "builtIn" ? <Cpu strokeWidth={1.9} /> : <Server strokeWidth={1.9} />}
          </span>
        )}
        <div className="provider-title">
          <input aria-label={`Nombre de ${p.label}`} title="Puedes cambiarle el nombre" className="title-input" value={p.label} onChange={(e) => onChange({ ...p, label: e.target.value })} />
          <p className="provider-sub">
            {isCli ? (
              <>
                <StatusDot tone={located ? "ok" : "warn"} />
                {located ? "Instalado en tu Mac" : "Sin localizar: pulsa «Detectar CLI instalados»"}
              </>
            ) : (
              preset.blurb
            )}
          </p>
        </div>
        <Switch label={`Activar ${p.label}`} checked={p.enabled} onChange={(enabled) => onChange({ ...p, enabled })} />
      </div>

      {preset.id === "nan" ? <NanIntro heading={false} onCreateAccount={() => onOpenUrl(NAN_REFERRAL_URL)} /> : null}

      {k.type === "openAiCompatible" && preset.needsKey ? (
        <ApiKeyField
          label={p.label}
          ref_={k.apiKey}
          onSave={async (value) => {
            setKind({ ...k, apiKey: await onSecretSet(p.id, value) });
            setReloadKey((n) => n + 1);
          }}
          onDelete={async () => {
            await onSecretDelete(p.id);
            setKind({ ...k, apiKey: null });
          }}
        />
      ) : null}
      {k.type === "anthropic" ? (
        <ApiKeyField
          label={p.label}
          ref_={k.apiKey}
          onSave={async (value) => setKind({ ...k, apiKey: await onSecretSet(p.id, value) })}
          onDelete={async () => {
            await onSecretDelete(p.id);
          }}
        />
      ) : null}

      <div className={`provider-fields${(k.type === "openAiCompatible" && !isCustom) || k.type === "opencodeCli" ? " provider-fields-one" : ""}`}>
        {isCli ? (
          <>
            {k.type === "opencodeCli" ? (
              <ModelField
                label={p.label}
                value={k.model ?? ""}
                placeholder="proveedor/modelo"
                hint="Pulsa «Ver modelos» para elegir uno de OpenCode. Si lo dejas vacío, acta usa el modelo por defecto o el último usado en OpenCode."
                onChange={(model) => setKind({ ...k, model: model.trim() || null })}
                load={() => api.llmListOpencodeModels(k.binaryPath, p.id)}
              />
            ) : (
              <Field label="Modelo" hint="Vacío: el que tenga configurado el CLI.">
                <input aria-label={`Modelo de ${p.label}`} placeholder="Por defecto" spellCheck={false} value={k.model ?? ""} onChange={(e) => setKind({ ...k, model: e.target.value || null })} />
              </Field>
            )}
            {k.type === "claudeCli" ? (
              <Field label="Esfuerzo" hint="Cuánto piensa antes de responder.">
                <Select label={`Esfuerzo de ${p.label}`} value={k.effort ?? ""} onChange={(e) => setKind({ ...k, effort: e.target.value || null })}>
                  {EFFORTS.map((o) => (
                    <option key={o.v} value={o.v}>
                      {o.l}
                    </option>
                  ))}
                </Select>
              </Field>
            ) : null}
          </>
        ) : null}
        {k.type === "openAiCompatible" ? (
          <>
            {isCustom ? urlField : null}
            <ModelField
              label={p.label}
              value={k.model}
              reloadKey={reloadKey}
              prefer={preset.id === "nan" ? NAN_UNMETERED : []}
              hint={preset.id === "nan" ? "Gemma 4 y Qwen 3.6 no consumen cuota." : preset.local ? "Los modelos que tengas descargados." : "Pulsa «Ver modelos» para elegirlo de la lista."}
              onChange={(model) => setKind({ ...k, model })}
              load={() => api.llmListModels(k.baseUrl, k.apiKey ? p.id : null)}
            />
          </>
        ) : null}
        {k.type === "anthropic" ? (
          <Field label="Modelo" hint="El identificador del modelo, tal cual aparece en la consola de Anthropic.">
            <input aria-label={`Modelo de ${p.label}`} spellCheck={false} value={k.model} onChange={(e) => setKind({ ...k, model: e.target.value })} />
          </Field>
        ) : null}
        {k.type === "builtIn" ? (
          <Field label="Modelo" hint="Se cambia en «Modelo local integrado», más abajo.">
            <code className="path selectable">{k.modelId || "Sin elegir"}</code>
          </Field>
        ) : null}
      </div>
      {k.type === "openAiCompatible" && isCustom ? (
        <ApiKeyField
          label={p.label}
          ref_={k.apiKey}
          onSave={async (value) => setKind({ ...k, apiKey: await onSecretSet(p.id, value) })}
          onDelete={async () => {
            await onSecretDelete(p.id);
            setKind({ ...k, apiKey: null });
          }}
        />
      ) : null}

      <div className="provider-actions">
        <Button capsule onClick={onTest} aria-label={`Probar conexión ${p.label}`} disabled={result === "pending"}>
          Probar conexión
        </Button>
        {result === "pending" ? (
          <span className="test-result muted">
            <Spinner /> Probando…
          </span>
        ) : null}
        {result && result !== "pending" ? (
          <span role="status" className={`test-result ${result.ok ? "ok" : "error"}`}>
            <StatusDot tone={result.ok ? "ok" : "danger"} />
            {result.message}
            {result.latencyMs != null ? ` (${result.latencyMs} ms)` : ""}
          </span>
        ) : null}
      </div>

      <Disclosure label="Avanzado" ariaLabel={`Avanzado de ${p.label}`}>
        {isCli ? (
          <>
            <Field label="Ruta del programa" hint="Las apps no heredan el PATH de tu terminal: hace falta la ruta completa. «Detectar» la rellena sola.">
              <input className="input-mono" aria-label={`Ruta del binario de ${p.label}`} value={k.binaryPath} placeholder={k.type === "opencodeCli" ? "~/.opencode/bin/opencode" : "/opt/homebrew/bin/claude"} spellCheck={false} onChange={(e) => setKind({ ...k, binaryPath: e.target.value })} />
            </Field>
            <EnvEditor label={p.label} env={k.extraEnv} onChange={(extraEnv) => setKind({ ...k, extraEnv })} />
          </>
        ) : null}
        {isCustom ? null : urlField}
        <Field label="Tiempo máximo de espera" hint="Las actas de reuniones largas pueden tardar varios minutos.">
          <Stepper label={`Tiempo máximo de ${p.label} (s)`} value={p.timeoutS} min={5} step={30} unit="s" onChange={(timeoutS) => onChange({ ...p, timeoutS })} />
        </Field>
        <div>
          <Button small variant="danger" icon={<Trash2 size={13} strokeWidth={2} aria-hidden />} onClick={onRemove} aria-label={`Eliminar ${p.label}`}>
            Eliminar proveedor
          </Button>
        </div>
      </Disclosure>
    </div>
  );
}

export function ProvidersSection({ llm, tests, onChange, onTest, onDetect, detecting = null, onOpenUrl, onSecretSet, onSecretDelete }: Props) {
  const api = useApi();
  const { detection, refresh, error: localError } = useLocalDetection();
  const builtIn = useBuiltInModels();
  const [linkError, setLinkError] = useState<string | null>(null);
  const enabled = llm.providers.filter((p) => p.enabled);
  const has = (id: PresetId) => llm.providers.some((p) => presetOf(p) === id);
  const openUrl = (url: string) => {
    setLinkError(null);
    (onOpenUrl ?? ((u: string) => api.openExternalUrl(u)))(url).catch((e: unknown) => setLinkError(`No se ha podido abrir ${url} en el navegador: ${errorMessage(e)}`));
  };
  // Quien escribe las actas: sin él hay transcripción, pero no resumen.
  const writer = enabled.find((p) => p.id === llm.minutesProvider);

  const task = (label: string, hint: string, aria: string, key: "minutesProvider" | "chatProvider" | "namingProvider") => (
    <Row label={label} hint={hint}>
      <Select label={`Proveedor para ${aria}`} value={llm[key] ?? ""} onChange={(e) => onChange({ ...llm, [key]: e.target.value || null })}>
        <option value="">Sin asignar</option>
        {enabled.map((p) => (
          <option key={p.id} value={p.id}>
            {p.label}
          </option>
        ))}
      </Select>
    </Row>
  );

  const add = (id: PresetId) => {
    const created = createFromPreset(id, llm.providers);
    // Lo que aún no hace nadie pasa a hacerlo el que se acaba de elegir; lo ya asignado no se toca.
    const active = (task: string | null) => (enabled.some((p) => p.id === task) ? task : created.id);
    onChange({ ...llm, providers: [...llm.providers, created], minutesProvider: active(llm.minutesProvider), chatProvider: active(llm.chatProvider), namingProvider: active(llm.namingProvider) });
    // Los CLI necesitan su ruta: se busca sola.
    if (id === "claudeCode" || id === "codex" || id === "openCode") onDetect();
  };
  const useLocal = (kind: LocalKind, model: string, baseUrl: string) => {
    const next = upsertPreset(llm, kind, { model, baseUrl });
    onChange(assignAll(next.llm, next.provider.id));
  };
  const useBuiltIn = (modelId: string) => {
    const next = upsertPreset(llm, "builtIn", { model: modelId });
    onChange(assignAll(next.llm, next.provider.id));
  };
  const builtInProvider = llm.providers.find((x) => x.enabled && x.kind.type === "builtIn" && [llm.minutesProvider, llm.chatProvider].includes(x.id));
  const builtInUse = builtInProvider?.kind.type === "builtIn" ? builtInProvider.kind.modelId : null;
  const inUse: Partial<Record<LocalKind, string>> = {};
  for (const kind of ["ollama", "lmStudio"] as const) {
    const p = llm.providers.find((x) => x.enabled && presetOf(x) === kind && [llm.minutesProvider, llm.chatProvider].includes(x.id));
    if (p?.kind.type === "openAiCompatible") inUse[kind] = p.kind.model;
  }
  // NaN tiene su tarjeta y los locales su sección: aquí no se repiten.
  const gallery = PRESET_ORDER.filter((id) => id !== "nan" && !PRESETS[id].local && !(SINGLE.includes(id) && has(id)));

  return (
    <Section title="Proveedores de IA" hint="Quién escribe las actas y responde en el chat. La transcripción siempre se hace en tu Mac; esto solo afecta al texto.">
      {writer ? null : (
        <div className="card no-provider" role="note" aria-label="Sin proveedor de IA">
          <CircleAlert size={18} strokeWidth={2} aria-hidden />
          <div>
            <p className="no-provider-title">{llm.providers.length === 0 ? "Aún no has elegido un proveedor de IA" : "Nadie se encarga de escribir las actas"}</p>
            <p className="no-provider-text">
              acta graba y transcribe tus reuniones igual, pero sin proveedor no hay resumen, tareas ni chat.{" "}
              {llm.providers.length === 0 ? "Elige uno más abajo: se añade con un toque y puedes cambiarlo cuando quieras." : "Activa uno y asígnalo en «Quién hace cada cosa»."}
            </p>
          </div>
        </div>
      )}
      {linkError ? (
        <p role="alert" className="card inline-error error">
          {linkError}
        </p>
      ) : null}

      <Group title="Quién hace cada cosa" foot="Solo aparecen los proveedores activados.">
        {task("Acta", "Resumen, decisiones, tareas y escaleta.", "el acta", "minutesProvider")}
        {task("Chat", "Las respuestas de «Preguntar».", "el chat", "chatProvider")}
        {task("Nombres de grupo", "Títulos para los grupos sugeridos.", "los nombres de grupo", "namingProvider")}
      </Group>

      <div className="settings-group">
        <p className="group-title">Tus proveedores</p>
        <div className="stack">
          {llm.providers.map((p) => (
            <ProviderCard
              key={p.id}
              p={p}
              result={tests[p.id]}
              onChange={(np) => onChange({ ...llm, providers: llm.providers.map((x) => (x.id === p.id ? np : x)) })}
              onRemove={() =>
                onChange({
                  ...llm,
                  providers: llm.providers.filter((x) => x.id !== p.id),
                  minutesProvider: llm.minutesProvider === p.id ? null : llm.minutesProvider,
                  chatProvider: llm.chatProvider === p.id ? null : llm.chatProvider,
                  namingProvider: llm.namingProvider === p.id ? null : llm.namingProvider,
                })
              }
              onTest={() => onTest(p.id)}
              onSecretSet={onSecretSet}
              onSecretDelete={onSecretDelete}
              onOpenUrl={openUrl}
            />
          ))}
          {llm.providers.length === 0 ? <p className="card card-empty">Ninguno todavía. Elige uno de los de abajo: se añade con un toque.</p> : null}
        </div>
      </div>

      <div className="settings-group">
        <p className="group-title">Modelo local integrado (opcional)</p>
        <BuiltInModels state={builtIn} inUse={builtInUse} onUse={useBuiltIn} />
      </div>

      <div className="settings-group">
        <p className="group-title">Ollama y LM Studio</p>
        <LocalModels detection={detection} error={localError} inUse={inUse} onUse={useLocal} onRefresh={refresh} onOpenUrl={openUrl} />
      </div>

      <div className="settings-group">
        <p className="group-title">Añadir un proveedor</p>
        <div className="stack">
          {has("nan") ? null : (
            <div className="card nan-promo" role="group" aria-label="NaN Builders">
              <NanIntro onCreateAccount={() => openUrl(NAN_REFERRAL_URL)}>
                <Button capsule variant="primary" icon={<Plus size={13} strokeWidth={2.4} aria-hidden />} onClick={() => add("nan")}>
                  Usar NaN Builders
                </Button>
              </NanIntro>
            </div>
          )}
          <PresetGallery presets={gallery} onAdd={add} />
        </div>
        <div className="provider-add">
          {detecting && detecting !== "pending" ? (
            <span role={detecting.ok ? "status" : "alert"} className={`test-result provider-detect ${detecting.ok ? "ok" : "error"}`}>
              <StatusDot tone={detecting.ok ? "ok" : "warn"} />
              {detecting.text}
            </span>
          ) : null}
          <span className="spacer" />
          <Button small variant="ghost" disabled={detecting === "pending"} title="Busca Claude Code y Codex en tu Mac y rellena su ruta" icon={detecting === "pending" ? <Spinner /> : <ScanSearch size={13} strokeWidth={2} aria-hidden />} onClick={onDetect}>
            Detectar CLI instalados
          </Button>
        </div>
      </div>
    </Section>
  );
}
