use crate::{
    library::{IMPORT_BATCH, Inventory},
    mcp_activity::{EXPIRED, SESSION_STATUS},
};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{
    Client, StatusCode, Url,
    header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue},
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    env,
    fs::File,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

const MAX_REQUEST: usize = 4 * 1024 * 1024;
const MAX_RESPONSE: usize = 32 * 1024 * 1024;
const SESSION: &str = "mcp-session-id";
const VERSION: &str = "mcp-protocol-version";

pub async fn run(address: &str) -> Result<()> {
    let endpoint = project_endpoint(address)?;
    let key = agent_key().await?;
    let media_dir = env::var_os("EDITROOM_MCP_MEDIA_DIR").map(PathBuf::from);
    ensure!(
        media_dir.as_ref().is_none_or(|path| path.is_absolute()),
        "Set EDITROOM_MCP_MEDIA_DIR to an absolute path on this device."
    );
    let mut builder = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(600));
    if endpoint.scheme() == "http" && endpoint.host_str() == Some("localhost") {
        let port = endpoint.port_or_known_default().unwrap_or(80);
        builder = builder.resolve("localhost", SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
    }
    let client = builder.build()?;
    let mut bridge = Bridge {
        endpoint,
        client,
        key,
        media_dir,
        hello: None,
        session: None,
        version: None,
    };
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = tokio::io::stdout();
    let mut line = Vec::new();
    loop {
        line.clear();
        if (&mut input)
            .take((MAX_REQUEST + 2) as u64)
            .read_until(b'\n', &mut line)
            .await?
            == 0
        {
            break;
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        let too_large = line.len() > MAX_REQUEST;
        let reply = if too_large {
            Some(rpc_error(
                &Value::Null,
                -32600,
                "The MCP request exceeds 4 MiB.",
            ))
        } else if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        } else {
            match serde_json::from_slice::<Value>(&line) {
                Ok(request) => bridge.forward(request).await,
                Err(_) => Some(rpc_error(&Value::Null, -32700, "Invalid JSON.")),
            }
        };
        if let Some(reply) = reply {
            let bytes = serde_json::to_vec(&reply)?;
            output.write_all(&bytes).await?;
            output.write_all(b"\n").await?;
            output.flush().await?;
        }
        if too_large {
            break;
        }
    }
    bridge.disconnect().await;
    Ok(())
}

fn project_endpoint(address: &str) -> Result<Url> {
    let url = Url::parse(address)
        .map_err(|_| anyhow::anyhow!("Use the MCP endpoint shown in Editor Settings."))?;
    let local = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    ensure!(
        (url.scheme() == "https" && url.host().is_some() || url.scheme() == "http" && local)
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Use an HTTPS studio endpoint or a loopback HTTP endpoint."
    );
    let is_mcp = url
        .path()
        .strip_prefix("/api/projects/")
        .and_then(|path| path.strip_suffix("/mcp"))
        .is_some_and(|id| id.parse::<Uuid>().is_ok());
    ensure!(
        is_mcp,
        "Use the project's MCP endpoint from Editor Settings."
    );
    Ok(url)
}

async fn agent_key() -> Result<String> {
    let key = if let Some(path) = env::var_os("EDITROOM_MCP_KEY_FILE") {
        let mut file = tokio::fs::File::open(PathBuf::from(path))
            .await
            .context("Could not open the EditCastle agent key file.")?
            .take(4097);
        let mut key = String::new();
        file.read_to_string(&mut key)
            .await
            .context("Could not read the EditCastle agent key file.")?;
        ensure!(
            key.len() <= 4096,
            "The EditCastle agent key file is too large."
        );
        key
    } else {
        env::var("EDITROOM_MCP_KEY")
            .context("Set EDITROOM_MCP_KEY or EDITROOM_MCP_KEY_FILE in the client environment.")?
    };
    let key = key.trim();
    ensure!(
        (32..=4096).contains(&key.len()) && key.bytes().all(|byte| byte.is_ascii_graphic()),
        "Use the agent key from EditCastle's Attached agents settings."
    );
    Ok(key.to_owned())
}

struct Bridge {
    endpoint: Url,
    client: Client,
    key: String,
    media_dir: Option<PathBuf>,
    hello: Option<Value>,
    session: Option<HeaderValue>,
    version: Option<HeaderValue>,
}

impl Bridge {
    async fn forward(&mut self, request: Value) -> Option<Value> {
        let id = request.get("id").cloned();
        let Some(method) = request["method"]
            .as_str()
            .filter(|_| request["jsonrpc"] == "2.0")
        else {
            return Some(rpc_error(
                id.as_ref().unwrap_or(&Value::Null),
                -32600,
                "Invalid MCP request.",
            ));
        };
        let initialize = method == "initialize";
        let list_tools = method == "tools/list";
        if !initialize
            && self.hello.is_some()
            && self.session.is_none()
            && self.reconnect().await.is_err()
        {
            return id.as_ref().map(reconnect_error);
        }
        if method == "tools/call"
            && request["params"]["name"] == "import_media"
            && let Some(directory) = self.media_dir.clone()
        {
            let id = id?;
            let result = self
                .import_local(directory, request["params"]["arguments"].clone())
                .await;
            return Some(match result {
                Ok(result) => tool_result(&id, result.to_string(), false),
                Err(error) => tool_result(&id, error.to_string(), true),
            });
        }
        let mut response = self.post(&request, initialize).await;
        if !initialize
            && self.session.is_some()
            && self.hello.is_some()
            && response.as_ref().is_ok_and(expired)
        {
            self.session = None;
            self.version = None;
            if self.reconnect().await.is_err() {
                return id.as_ref().map(reconnect_error);
            }
            response = self.post(&request, false).await;
        }
        let id = id?;
        let Ok(response) = response else {
            return Some(rpc_error(
                &id,
                -32000,
                "EditCastle did not confirm the request. Check the cut before sending the edit again.",
            ));
        };
        let status = response.status();
        if !status.is_success() {
            let message = match status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    "EditCastle refused the agent key. Check Attached agents in the studio."
                }
                StatusCode::NOT_FOUND if initialize => {
                    "That EditCastle project could not be found."
                }
                StatusCode::NOT_FOUND => {
                    "The EditCastle project or MCP session is unavailable. Reconnect the client."
                }
                _ => "EditCastle could not complete the MCP request.",
            };
            return Some(rpc_error(&id, -32000, message));
        }
        let session = response.headers().get(SESSION).cloned();
        let mut answer = match read_reply(response, &id).await {
            Ok(answer) => answer,
            _ => {
                return Some(rpc_error(
                    &id,
                    -32000,
                    "EditCastle returned an unreadable response. Check the cut before sending the edit again.",
                ));
            }
        };
        if initialize && answer.get("result").is_some() {
            let Some(session) = session else {
                return Some(rpc_error(
                    &id,
                    -32000,
                    "Studio did not create an MCP session.",
                ));
            };
            self.disconnect().await;
            self.set_session(session, &answer);
            self.hello = Some(request);
        }
        if list_tools
            && self.media_dir.is_some()
            && let Some(tools) = answer
                .pointer_mut("/result/tools")
                .and_then(Value::as_array_mut)
            && let Some(import) = tools.iter_mut().find(|tool| tool["name"] == "import_media")
        {
            import["description"] = json!(
                "Copy local media files or folders from the media directory shared with the EditCastle bridge. Folders keep their layout in the project. Use the returned source IDs to edit."
            );
            import["inputSchema"]["properties"]["paths"]["description"] = json!(
                "File and folder paths within the configured local media directory. Paths can be relative to that directory or absolute."
            );
        }
        Some(answer)
    }

    async fn import_local(&mut self, directory: PathBuf, arguments: Value) -> Result<Value> {
        ensure!(
            self.session.is_some(),
            "Initialize the attached agent before importing media."
        );
        let ping = json!({"jsonrpc":"2.0","id":Uuid::new_v4(),"method":"ping"});
        let reply = self
            .post(&ping, false)
            .await
            .context("Check the EditCastle connection.")?;
        if expired(&reply) {
            self.session = None;
            self.version = None;
            self.reconnect()
                .await
                .context("Reconnect the attached agent before importing.")?;
        } else {
            ensure!(
                reply.status().is_success(),
                "EditCastle refused the attached-agent session."
            );
        }

        let groups =
            tokio::task::spawn_blocking(move || plan_import(&directory, &arguments)).await??;
        let mut url = self.endpoint.clone();
        url.path_segments_mut().expect("project URL").push("upload");
        let planned: usize = groups.iter().map(|group| group.files.len()).sum();
        let mut folders = vec![];
        let mut imported = vec![];
        let transfer: Result<()> = async {
            for group in groups {
                let folder = if let Some(name) = group.name {
                    let request = self
                        .client
                        .post(url.clone())
                        .bearer_auth(&self.key)
                        .json(&json!({"name":name}));
                    let folder = upload_reply(self.with_session(request)).await?;
                    let id = folder["id"]
                        .as_str()
                        .context("EditCastle returned an invalid media folder.")?
                        .to_owned();
                    folders.push(folder);
                    Some(id)
                } else {
                    None
                };
                for entry in group.files {
                    let mut request = self
                        .client
                        .put(url.clone())
                        .bearer_auth(&self.key)
                        .query(&[("title", entry.name.as_str())]);
                    if let Some(folder) = &folder {
                        request = request.query(&[("folder", folder), ("path", &entry.relative)]);
                    }
                    let body = tokio::fs::File::from_std(entry.file).take(entry.size);
                    let request = request
                        .header(CONTENT_LENGTH, entry.size)
                        .header(CONTENT_TYPE, "application/octet-stream")
                        .body(reqwest::Body::wrap_stream(ReaderStream::new(body)));
                    let source = upload_reply(self.with_session(request)).await?;
                    ensure!(
                        source["id"].as_str().is_some(),
                        "EditCastle could not confirm the source. Check Media before retrying."
                    );
                    imported.push(source);
                }
            }
            Ok(())
        }
        .await;
        let issue = match transfer {
            Ok(()) => None,
            Err(error) if imported.is_empty() => return Err(error),
            Err(error) => Some(error.to_string()),
        };
        Ok(
            json!({"imported":imported,"folders":folders,"failed":planned-imported.len(),"issue":issue}),
        )
    }

    async fn post(&self, request: &Value, initialize: bool) -> reqwest::Result<reqwest::Response> {
        let outgoing = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(&self.key)
            .header(ACCEPT, "application/json, text/event-stream")
            .json(request);
        if initialize {
            outgoing.send().await
        } else {
            self.with_session(outgoing).send().await
        }
    }

    async fn reconnect(&mut self) -> Result<()> {
        let hello = self.hello.as_ref().context("Missing MCP handshake.")?;
        let response = self.post(hello, true).await?.error_for_status()?;
        let session = response
            .headers()
            .get(SESSION)
            .cloned()
            .context("Missing MCP session.")?;
        let answer = read_reply(response, &hello["id"]).await?;
        ensure!(
            answer.get("result").is_some(),
            "EditCastle refused the MCP handshake."
        );
        self.set_session(session, &answer);
        let initialized = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        if !self
            .post(&initialized, false)
            .await
            .is_ok_and(|response| response.status() == StatusCode::ACCEPTED)
        {
            self.disconnect().await;
            bail!("EditCastle did not accept the MCP session.");
        }
        Ok(())
    }

    fn set_session(&mut self, session: HeaderValue, answer: &Value) {
        self.session = Some(session);
        self.version = answer["result"]["protocolVersion"]
            .as_str()
            .and_then(|version| HeaderValue::from_str(version).ok());
    }

    fn with_session(&self, mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(session) = &self.session {
            request = request.header(SESSION, session.clone());
        }
        if let Some(version) = &self.version {
            request = request.header(VERSION, version.clone());
        }
        request
    }

    async fn disconnect(&mut self) {
        if self.session.is_some() {
            let request = self
                .client
                .delete(self.endpoint.clone())
                .bearer_auth(&self.key);
            let _ = self
                .with_session(request)
                .timeout(Duration::from_secs(3))
                .send()
                .await;
            self.session = None;
            self.version = None;
        }
    }
}

struct ImportGroup {
    name: Option<String>,
    files: Vec<LocalMedia>,
}

struct LocalMedia {
    name: String,
    relative: String,
    file: File,
    size: u64,
}

fn plan_import(directory: &Path, arguments: &Value) -> Result<Vec<ImportGroup>> {
    let paths = arguments["paths"]
        .as_array()
        .context("Pass paths within the shared local media directory.")?;
    ensure!(
        (1..=32).contains(&paths.len()),
        "Import up to 32 files or folders at once."
    );
    let inventory =
        Inventory::scan_existing(directory).context("Open the shared local media directory.")?;
    let mut paths = paths
        .iter()
        .map(|path| {
            let path = path
                .as_str()
                .filter(|path| !path.trim().is_empty())
                .context("Choose a local file or folder.")?;
            inventory
                .requested(path)
                .context("Choose a path inside the shared local media directory.")
        })
        .collect::<Result<Vec<_>>>()?;
    paths.sort_unstable();
    paths.dedup();
    let mut selected = HashSet::new();
    let mut groups = vec![];
    for path in paths {
        let folder = inventory.contains_folder(&path);
        let prefix = if folder && !path.is_empty() {
            format!("{path}/")
        } else {
            String::new()
        };
        let matches = if folder {
            inventory
                .file_paths()
                .filter_map(|candidate| {
                    candidate
                        .strip_prefix(&prefix)
                        .map(|relative| (candidate, relative))
                })
                .collect::<Vec<_>>()
        } else {
            vec![(path.as_str(), "")]
        };
        ensure!(
            !matches.is_empty(),
            "That local folder contains no supported media."
        );
        let mut files = vec![];
        for (candidate, relative) in matches {
            if !selected.insert(candidate.to_owned()) {
                continue;
            }
            ensure!(
                selected.len() <= IMPORT_BATCH,
                "Import up to {IMPORT_BATCH} media files at once."
            );
            let file = inventory
                .open_file(candidate)
                .context("Choose available video, audio or images within the shared directory.")?;
            let size = file.metadata()?.len();
            ensure!(
                (1..=crate::app::MAX_IMPORT as u64).contains(&size),
                "Use media files up to 256 MB."
            );
            let name = Path::new(candidate)
                .file_stem()
                .and_then(|name| name.to_str())
                .map(|name| name.chars().take(100).collect())
                .unwrap_or_else(|| "Imported media".into());
            files.push(LocalMedia {
                name,
                relative: relative.to_owned(),
                file,
                size,
            });
        }
        if !files.is_empty() {
            let name = folder.then(|| {
                Path::new(&path)
                    .file_name()
                    .or_else(|| directory.file_name())
                    .and_then(|part| part.to_str())
                    .unwrap_or("Media")
                    .chars()
                    .take(100)
                    .collect()
            });
            groups.push(ImportGroup { name, files });
        }
    }
    Ok(groups)
}

async fn upload_reply(request: reqwest::RequestBuilder) -> Result<Value> {
    let mut response = request
        .send()
        .await
        .context("EditCastle did not confirm the upload. Check Media before retrying.")?;
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("The upload response was interrupted. Check Media before retrying.")?
    {
        ensure!(
            chunk.len() <= 64 * 1024 - body.len(),
            "The upload response was too large."
        );
        body.extend_from_slice(&chunk);
    }
    let result: Value = serde_json::from_slice(&body)
        .context("EditCastle returned an unreadable upload response.")?;
    ensure!(
        status.is_success(),
        "{}",
        result["error"]
            .as_str()
            .unwrap_or("EditCastle refused the upload.")
    );
    Ok(result)
}

fn expired(response: &reqwest::Response) -> bool {
    response.status() == StatusCode::NOT_FOUND
        && response
            .headers()
            .get(SESSION_STATUS)
            .is_some_and(|header| header == EXPIRED)
}

fn tool_result(id: &Value, text: String, failed: bool) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}],"isError":failed}})
}

async fn read_reply(mut response: reqwest::Response, id: &Value) -> Result<Value> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > MAX_RESPONSE - body.len() {
            bail!("The MCP response exceeds 32 MiB.");
        }
        body.extend_from_slice(&chunk);
    }
    let reply: Value = serde_json::from_slice(&body)?;
    ensure!(
        reply["jsonrpc"] == "2.0"
            && &reply["id"] == id
            && (reply.get("result").is_some() || reply.get("error").is_some()),
        "Invalid MCP response."
    );
    Ok(reply)
}

fn reconnect_error(id: &Value) -> Value {
    rpc_error(
        id,
        -32000,
        "Could not reconnect to the EditCastle project. Check Attached agents in Studio.",
    )
}

fn rpc_error(id: &Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
