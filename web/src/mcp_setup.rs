use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    env,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Client {
    Codex,
    ClaudeCode,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Connect,
    Remove,
}

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Registration {
    Registered,
    Missing,
    Different,
    Unavailable,
}

#[derive(Serialize)]
pub struct Commands {
    install: String,
    remove: String,
    needs_paths: bool,
}

pub struct Setup {
    client: Client,
    name: String,
    executable: String,
    endpoint: String,
    key_file: String,
    install: Vec<String>,
    remove: Vec<String>,
    needs_paths: bool,
}

impl Setup {
    pub fn commands(&self) -> Commands {
        let line = |args: &[String]| {
            args.iter()
                .map(|arg| shell_argument(arg))
                .collect::<Vec<_>>()
                .join(" ")
        };
        Commands {
            install: line(&self.install),
            remove: line(&self.remove),
            needs_paths: self.needs_paths,
        }
    }

    pub async fn apply(self, action: Action) -> Result<()> {
        let args = match action {
            Action::Connect => {
                ensure!(
                    !self.needs_paths,
                    "Studio could not find the bridge executable or agent key file."
                );
                match self.registration().await {
                    Registration::Registered => return Ok(()),
                    Registration::Different => bail!(
                        "This project name is already configured with different settings. Remove its entry before connecting again."
                    ),
                    Registration::Missing | Registration::Unavailable => {}
                }
                self.install
            }
            Action::Remove => self.remove,
        };
        let (cli, arguments) = args.split_first().context("Missing client command.")?;
        let mut child = tokio::process::Command::new(cli);
        child
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let status = tokio::time::timeout(Duration::from_secs(20), child.status())
            .await
            .context("The client did not respond.")?
            .with_context(|| format!("Studio could not start {cli}."))?;
        ensure!(
            status.success(),
            "The client did not accept the change. Check its settings in Terminal."
        );
        Ok(())
    }

    pub async fn registration(&self) -> Registration {
        let entry = match self.client {
            Client::Codex => codex_registration(&self.name).await,
            Client::ClaudeCode => claude_registration(&self.name).await,
        };
        match entry {
            Ok(None) => Registration::Missing,
            Ok(Some(config)) if self.matches(&config) => Registration::Registered,
            Ok(Some(_)) => Registration::Different,
            Err(_) => Registration::Unavailable,
        }
    }

    fn matches(&self, entry: &Value) -> bool {
        let config = match self.client {
            Client::Codex => {
                if entry["enabled"] != true || entry["transport"]["type"] != "stdio" {
                    return false;
                }
                &entry["transport"]
            }
            Client::ClaudeCode => entry,
        };
        config["command"].as_str() == Some(&self.executable)
            && config["args"] == serde_json::json!(["mcp-stdio", self.endpoint])
            && config["env"]["EDITROOM_MCP_KEY_FILE"].as_str() == Some(&self.key_file)
    }
}

async fn codex_registration(name: &str) -> Result<Option<Value>> {
    let mut command = tokio::process::Command::new("codex");
    command
        .args(["mcp", "get", "--json", name])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(5), command.output()).await??;
    if output.status.success() {
        Ok(Some(serde_json::from_slice(&output.stdout)?))
    } else if String::from_utf8_lossy(&output.stderr)
        .lines()
        .any(|line| line.trim() == format!("Error: No MCP server named '{name}' found."))
    {
        Ok(None)
    } else {
        bail!("Codex registration status is unavailable.")
    }
}

async fn claude_registration(name: &str) -> Result<Option<Value>> {
    let directory = env::var_os("CLAUDE_CONFIG_DIR")
        .or_else(|| env::var_os("HOME"))
        .context("Claude Code settings directory is unavailable.")?;
    let file = match tokio::fs::File::open(PathBuf::from(directory).join(".claude.json")).await {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    const LIMIT: u64 = 32 * 1024 * 1024;
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes).await?;
    ensure!(
        bytes.len() as u64 <= LIMIT,
        "Claude Code settings are too large to inspect."
    );
    let settings: Value = serde_json::from_slice(&bytes)?;
    Ok(settings["mcpServers"].get(name).cloned())
}

pub fn server_name(project: Uuid) -> String {
    format!("editroom-studio-{}", project.simple())
}

pub fn setup(
    client: Client,
    project: Uuid,
    endpoint: &str,
    binary: Option<&Path>,
    key_file: Option<&Path>,
) -> Setup {
    let name = server_name(project);
    let binary = binary.and_then(Path::to_str);
    let key_file = key_file.and_then(Path::to_str);
    let needs_paths = binary.is_none() || key_file.is_none();
    let executable = binary.unwrap_or("/absolute/path/to/editcastle-web");
    let key_file = key_file.unwrap_or("/absolute/path/to/editroom-agent-key");
    let key = format!("EDITROOM_MCP_KEY_FILE={key_file}");
    let (mut install, remove) = match client {
        Client::Codex => (
            vec!["codex", "mcp", "add", &name, "--env", &key],
            vec!["codex", "mcp", "remove", &name],
        ),
        Client::ClaudeCode => (
            vec![
                "claude",
                "mcp",
                "add",
                "--scope",
                "user",
                "--env",
                &key,
                "--transport",
                "stdio",
                &name,
            ],
            vec!["claude", "mcp", "remove", "--scope", "user", &name],
        ),
    };
    install.extend(["--", executable, "mcp-stdio", endpoint]);
    let install = install.into_iter().map(str::to_owned).collect();
    let remove = remove.into_iter().map(str::to_owned).collect();
    Setup {
        client,
        name,
        executable: executable.into(),
        endpoint: endpoint.into(),
        key_file: key_file.into(),
        install,
        remove,
        needs_paths,
    }
}

fn shell_argument(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
