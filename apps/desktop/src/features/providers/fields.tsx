import { RotateCw } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import type { SecretRef } from "../../api";
import { Button, Select, Spinner, errorMessage } from "../../shared/ui/ui";
import { Field } from "../settings/fields";

/** Clave de API: nunca se guarda en los ajustes, solo la referencia al Llavero. */
export function ApiKeyField({ label, ref_, onSave, onDelete }: { label: string; ref_: SecretRef | null; onSave(value: string): Promise<void>; onDelete?(): Promise<void> }) {
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const run = async (fn: () => Promise<void>) => {
    setError(null);
    try {
      await fn();
    } catch (e) {
      setError(errorMessage(e));
    }
  };
  return (
    <Field label="Clave de API" hint={ref_ ? "Guardada en el Llavero de macOS. No se muestra ni se copia a los ajustes." : "Se guarda en el Llavero de macOS, nunca en los ajustes."}>
      <div className="field-line">
        <input
          type="password"
          autoComplete="off"
          aria-label={`Clave de API de ${label}`}
          placeholder={ref_ ? "••••••••  (ya hay una guardada)" : "Pega aquí la clave"}
          value={value}
          onChange={(e) => setValue(e.target.value)}
        />
        <Button
          variant="primary"
          aria-label={`Guardar en el Llavero ${label}`}
          disabled={!value}
          onClick={() =>
            void run(async () => {
              await onSave(value);
              setValue("");
            })
          }
        >
          Guardar en el Llavero
        </Button>
        {onDelete && ref_ ? (
          <Button variant="ghost" aria-label={`Borrar clave ${label}`} onClick={() => void run(onDelete)}>
            Borrar clave
          </Button>
        ) : null}
      </div>
      {error ? (
        <span role="alert" className="error">
          {error}
        </span>
      ) : null}
    </Field>
  );
}

interface ModelFieldProps {
  /** Nombre del proveedor, para las etiquetas accesibles. */
  label: string;
  value: string;
  onChange(model: string): void;
  /** Pide la lista de modelos al servidor. */
  load(): Promise<string[]>;
  /** Cada vez que cambia (y no es 0) se vuelve a pedir la lista. */
  reloadKey?: number;
  /** Modelos que se proponen primero si el actual no está en la lista. */
  prefer?: string[];
  hint?: ReactNode;
  placeholder?: string;
  /** Si está, la primera opción es «vacío» con este texto (el modelo por defecto del proveedor) y la lista no cambia la elección. */
  defaultLabel?: string;
}

/** Modelo de un servidor: un desplegable con lo que ofrece, o un campo de texto si no se puede preguntar. */
export function ModelField({ label, value, onChange, load, reloadKey = 0, prefer = [], hint, placeholder, defaultLabel }: ModelFieldProps) {
  const [models, setModels] = useState<string[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // La lista llega más tarde: se usa siempre lo último que nos pasaron.
  const latest = useRef({ value, onChange, load, prefer });
  latest.current = { value, onChange, load, prefer };

  const run = async () => {
    setBusy(true);
    setError(null);
    try {
      const list = await latest.current.load();
      if (list.length === 0) {
        setModels(null);
        setError("El servidor no ofrece ningún modelo de texto.");
        return;
      }
      setModels(list);
      const now = latest.current;
      if (defaultLabel === undefined && !list.includes(now.value)) now.onChange(now.prefer.find((m) => list.includes(m)) ?? list[0]!);
    } catch (e) {
      setModels(null);
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    if (reloadKey > 0) void run();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reloadKey]);

  return (
    <Field label="Modelo" hint={hint}>
      <div className="field-line model-field">
        {models ? (
          <Select label={`Modelo de ${label}`} value={value} onChange={(e) => onChange(e.target.value)}>
            {defaultLabel !== undefined ? <option value="">{defaultLabel}</option> : null}
            {/* Un modelo guardado que ya no sale en la lista se conserva. */}
            {(defaultLabel !== undefined && value && !models.includes(value) ? [value, ...models] : models).map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </Select>
        ) : (
          <input aria-label={`Modelo de ${label}`} placeholder={placeholder ?? "Nombre del modelo"} spellCheck={false} value={value} onChange={(e) => onChange(e.target.value)} />
        )}
        <Button aria-label={`${models ? "Actualizar" : "Ver"} modelos de ${label}`} title="Pregunta al servidor qué modelos tiene" disabled={busy} icon={busy ? <Spinner /> : models ? <RotateCw size={13} strokeWidth={2.2} aria-hidden /> : null} onClick={() => void run()}>
          {busy ? "Buscando…" : models ? "Actualizar" : "Ver modelos"}
        </Button>
      </div>
      {error ? (
        <span role="alert" className="error">
          {error}
        </span>
      ) : null}
    </Field>
  );
}
