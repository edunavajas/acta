//! `codex exec` (Codex CLI) no interactivo, sandbox de solo lectura.
//! Se lanza el binario del usuario sin tocar sus credenciales.

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
pub struct CodexCliProvider {
    id: ProviderId,
    binary: PathBuf,
    model: Option<String>,
    env: BTreeMap<String, String>,
    timeout: Duration,
}

impl CodexCliProvider {
    pub fn new(
        id: ProviderId,
        binary: PathBuf,
        model: Option<String>,
        env: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Self {
        Self { id, binary, model, env, timeout }
    }

    /// `schema_file` solo si hay esquema; `out_file` es donde Codex deja el último mensaje.
    pub fn build_args(&self, schema_file: Option<&Path>, out_file: &Path) -> Vec<String> {
        let mut args: Vec<String> = ["exec", "--skip-git-repo-check", "--ephemeral", "--sandbox", "read-only"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        if let Some(s) = schema_file {
            args.push("--output-schema".into());
            args.push(s.display().to_string());
        }
        args.push("-o".into());
        args.push(out_file.display().to_string());
        if let Some(m) = &self.model {
            args.push("-m".into());
            args.push(m.clone());
        }
        args.push("-".into());
        args
    }
}

/// Codex mezcla en stderr el progreso de sus hooks con el error real
/// (`ERROR: {json}` o `ERROR: texto`). Se queda con el último `ERROR:` y, si
/// es JSON de la API, con su `error.message`.
pub fn error_detail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let Some(last) = text.lines().rev().find_map(|l| l.trim().strip_prefix("ERROR:")) else {
        return tail(stderr);
    };
    let last = last.trim();
    serde_json::from_str::<serde_json::Value>(last)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| last.to_owned())
}

#[async_trait]
impl LlmProvider for CodexCliProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities { streaming: false, native_json_schema: true, context_tokens: 200_000 }
    }

    async fn complete(&self, request: LlmRequest) -> PortResult<LlmResponse> {
        ensure_binary(&self.binary, "codex").await?;
        let dir = temp_dir()?;
        let out_file = dir.path().join("last-message.txt");
        let schema_file = match &request.json_schema {
            Some(schema) => {
                let p = dir.path().join("schema.json");
                tokio::fs::write(&p, wire_schema(schema).to_string()).await?;
                Some(p)
            }
            None => None,
        };
        // Codex no tiene flag de system prompt: va delante en el prompt.
        let mut prompt = String::new();
        if let Some(sys) = &request.system {
            prompt.push_str(sys);
            prompt.push_str("\n\n---\n\n");
        }
        prompt.push_str(&flatten_messages(&request.messages));

        let mut cmd = Command::new(&self.binary);
        cmd.args(self.build_args(schema_file.as_deref(), &out_file))
            .envs(&self.env)
            .current_dir(dir.path());
        let out = run_supervised(cmd, Some(prompt.into_bytes()), self.timeout).await?;
        if !out.status.success() {
            let code = out.status.code().map_or("señal".to_string(), |c| c.to_string());
            let detail = if out.stderr.is_empty() { tail(&out.stdout) } else { error_detail(&out.stderr) };
            return Err(PortError::Unavailable(format!("codex terminó con error (código {code}): {detail}")));
        }
        let text = match tokio::fs::read_to_string(&out_file).await {
            Ok(t) if !t.trim().is_empty() => t,
            _ => String::from_utf8_lossy(&out.stdout).into_owned(),
        };
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Err(PortError::InvalidResponse("codex no devolvió ningún mensaje".into()));
        }
        let json = request.json_schema.as_ref().and_then(|_| crate::structured::extract_json(&text));
        Ok(LlmResponse { text, json, model: self.model.clone(), usage: Usage::default() })
    }

    async fn stream(&self, request: LlmRequest) -> PortResult<EventStream<PortResult<String>>> {
        Ok(single_chunk(self.complete(request).await?.text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    fn provider(bin: PathBuf, model: Option<&str>, env: &[(&str, &str)]) -> CodexCliProvider {
        CodexCliProvider::new(
            "codex-cli".into(),
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
    fn args_are_read_only_ephemeral_with_stdin_dash() {
        let p = provider("/x/codex".into(), Some("gpt-5"), &[]);
        let args = p.build_args(Some(Path::new("/t/s.json")), Path::new("/t/o.txt"));
        assert_eq!(
            args,
            ["exec", "--skip-git-repo-check", "--ephemeral", "--sandbox", "read-only", "--output-schema", "/t/s.json", "-o", "/t/o.txt", "-m", "gpt-5", "-"]
        );
        let args = provider("/x/codex".into(), None, &[]).build_args(None, Path::new("/t/o.txt"));
        assert!(!args.iter().any(|a| a == "--output-schema" || a == "-m"));
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn reads_last_message_file_and_pipes_system_plus_prompt() {
        let dir = tempfile::tempdir().expect("dir");
        let bin = fake_script(
            dir.path(),
            "codex",
            r#"
# localiza el valor de -o
while [ $# -gt 0 ]; do
  if [ "$1" = "-o" ]; then OUTFILE="$2"; fi
  if [ "$1" = "--output-schema" ]; then cp "$2" "$OUT/schema-copy.json"; fi
  shift
done
cat > "$OUT/stdin.txt"
printf '{"a": 5}' > "$OUTFILE"
echo "ruido de progreso en stdout"
"#,
        );
        let out = dir.path().join("out");
        std::fs::create_dir(&out).expect("mkdir");
        let p = provider(bin, None, &[("OUT", out.to_str().expect("utf8"))]);
        let r = p.complete(req()).await.expect("complete");
        assert_eq!(r.text, "{\"a\": 5}");
        assert_eq!(r.json.expect("json")["a"], 5);
        let stdin = std::fs::read_to_string(out.join("stdin.txt")).expect("stdin");
        assert!(stdin.starts_with("SISTEMA") && stdin.ends_with("pregunta"));
        assert!(std::fs::read_to_string(out.join("schema-copy.json")).expect("schema").contains("integer"));
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn output_schema_file_never_carries_meta_schema() {
        let dir = tempfile::tempdir().expect("dir");
        let bin = fake_script(
            dir.path(),
            "codex",
            r#"
while [ $# -gt 0 ]; do
  if [ "$1" = "-o" ]; then OUTFILE="$2"; fi
  if [ "$1" = "--output-schema" ]; then cp "$2" "$OUT/schema-copy.json"; fi
  shift
done
cat > /dev/null
printf '{"a": 1}' > "$OUTFILE"
"#,
        );
        let out = dir.path().join("out");
        std::fs::create_dir(&out).expect("mkdir");
        let mut r = req();
        r.json_schema = Some(serde_json::json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"a":{"type":"integer"}}}));
        provider(bin, None, &[("OUT", out.to_str().expect("utf8"))]).complete(r).await.expect("complete");
        let copy = std::fs::read_to_string(out.join("schema-copy.json")).expect("schema");
        assert!(!copy.contains("$schema"), "{copy}");
    }

    #[test]
    fn error_detail_keeps_the_api_message_not_hook_noise() {
        let stderr = b"hook: SessionStart\nhook: SessionStart Completed\nERROR: {\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"invalid_request_error\",\"message\":\"The 'gpt-6-astra' model requires a newer version of Codex.\"}}\nERROR: {\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"invalid_request_error\",\"message\":\"The 'gpt-6-astra' model requires a newer version of Codex.\"}}\n";
        assert_eq!(error_detail(stderr), "The 'gpt-6-astra' model requires a newer version of Codex.");
        assert_eq!(error_detail(b"hook: x\nERROR: You've hit your usage limit.\n"), "You've hit your usage limit.");
        assert_eq!(error_detail(b"not logged in"), "not logged in");
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn falls_back_to_stdout_when_no_output_file() {
        let dir = tempfile::tempdir().expect("dir");
        let bin = fake_script(dir.path(), "codex", "cat > /dev/null\necho 'hola desde stdout'\n");
        let mut r = req();
        r.json_schema = None;
        let out = provider(bin, None, &[]).complete(r).await.expect("complete");
        assert_eq!(out.text, "hola desde stdout");
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn failure_and_missing_binary_are_clear() {
        let dir = tempfile::tempdir().expect("dir");
        let bin = fake_script(dir.path(), "codex", "echo 'not logged in' >&2\nexit 1\n");
        let e = provider(bin, None, &[]).complete(req()).await.expect_err("err");
        assert!(matches!(e, PortError::Unavailable(m) if m.contains("código 1") && m.contains("not logged in")));
        let e = provider("/no/existe/codex".into(), None, &[]).complete(req()).await.expect_err("err");
        assert!(matches!(e, PortError::Unavailable(m) if m.contains("codex no encontrado en /no/existe/codex")));
    }
}
