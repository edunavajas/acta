//! `claude -p` (Claude Code CLI). Se lanza el binario del usuario tal cual,
//! con su entorno (+ `extra_env`, p. ej. `CLAUDE_CONFIG_DIR`). Nunca se tocan
//! credenciales y NO se usa `--bare` (ignoraría el login OAuth).

use super::common::*;
use crate::process::run_supervised;
use acta_core::ids::ProviderId;
use acta_core::llm::*;
use acta_core::{EventStream, PortError, PortResult};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct ClaudeCliProvider {
    id: ProviderId,
    binary: PathBuf,
    model: Option<String>,
    effort: Option<String>,
    env: BTreeMap<String, String>,
    timeout: Duration,
}

impl ClaudeCliProvider {
    pub fn new(
        id: ProviderId,
        binary: PathBuf,
        model: Option<String>,
        env: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Self {
        Self { id, binary, model, effort: None, env, timeout }
    }

    /// `--effort <nivel>` (low, medium, high, xhigh, max). Vacío = sin flag.
    pub fn with_effort(mut self, effort: Option<String>) -> Self {
        self.effort = effort.map(|e| e.trim().to_owned()).filter(|e| !e.is_empty());
        self
    }

    /// Argumentos exactos (docs/01-arquitectura.md §7.2). El prompt va por stdin.
    /// `--safe-mode`: sin hooks, skills ni CLAUDE.md del usuario (la
    /// autenticación y el modelo por defecto se mantienen).
    pub fn build_args(&self, request: &LlmRequest) -> Vec<String> {
        let mut args: Vec<String> = ["-p", "--safe-mode", "--output-format", "json", "--tools", "", "--strict-mcp-config", "--no-session-persistence"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        if let Some(schema) = &request.json_schema {
            args.push("--json-schema".into());
            args.push(wire_schema(schema).to_string());
        }
        if let Some(sys) = &request.system {
            args.push("--system-prompt".into());
            args.push(sys.clone());
        }
        if let Some(m) = &self.model {
            args.push("--model".into());
            args.push(m.clone());
        }
        if let Some(e) = &self.effort {
            args.push("--effort".into());
            args.push(e.clone());
        }
        args
    }
}

/// Interpreta el JSON de `claude -p --output-format json`.
pub fn parse_output(stdout: &[u8]) -> PortResult<LlmResponse> {
    let v: serde_json::Value = serde_json::from_slice(stdout).map_err(|e| {
        PortError::InvalidResponse(format!("claude no devolvió JSON ({e}): {}", tail(stdout)))
    })?;
    if v["is_error"].as_bool().unwrap_or(false) {
        let msg = v["result"].as_str().unwrap_or("error desconocido");
        return Err(PortError::Unavailable(format!("claude devolvió un error: {msg}")));
    }
    if let Some(sub) = v["subtype"].as_str()
        && sub != "success"
    {
        let msg = v["result"].as_str().unwrap_or_default();
        return Err(PortError::InvalidResponse(format!("claude terminó sin respuesta ({sub}): {}", msg.chars().take(200).collect::<String>())));
    }
    let structured = v.get("structured_output").filter(|s| !s.is_null()).cloned();
    let text = match (v["result"].as_str(), &structured) {
        (Some(t), _) if !t.is_empty() => t.to_owned(),
        (_, Some(s)) => s.to_string(),
        _ => return Err(PortError::InvalidResponse("claude: respuesta sin campo `result`".into())),
    };
    let model = v["modelUsage"]
        .as_object()
        .and_then(|m| m.keys().next().cloned());
    Ok(LlmResponse {
        text,
        json: structured,
        model,
        usage: Usage {
            input_tokens: v["usage"]["input_tokens"].as_u64(),
            output_tokens: v["usage"]["output_tokens"].as_u64(),
        },
    })
}

#[async_trait]
impl LlmProvider for ClaudeCliProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities { streaming: false, native_json_schema: true, context_tokens: 200_000 }
    }

    async fn complete(&self, request: LlmRequest) -> PortResult<LlmResponse> {
        ensure_binary(&self.binary, "claude").await?;
        let dir = temp_dir()?;
        let mut cmd = Command::new(&self.binary);
        cmd.args(self.build_args(&request)).envs(&self.env).current_dir(dir.path());
        let prompt = flatten_messages(&request.messages);
        let out = run_supervised(cmd, Some(prompt.into_bytes()), self.timeout).await?;
        if !out.status.success() {
            let code = out.status.code().map_or("señal".to_string(), |c| c.to_string());
            let detail = if out.stderr.is_empty() { tail(&out.stdout) } else { tail(&out.stderr) };
            return Err(PortError::Unavailable(format!("claude terminó con error (código {code}): {detail}")));
        }
        parse_output(&out.stdout)
    }

    async fn stream(&self, request: LlmRequest) -> PortResult<EventStream<PortResult<String>>> {
        Ok(single_chunk(self.complete(request).await?.text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    fn provider(bin: PathBuf, model: Option<&str>, env: &[(&str, &str)]) -> ClaudeCliProvider {
        ClaudeCliProvider::new(
            "claude-cli".into(),
            bin,
            model.map(str::to_owned),
            env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            Duration::from_secs(10),
        )
    }

    fn req(schema: bool) -> LlmRequest {
        LlmRequest {
            system: Some("Eres un secretario".into()),
            messages: vec![LlmMessage { role: Role::User, content: "Resume esto".into() }],
            json_schema: schema.then(|| serde_json::json!({"type":"object"})),
            max_tokens: None,
            temperature: None,
        }
    }

    #[test]
    fn args_follow_the_contract_and_never_use_bare() {
        let p = provider("/x/claude".into(), Some("opus"), &[]);
        let args = p.build_args(&req(true));
        assert_eq!(&args[..8], ["-p", "--safe-mode", "--output-format", "json", "--tools", "", "--strict-mcp-config", "--no-session-persistence"]);
        assert!(args.windows(2).any(|w| w == ["--json-schema", "{\"type\":\"object\"}"]));
        assert!(args.windows(2).any(|w| w == ["--system-prompt", "Eres un secretario"]));
        assert!(args.windows(2).any(|w| w == ["--model", "opus"]));
        assert!(!args.iter().any(|a| a == "--bare"));
        let no_schema = p.build_args(&req(false));
        assert!(!no_schema.iter().any(|a| a == "--json-schema"));
    }

    #[test]
    fn json_schema_arg_never_carries_meta_schema() {
        let p = provider("/x/claude".into(), None, &[]);
        let mut r = req(false);
        r.json_schema = Some(serde_json::json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"a":{"type":"integer"}}}));
        let args = p.build_args(&r);
        let i = args.iter().position(|a| a == "--json-schema").expect("flag");
        assert!(!args[i + 1].contains("$schema"), "{}", args[i + 1]);
        assert!(args[i + 1].contains("integer"));
    }

    /// Con los hooks y skills del usuario (que viven en ~/Documents), macOS
    /// pregunta «acta.app quiere acceder a tu carpeta Documentos» y el
    /// resumen se queda parado hasta que alguien contesta.
    #[test]
    fn user_customizations_are_never_loaded() {
        let p = provider("/x/claude".into(), None, &[]);
        assert!(p.build_args(&req(false)).iter().any(|a| a == "--safe-mode"));
        assert!(p.build_args(&req(true)).iter().any(|a| a == "--safe-mode"));
    }

    #[test]
    fn effort_is_passed_only_when_configured() {
        let p = provider("/x/claude".into(), None, &[]).with_effort(Some("high".into()));
        assert!(p.build_args(&req(false)).windows(2).any(|w| w == ["--effort", "high"]));
        let p = provider("/x/claude".into(), None, &[]).with_effort(Some("  ".into()));
        assert!(!p.build_args(&req(false)).iter().any(|a| a == "--effort"));
        let p = provider("/x/claude".into(), None, &[]);
        assert!(!p.build_args(&req(false)).iter().any(|a| a == "--effort"));
    }

    #[test]
    fn parses_result_usage_and_structured_output() {
        let r = parse_output(br#"{"type":"result","is_error":false,"result":"hola","usage":{"input_tokens":5,"output_tokens":2},"modelUsage":{"claude-opus":{}}}"#).expect("ok");
        assert_eq!(r.text, "hola");
        assert_eq!(r.usage.input_tokens, Some(5));
        assert_eq!(r.model.as_deref(), Some("claude-opus"));
        assert!(r.json.is_none());
        let r = parse_output(br#"{"is_error":false,"result":"","structured_output":{"a":1}}"#).expect("ok");
        assert_eq!(r.json.expect("json")["a"], 1);
    }

    #[test]
    fn a_non_success_subtype_is_reported_with_its_reason() {
        let e = parse_output(br#"{"type":"result","subtype":"error_max_structured_output_retries","is_error":false,"result":"No pude"}"#).expect_err("err");
        assert!(matches!(&e, PortError::InvalidResponse(m) if m.contains("error_max_structured_output_retries")), "{e}");
    }

    #[test]
    fn error_flag_and_garbage_are_errors() {
        let e = parse_output(br#"{"is_error":true,"result":"Not logged in"}"#).expect_err("err");
        assert!(matches!(e, PortError::Unavailable(m) if m.contains("Not logged in")));
        assert!(matches!(parse_output(b"basura"), Err(PortError::InvalidResponse(_))));
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn runs_fake_binary_with_stdin_args_env_and_temp_cwd() {
        let dir = tempfile::tempdir().expect("dir");
        let bin = fake_script(
            dir.path(),
            "claude",
            r#"
printf '%s\n' "$@" > "$OUT/args.txt"
cat > "$OUT/stdin.txt"
pwd > "$OUT/cwd.txt"
echo "$MY_FLAG" > "$OUT/env.txt"
cat <<'JSON'
{"type":"result","is_error":false,"result":"```json\n{\"ok\":true}\n```","usage":{"input_tokens":3,"output_tokens":4}}
JSON
"#,
        );
        let out = dir.path().join("out");
        std::fs::create_dir(&out).expect("mkdir");
        let p = provider(bin, Some("sonnet"), &[("OUT", out.to_str().expect("utf8")), ("MY_FLAG", "alias-env")]);
        let r = p.complete(req(true)).await.expect("complete");
        assert!(r.text.contains("ok"));
        assert_eq!(std::fs::read_to_string(out.join("stdin.txt")).expect("stdin"), "Resume esto");
        assert!(std::fs::read_to_string(out.join("args.txt")).expect("args").contains("--no-session-persistence"));
        assert_eq!(std::fs::read_to_string(out.join("env.txt")).expect("env").trim(), "alias-env");
        let cwd = std::fs::read_to_string(out.join("cwd.txt")).expect("cwd");
        assert_ne!(cwd.trim(), dir.path().to_str().expect("utf8"));
        let chunks: Vec<_> = futures::StreamExt::collect(p.stream(req(false)).await.expect("stream")).await;
        assert_eq!(chunks.len(), 1);
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn nonzero_exit_reports_code_and_stderr() {
        let dir = tempfile::tempdir().expect("dir");
        let bin = fake_script(dir.path(), "claude", "echo 'Invalid API key' >&2\nexit 2\n");
        let e = provider(bin, None, &[]).complete(req(false)).await.expect_err("err");
        assert!(matches!(e, PortError::Unavailable(m) if m.contains("código 2") && m.contains("Invalid API key")));
    }

    #[tokio::test]
    async fn missing_binary_is_clear() {
        let e = provider("/no/existe/claude".into(), None, &[]).complete(req(false)).await.expect_err("err");
        assert!(matches!(e, PortError::Unavailable(m) if m.contains("claude no encontrado en /no/existe/claude")));
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn hanging_binary_times_out() {
        let dir = tempfile::tempdir().expect("dir");
        let bin = fake_script(dir.path(), "claude", "exec sleep 5\n");
        let mut p = provider(bin, None, &[]);
        p.timeout = Duration::from_millis(150);
        let e = p.complete(req(false)).await.expect_err("err");
        assert!(matches!(e, PortError::Timeout(_)));
    }
}
