//! `cursor-agent -p` (Cursor CLI) en modo `ask`: solo lectura, sin escribir
//! ficheros ni ejecutar comandos. Se lanza el binario del usuario sin tocar
//! sus credenciales, con `cwd` en un directorio temporal vacío.

use super::common::*;
use crate::process::run_supervised;
use acta_core::ids::ProviderId;
use acta_core::llm::*;
use acta_core::{EventStream, PortError, PortResult};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct CursorCliProvider {
    id: ProviderId,
    binary: PathBuf,
    model: Option<String>,
    env: BTreeMap<String, String>,
    timeout: Duration,
}

impl CursorCliProvider {
    pub fn new(
        id: ProviderId,
        binary: PathBuf,
        model: Option<String>,
        env: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Self {
        Self { id, binary, model, env, timeout }
    }

    /// Argumentos exactos (docs/01-arquitectura.md §7.2). El prompt va por stdin.
    /// `--trust` evita la pregunta de confianza del directorio, que bloquearía sin TTY.
    pub fn build_args(&self) -> Vec<String> {
        let mut args: Vec<String> =
            ["-p", "--output-format", "json", "--mode", "ask", "--trust"].iter().map(|s| s.to_string()).collect();
        if let Some(m) = &self.model {
            args.push("--model".into());
            args.push(m.clone());
        }
        args
    }
}

/// Interpreta el JSON de `cursor-agent -p --output-format json`.
pub fn parse_output(stdout: &[u8]) -> PortResult<LlmResponse> {
    let v: serde_json::Value = serde_json::from_slice(stdout).map_err(|e| {
        PortError::InvalidResponse(format!("cursor-agent no devolvió JSON ({e}): {}", tail(stdout)))
    })?;
    if v["is_error"].as_bool().unwrap_or(false) {
        let msg = v["result"].as_str().unwrap_or("error desconocido");
        return Err(PortError::Unavailable(format!("cursor-agent devolvió un error: {msg}")));
    }
    if let Some(sub) = v["subtype"].as_str()
        && sub != "success"
    {
        return Err(PortError::InvalidResponse(format!("cursor-agent terminó sin respuesta ({sub})")));
    }
    let text = v["result"].as_str().map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(PortError::InvalidResponse("cursor-agent no devolvió ningún mensaje".into()));
    }
    Ok(LlmResponse {
        text: text.to_owned(),
        json: None,
        model: None,
        usage: Usage { input_tokens: v["usage"]["inputTokens"].as_u64(), output_tokens: v["usage"]["outputTokens"].as_u64() },
    })
}

/// Ids de `cursor-agent models` (`<id> - <nombre>` por línea). Son los que
/// permite la cuenta del usuario, así que cambian según su plan.
pub fn parse_models(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|l| l.trim().split_once(" - "))
        .map(|(id, _)| id.trim())
        .filter(|id| !id.is_empty() && !id.contains(char::is_whitespace))
        .map(str::to_owned)
        .collect()
}

/// `cursor-agent models`, supervisado. Para «Reintentar con…» y el selector del chat.
pub async fn list_models(binary: &Path, env: &BTreeMap<String, String>, timeout: Duration) -> PortResult<Vec<String>> {
    ensure_binary(binary, "cursor-agent").await?;
    let mut cmd = Command::new(binary);
    cmd.arg("models").envs(env);
    let out = run_supervised(cmd, None, timeout).await?;
    if !out.status.success() {
        return Err(PortError::Unavailable(format!("cursor-agent models falló: {}", tail(&out.stderr))));
    }
    Ok(parse_models(&out.stdout))
}

#[async_trait]
impl LlmProvider for CursorCliProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities { streaming: false, native_json_schema: false, context_tokens: 200_000 }
    }

    async fn complete(&self, request: LlmRequest) -> PortResult<LlmResponse> {
        ensure_binary(&self.binary, "cursor-agent").await?;
        let dir = temp_dir()?;
        // Sin flags de system prompt ni de esquema: ambos van delante en el prompt.
        let mut prompt = String::new();
        if let Some(sys) = system_with_schema(&request, true) {
            prompt.push_str(&sys);
            prompt.push_str("\n\n---\n\n");
        }
        prompt.push_str(&flatten_messages(&request.messages));

        let mut cmd = Command::new(&self.binary);
        cmd.args(self.build_args()).envs(&self.env).current_dir(dir.path());
        let out = run_supervised(cmd, Some(prompt.into_bytes()), self.timeout).await?;
        if !out.status.success() {
            let code = out.status.code().map_or("señal".to_string(), |c| c.to_string());
            let detail = if out.stderr.is_empty() { tail(&out.stdout) } else { tail(&out.stderr) };
            return Err(PortError::Unavailable(format!("cursor-agent terminó con error (código {code}): {detail}")));
        }
        let mut r = parse_output(&out.stdout)?;
        r.model = self.model.clone();
        r.json = request.json_schema.as_ref().and_then(|_| crate::structured::extract_json(&r.text));
        Ok(r)
    }

    async fn stream(&self, request: LlmRequest) -> PortResult<EventStream<PortResult<String>>> {
        Ok(single_chunk(self.complete(request).await?.text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    fn provider(bin: PathBuf, model: Option<&str>, env: &[(&str, &str)]) -> CursorCliProvider {
        CursorCliProvider::new(
            "cursor-cli".into(),
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
    fn args_are_print_ask_mode_and_trusted() {
        let args = provider("/x/cursor-agent".into(), Some("gpt-5.2"), &[]).build_args();
        assert_eq!(args, ["-p", "--output-format", "json", "--mode", "ask", "--trust", "--model", "gpt-5.2"]);
        let args = provider("/x/cursor-agent".into(), None, &[]).build_args();
        assert!(!args.iter().any(|a| a == "--model" || a == "--force" || a == "--yolo"));
    }

    #[tokio::test]
    async fn pipes_system_schema_and_prompt_and_reads_result() {
        let dir = tempfile::tempdir().expect("dir");
        let bin = fake_script(
            dir.path(),
            "cursor-agent",
            r#"
cat > "$OUT/stdin.txt"
echo '{"type":"result","subtype":"success","is_error":false,"result":"{\"a\": 5}","usage":{"inputTokens":7,"outputTokens":3}}'
"#,
        );
        let out = dir.path().join("out");
        std::fs::create_dir(&out).expect("mkdir");
        let r = provider(bin, Some("auto"), &[("OUT", out.to_str().expect("utf8"))]).complete(req()).await.expect("complete");
        assert_eq!(r.json.expect("json")["a"], 5);
        assert_eq!((r.usage.input_tokens, r.usage.output_tokens), (Some(7), Some(3)));
        assert_eq!(r.model.as_deref(), Some("auto"));
        let stdin = std::fs::read_to_string(out.join("stdin.txt")).expect("stdin");
        assert!(stdin.starts_with("SISTEMA") && stdin.contains("integer") && stdin.ends_with("pregunta"), "{stdin}");
        assert!(!stdin.contains("$schema"));
    }

    #[test]
    fn is_error_and_empty_result_are_errors() {
        let e = parse_output(br#"{"type":"result","subtype":"success","is_error":true,"result":"quota"}"#).expect_err("err");
        assert!(matches!(e, PortError::Unavailable(m) if m.contains("quota")));
        let e = parse_output(br#"{"type":"result","subtype":"success","is_error":false,"result":"  "}"#).expect_err("err");
        assert!(matches!(e, PortError::InvalidResponse(_)));
        assert!(matches!(parse_output(b"no json"), Err(PortError::InvalidResponse(_))));
    }

    #[tokio::test]
    async fn failure_and_missing_binary_are_clear() {
        let dir = tempfile::tempdir().expect("dir");
        let bin = fake_script(dir.path(), "cursor-agent", "cat > /dev/null\necho 'Cannot use this model: x.' >&2\nexit 1\n");
        let e = provider(bin, None, &[]).complete(req()).await.expect_err("err");
        assert!(matches!(e, PortError::Unavailable(m) if m.contains("código 1") && m.contains("Cannot use this model")));
        let e = provider("/no/existe/cursor-agent".into(), None, &[]).complete(req()).await.expect_err("err");
        assert!(matches!(e, PortError::Unavailable(m) if m.contains("cursor-agent no encontrado en /no/existe/cursor-agent")));
    }

    #[test]
    fn models_output_keeps_only_ids() {
        let raw = b"Available models\n\nauto - Auto (default)\ngpt-5.3-codex - Codex 5.3\nclaude-opus-5-thinking-high - Claude Opus 5 1M Thinking\n\nTip: use --model <id> (or /model <id> in interactive mode) to switch. Parameterized models also accept quoted overrides, e.g. --model 'x'.\n";
        assert_eq!(parse_models(raw), ["auto", "gpt-5.3-codex", "claude-opus-5-thinking-high"]);
    }

    #[tokio::test]
    async fn list_models_runs_the_models_subcommand() {
        let dir = tempfile::tempdir().expect("dir");
        let bin = fake_script(dir.path(), "cursor-agent", "[ \"$1\" = models ] || exit 2\necho 'auto - Auto (default)'\n");
        assert_eq!(list_models(&bin, &BTreeMap::new(), Duration::from_secs(5)).await.expect("ok"), ["auto"]);
    }
}
