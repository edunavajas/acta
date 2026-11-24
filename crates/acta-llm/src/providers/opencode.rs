//! `opencode run --archived --format json` (OpenCode CLI) no interactivo.
//! Se lanza el binario del usuario en un directorio temporal con un
//! `opencode.json` que desactiva snapshots y deniega herramientas, para usarlo
//! como motor puro de texto/JSON sin tocar archivos ni credenciales.

use super::common::*;
use crate::process::run_supervised;
use acta_core::ids::ProviderId;
use acta_core::llm::*;
use acta_core::{EventStream, PortError, PortResult};
use async_trait::async_trait;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

const ISOLATED_OPENCODE_CONFIG: &str = r#"{
  "$schema": "https://opencode.ai/config.json",
  "snapshots": false,
  "permissions": [
    { "action": "*", "resource": "*", "effect": "deny" }
  ]
}
"#;

#[derive(Debug, Clone)]
pub struct OpenCodeCliProvider {
    id: ProviderId,
    binary: PathBuf,
    model: Option<String>,
    env: BTreeMap<String, String>,
    timeout: Duration,
}

impl OpenCodeCliProvider {
    pub fn new(
        id: ProviderId,
        binary: PathBuf,
        model: Option<String>,
        env: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Self {
        let model = model.map(|m| m.trim().to_owned()).filter(|m| !m.is_empty());
        Self { id, binary, model, env, timeout }
    }

    /// Argumentos de `opencode run`. El prompt va por stdin.
    pub fn build_args(&self) -> Vec<String> {
        Self::build_args_for(self.model.as_deref())
    }

    fn build_args_for(model: Option<&str>) -> Vec<String> {
        let mut args: Vec<String> = ["run", "--archived", "--format", "json"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        if let Some(m) = model.map(str::trim).filter(|m| !m.is_empty()) {
            args.push("-m".into());
            args.push(m.to_owned());
        }
        args
    }
}

/// ¿Es un modelo del tier gratuito integrado de OpenCode (`opencode/*-free`)?
fn is_opencode_free_model(model: &str) -> bool {
    model.starts_with("opencode/") && model.ends_with("-free")
}

/// Quita comentarios `//` de línea para poder leer `opencode.jsonc` sencillos.
fn strip_jsonc_line_comments(raw: &str) -> String {
    raw.lines()
        .map(|line| {
            let mut in_str = false;
            let mut escaped = false;
            let bytes = line.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                let b = bytes[i];
                if in_str {
                    if escaped {
                        escaped = false;
                    } else if b == b'\\' {
                        escaped = true;
                    } else if b == b'"' {
                        in_str = false;
                    }
                } else if b == b'"' {
                    in_str = true;
                } else if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    return &line[..i];
                }
                i += 1;
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Resuelve el modelo por defecto configurado en OpenCode para `home`:
/// 1. Campo `"model"` en `~/.config/opencode/opencode.json` (o `.jsonc`).
/// 2. Último modelo usado en `~/.local/state/opencode/model.json` (`recent`),
///    priorizando proveedores conectados frente al tier gratuito `opencode/*-free`.
pub fn resolve_default_model(home: &Path) -> Option<String> {
    let config_dir = home.join(".config/opencode");
    for name in ["opencode.json", "opencode.jsonc"] {
        let path = config_dir.join(name);
        if let Ok(raw) = std::fs::read_to_string(&path) {
            let cleaned = strip_jsonc_line_comments(&raw);
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&cleaned)
                && let Some(m) = v["model"].as_str().map(str::trim).filter(|m| !m.is_empty())
            {
                return Some(m.to_owned());
            }
        }
    }

    let state_path = home.join(".local/state/opencode/model.json");
    if let Ok(raw) = std::fs::read_to_string(&state_path)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
        && let Some(recent) = v["recent"].as_array()
    {
        let candidates: Vec<String> = recent
            .iter()
            .filter_map(|item| {
                let p = item["providerID"].as_str()?.trim();
                let m = item["modelID"].as_str()?.trim();
                if p.is_empty() || m.is_empty() { None } else { Some(format!("{p}/{m}")) }
            })
            .collect();
        if let Some(non_free) = candidates.iter().find(|m| !is_opencode_free_model(m)) {
            return Some(non_free.clone());
        }
        if let Some(first) = candidates.into_iter().next() {
            return Some(first);
        }
    }

    None
}

/// Parsea la salida de `opencode models` (un `proveedor/modelo` por línea).
/// Pone primero los proveedores conectados y deja al final los `opencode/*-free`.
pub fn parse_models_output(stdout: &[u8]) -> Vec<String> {
    let raw = String::from_utf8_lossy(stdout);
    let mut seen = HashSet::new();
    let mut regular = Vec::new();
    let mut free = Vec::new();

    for line in raw.lines() {
        let m = line.trim();
        if m.is_empty() || !m.contains('/') || m.contains(char::is_whitespace) {
            continue;
        }
        if !seen.insert(m.to_owned()) {
            continue;
        }
        if is_opencode_free_model(m) {
            free.push(m.to_owned());
        } else {
            regular.push(m.to_owned());
        }
    }

    regular.extend(free);
    regular
}

/// Consulta `opencode models` de forma supervisada para poblar el selector en Ajustes.
pub async fn list_models(
    binary: &Path,
    env: &BTreeMap<String, String>,
    timeout: Duration,
) -> PortResult<Vec<String>> {
    ensure_binary(binary, "opencode").await?;
    let mut cmd = Command::new(binary);
    cmd.arg("models").envs(env);
    let out = run_supervised(cmd, None, timeout).await?;
    if !out.status.success() {
        let detail = if out.stderr.is_empty() { tail(&out.stdout) } else { tail(&out.stderr) };
        return Err(PortError::Unavailable(format!("opencode models falló: {detail}")));
    }
    Ok(parse_models_output(&out.stdout))
}

/// Interpreta el stream NDJSON de `opencode run --format json`.
pub fn parse_output(stdout: &[u8], model: Option<String>) -> PortResult<LlmResponse> {
    let raw = String::from_utf8_lossy(stdout);
    let mut chunks: Vec<String> = Vec::new();
    let mut last_error: Option<String> = None;
    let mut input_tokens: Option<u64> = None;
    let mut output_tokens: Option<u64> = None;

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match v["type"].as_str() {
            Some("text") => {
                if let Some(t) = v["part"]["text"].as_str() {
                    chunks.push(t.to_owned());
                }
            }
            Some("error") => {
                let msg = v["error"]["message"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v["error"].to_string());
                last_error = Some(msg);
            }
            Some("step_finish") => {
                if let Some(i) = v["part"]["tokens"]["input"].as_u64() {
                    input_tokens = Some(input_tokens.unwrap_or(0) + i);
                }
                if let Some(o) = v["part"]["tokens"]["output"].as_u64() {
                    output_tokens = Some(output_tokens.unwrap_or(0) + o);
                }
            }
            _ => {}
        }
    }

    let text = chunks.concat().trim().to_owned();
    if let Some(err) = last_error
        && text.is_empty()
    {
        let hint = if err.to_lowercase().contains("free usage limit") {
            " Elige un modelo de tu proveedor conectado en Ajustes › Proveedores de IA › OpenCode (CLI)."
        } else {
            ""
        };
        return Err(PortError::Unavailable(format!("opencode devolvió un error: {err}{hint}")));
    }
    if text.is_empty() {
        return Err(PortError::InvalidResponse("opencode no devolvió ningún mensaje".into()));
    }
    Ok(LlmResponse {
        text,
        json: None,
        model,
        usage: Usage { input_tokens, output_tokens },
    })
}

#[async_trait]
impl LlmProvider for OpenCodeCliProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities { streaming: false, native_json_schema: false, context_tokens: 200_000 }
    }

    async fn complete(&self, request: LlmRequest) -> PortResult<LlmResponse> {
        ensure_binary(&self.binary, "opencode").await?;
        let dir = temp_dir()?;
        tokio::fs::write(dir.path().join("opencode.json"), ISOLATED_OPENCODE_CONFIG).await?;

        let effective_model = self.model.clone().or_else(|| {
            let home = self
                .env
                .get("HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from))?;
            resolve_default_model(&home)
        });

        let mut prompt = String::new();
        if let Some(sys) = system_with_schema(&request, true) {
            prompt.push_str(&sys);
            prompt.push_str("\n\n---\n\n");
        }
        prompt.push_str(&flatten_messages(&request.messages));

        let mut cmd = Command::new(&self.binary);
        cmd.args(Self::build_args_for(effective_model.as_deref()))
            .envs(&self.env)
            .current_dir(dir.path());
        let out = run_supervised(cmd, Some(prompt.into_bytes()), self.timeout).await?;

        let parsed = parse_output(&out.stdout, effective_model);
        match (out.status.success(), parsed) {
            (_, Ok(mut resp)) => {
                if request.json_schema.is_some() {
                    resp.json = crate::structured::extract_json(&resp.text);
                }
                Ok(resp)
            }
            (false, Err(PortError::Unavailable(msg))) => Err(PortError::Unavailable(msg)),
            (false, Err(_)) => {
                let code = out.status.code().map_or("señal".to_string(), |c| c.to_string());
                let detail = if out.stderr.is_empty() { tail(&out.stdout) } else { tail(&out.stderr) };
                Err(PortError::Unavailable(format!("opencode terminó con error (código {code}): {detail}")))
            }
            (true, Err(e)) => Err(e),
        }
    }

    async fn stream(&self, request: LlmRequest) -> PortResult<EventStream<PortResult<String>>> {
        Ok(single_chunk(self.complete(request).await?.text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    fn provider(bin: PathBuf, model: Option<&str>, env: &[(&str, &str)]) -> OpenCodeCliProvider {
        OpenCodeCliProvider::new(
            "opencode-cli".into(),
            bin,
            model.map(str::to_owned),
            env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            Duration::from_secs(10),
        )
    }

    fn req() -> LlmRequest {
        LlmRequest {
            system: Some("SISTEMA".into()),
            messages: vec![LlmMessage { role: Role::User, content: "pregunta".into() }],
            json_schema: Some(serde_json::json!({"type":"object","properties":{"a":{"type":"integer"}}})),
            max_tokens: None,
            temperature: None,
        }
    }

    #[test]
    fn args_use_archived_json_and_optional_model() {
        let p = provider("/x/opencode".into(), Some("github-copilot/gemini-4-neon"), &[]);
        assert_eq!(p.build_args(), ["run", "--archived", "--format", "json", "-m", "github-copilot/gemini-4-neon"]);
        let p = provider("/x/opencode".into(), Some("   "), &[]);
        assert_eq!(p.build_args(), ["run", "--archived", "--format", "json"]);
    }

    #[test]
    fn parses_ndjson_text_and_usage() {
        let ndjson = concat!(
            "{\"type\":\"step_start\",\"part\":{\"type\":\"step-start\"}}\n",
            "{\"type\":\"text\",\"part\":{\"type\":\"text\",\"text\":\"hola \"}}\n",
            "{\"type\":\"text\",\"part\":{\"type\":\"text\",\"text\":\"mundo\"}}\n",
            "{\"type\":\"step_finish\",\"part\":{\"tokens\":{\"input\":120,\"output\":8}}}\n"
        );
        let resp = parse_output(ndjson.as_bytes(), Some("github-copilot/gemini-4-neon".into())).expect("ok");
        assert_eq!(resp.text, "hola mundo");
        assert_eq!(resp.usage.input_tokens, Some(120));
        assert_eq!(resp.usage.output_tokens, Some(8));
        assert_eq!(resp.model.as_deref(), Some("github-copilot/gemini-4-neon"));
    }

    #[test]
    fn surfaces_ndjson_error_event_with_hint_on_free_limit() {
        let ndjson = "{\"type\":\"error\",\"error\":{\"message\":\"Free usage limit reached.\"}}\n";
        let err = parse_output(ndjson.as_bytes(), None).expect_err("err");
        assert!(matches!(err, PortError::Unavailable(m) if m.contains("Free usage limit reached.") && m.contains("Ajustes")));
    }

    #[test]
    fn resolves_default_model_from_config_or_recent_state() {
        let dir = tempfile::tempdir().expect("dir");
        let home = dir.path();
        assert_eq!(resolve_default_model(home), None);

        // Sin `model` en opencode.json, cae al primer modelo no-free en `recent`.
        std::fs::create_dir_all(home.join(".config/opencode")).expect("cfgdir");
        std::fs::write(home.join(".config/opencode/opencode.json"), r#"{"permissions":[]}"#).expect("cfg");
        std::fs::create_dir_all(home.join(".local/state/opencode")).expect("statedir");
        std::fs::write(
            home.join(".local/state/opencode/model.json"),
            r#"{"recent":[{"providerID":"opencode","modelID":"beam-free"},{"providerID":"github-copilot","modelID":"gemini-4-neon"}]}"#,
        )
        .expect("state");
        assert_eq!(resolve_default_model(home).as_deref(), Some("github-copilot/gemini-4-neon"));

        // Con `model` explícito en opencode.json, ese gana.
        std::fs::write(
            home.join(".config/opencode/opencode.json"),
            r#"{"model":"anthropic/claude-sonnet-5.5"}"#,
        )
        .expect("cfg");
        assert_eq!(resolve_default_model(home).as_deref(), Some("anthropic/claude-sonnet-5.5"));
    }

    #[test]
    fn parses_models_output_putting_free_tier_last() {
        let raw = b"opencode/beam-free\ngithub-copilot/gemini-4-neon\ngithub-copilot/claude-sonnet-5.5\nopencode/beam-free\nnoise line\n";
        assert_eq!(
            parse_models_output(raw),
            vec!["github-copilot/gemini-4-neon", "github-copilot/claude-sonnet-5.5", "opencode/beam-free"]
        );
    }

    #[tokio::test]
    async fn writes_isolated_config_embeds_schema_and_extracts_json() {
        let dir = tempfile::tempdir().expect("dir");
        let out = dir.path().join("out");
        std::fs::create_dir(&out).expect("mkdir");
        let bin = fake_script(
            dir.path(),
            "opencode",
            r#"
cp opencode.json "$OUT/opencode.json"
cat > "$OUT/stdin.txt"
printf '%s\n' '{"type":"text","part":{"type":"text","text":"```json\n{\"a\": 7}\n```"}}'
printf '%s\n' '{"type":"step_finish","part":{"tokens":{"input":50,"output":5}}}'
"#,
        );
        let p = provider(bin, Some("github-copilot/gemini-4-neon"), &[("OUT", out.to_str().expect("utf8"))]);
        let r = p.complete(req()).await.expect("complete");
        assert_eq!(r.json.expect("json")["a"], 7);
        assert_eq!(r.usage.input_tokens, Some(50));
        let stdin = std::fs::read_to_string(out.join("stdin.txt")).expect("stdin");
        assert!(stdin.starts_with("SISTEMA"));
        assert!(stdin.contains("\"integer\""));
        assert!(stdin.ends_with("pregunta"));
        let cfg = std::fs::read_to_string(out.join("opencode.json")).expect("cfg");
        assert!(cfg.contains("\"effect\": \"deny\""));
    }
}
