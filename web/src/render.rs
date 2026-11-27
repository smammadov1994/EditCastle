use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, StatusCode, Url, multipart};
use serde_json::{Value, json};
use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};
use tokio::{
    fs,
    io::AsyncWriteExt,
    net::TcpStream,
    process::Command,
    sync::Mutex,
    time::{Duration, Instant, sleep, timeout},
};
use uuid::Uuid;

#[derive(Clone, Copy, Default, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Model {
    #[default]
    PiH3,
    Hyperflow,
}

impl Model {
    pub fn label(self) -> &'static str {
        match self {
            Self::PiH3 => "Pi-H3",
            Self::Hyperflow => "HyperFlow",
        }
    }
    fn capability(self) -> &'static str {
        match self {
            Self::PiH3 => "MiniMaxH3SparkAttentionSM120",
            Self::Hyperflow => "HyperFlowGGUFLoader",
        }
    }
}

#[derive(Clone)]
pub struct Worker {
    base: Url,
    client: Client,
    model: Model,
    tunnel: Option<Arc<Tunnel>>,
}

struct Tunnel {
    command: PathBuf,
    address: SocketAddr,
    connect: Mutex<()>,
}

impl Tunnel {
    async fn online(&self) -> bool {
        matches!(
            timeout(Duration::from_secs(2), TcpStream::connect(self.address)).await,
            Ok(Ok(_))
        )
    }

    async fn restore(&self) {
        if self.online().await {
            return;
        }
        let _guard = self.connect.lock().await;
        if self.online().await {
            return;
        }
        let _ = timeout(
            Duration::from_secs(20),
            Command::new(&self.command)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await;
    }
}

pub enum Status {
    Ready { filename: String, subfolder: String },
    Failed,
    Rendering,
    Queued,
    Unknown,
}

pub enum Submission {
    Delivered,
    Unknown,
}

fn output_status(id: Uuid, outputs: &Value) -> Status {
    let prefix = format!("editroom-web-{id}_");
    let file = outputs
        .as_object()
        .into_iter()
        .flat_map(|nodes| nodes.values())
        .filter_map(Value::as_object)
        .flat_map(|node| node.values())
        .filter_map(Value::as_array)
        .flatten()
        .find_map(|file| {
            let name = file["filename"].as_str()?;
            (name.starts_with(&prefix)
                && name.ends_with(".mp4")
                && !name.contains(['/', '\\'])
                && file["type"].as_str().unwrap_or("output") == "output")
                .then(|| Status::Ready {
                    filename: name.into(),
                    subfolder: file["subfolder"].as_str().unwrap_or("").into(),
                })
        });
    file.unwrap_or(Status::Failed)
}

impl Worker {
    pub fn new(address: &str) -> Result<Self> {
        let base = Url::parse(address)?;
        let local_ip = base
            .host_str()
            .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .filter(IpAddr::is_loopback);
        ensure!(
            base.scheme() == "http"
                && local_ip.is_some()
                && base.username().is_empty()
                && base.password().is_none()
                && base.path() == "/"
                && base.query().is_none()
                && base.fragment().is_none(),
            "ComfyUI must be a loopback HTTP address. Forward a remote worker over SSH."
        );
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(25))
            .build()?;
        Ok(Self {
            base: base.clone(),
            client,
            model: Model::default(),
            tunnel: std::env::var_os("EDITROOM_COMFY_TUNNEL")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|command| {
                    Arc::new(Tunnel {
                        command,
                        address: SocketAddr::new(
                            local_ip.unwrap(),
                            base.port_or_known_default().unwrap_or(80),
                        ),
                        connect: Mutex::new(()),
                    })
                }),
        })
    }

    pub fn maintain_tunnel(&self) {
        if let Some(tunnel) = self.tunnel.clone() {
            tokio::spawn(async move {
                loop {
                    tunnel.restore().await;
                    sleep(Duration::from_secs(15)).await;
                }
            });
        }
    }

    pub fn model(&self, model: Model) -> Self {
        Self {
            model,
            ..self.clone()
        }
    }
    pub fn label(&self) -> &'static str {
        self.model.label()
    }

    fn url(&self, path: &str) -> Url {
        self.base.join(path).expect("static worker path")
    }

    async fn json(&self, path: &str) -> Result<Value> {
        self.client
            .get(self.url(path))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .map_err(Into::into)
    }

    pub async fn check(&self) -> Result<()> {
        if let Some(tunnel) = &self.tunnel {
            tunnel.restore().await;
        }
        self.json("system_stats").await.map_err(|_| {
            anyhow::anyhow!("ComfyUI is offline. Check the private GPU connection.")
        })?;
        let node = self.model.capability();
        let nodes = self
            .json(&format!("object_info/{node}"))
            .await
            .map_err(|_| anyhow::anyhow!("The {} node could not be checked.", self.label()))?;
        ensure!(
            nodes.get(node).is_some(),
            "The GPU worker does not have the {} node.",
            self.label()
        );
        Ok(())
    }

    pub async fn upload(&self, id: Uuid, path: &Path) -> Result<String> {
        let name = format!("editroom-web-{id}.png");
        let part = multipart::Part::bytes(fs::read(path).await?)
            .file_name(name.clone())
            .mime_str("image/png")?;
        let result: Value = self
            .client
            .post(self.url("upload/image"))
            .multipart(multipart::Form::new().part("image", part))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ensure!(
            result["name"].as_str() == Some(name.as_str())
                && result["subfolder"].as_str().unwrap_or("").is_empty(),
            "ComfyUI did not accept the starting frame."
        );
        Ok(name)
    }

    pub async fn submit(
        &self,
        id: Uuid,
        direction: &str,
        frames: u32,
        anchor: Option<String>,
    ) -> Result<Submission> {
        let body = json!({
            "prompt_id": id, "prompt": workflow(direction, frames, id, anchor, self.model)
        });
        let Ok(response) = self
            .client
            .post(self.url("prompt"))
            .json(&body)
            .send()
            .await
        else {
            return Ok(Submission::Unknown);
        };
        if response.status().is_client_error() {
            bail!("ComfyUI rejected the {} workflow.", self.label());
        }
        if !response.status().is_success() {
            return Ok(Submission::Unknown);
        }
        let reply: Option<Value> = response.json().await.ok();
        if reply
            .as_ref()
            .and_then(|value| value["prompt_id"].as_str())
            .and_then(|id| id.parse().ok())
            == Some(id)
        {
            Ok(Submission::Delivered)
        } else {
            Ok(Submission::Unknown)
        }
    }

    pub async fn status(&self, id: Uuid) -> Result<Status> {
        let response = self
            .client
            .get(self.url(&format!("api/jobs/{id}")))
            .send()
            .await?;
        match response.status() {
            StatusCode::NOT_FOUND => {
                let error = response.json::<Value>().await.unwrap_or_default();
                if error["error"] == "Job not found" {
                    return Ok(Status::Unknown);
                }
            }
            StatusCode::METHOD_NOT_ALLOWED => {}
            _ => {
                let entry: Value = response.error_for_status()?.json().await?;
                ensure!(
                    entry["id"].as_str().and_then(|value| value.parse().ok()) == Some(id),
                    "ComfyUI returned a different job."
                );
                return match entry["status"].as_str() {
                    Some("pending") => Ok(Status::Queued),
                    Some("in_progress") => Ok(Status::Rendering),
                    Some("completed") => Ok(output_status(id, &entry["outputs"])),
                    Some("failed" | "cancelled") => Ok(Status::Failed),
                    _ => bail!("ComfyUI returned an unknown job status."),
                };
            }
        }

        // Older workers only offer the per-job history and shared queue.
        let history = self.json(&format!("history/{id}")).await?;
        if let Some(entry) = history.get(id.to_string()) {
            if entry["status"]["status_str"] == "error" {
                return Ok(Status::Failed);
            }
            return Ok(output_status(id, &entry["outputs"]));
        }
        let queue = self.json("queue").await?;
        let id = id.to_string();
        let has = |key: &str| {
            queue[key]
                .as_array()
                .into_iter()
                .flatten()
                .any(|entry| entry[1].as_str() == Some(id.as_str()))
        };
        Ok(if has("queue_running") {
            Status::Rendering
        } else if has("queue_pending") {
            Status::Queued
        } else {
            Status::Unknown
        })
    }

    pub async fn wait(
        &self,
        id: Uuid,
        mut stage: impl FnMut(&str),
        mut done: impl FnMut(bool),
    ) -> Result<(String, String)> {
        let deadline = Instant::now() + Duration::from_secs(2700);
        let mut missing_since = None;
        let mut last = "";
        while Instant::now() < deadline {
            let status = self.status(id).await;
            let message = match status {
                Ok(Status::Ready {
                    filename,
                    subfolder,
                }) => {
                    done(true);
                    return Ok((filename, subfolder));
                }
                Ok(Status::Failed) => {
                    done(false);
                    bail!("The GPU did not produce a video for this shot.");
                }
                Ok(Status::Queued) => "Queued on the GPU",
                Ok(Status::Rendering) => "Rendering on the GPU",
                Ok(Status::Unknown) => {
                    let since = missing_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= Duration::from_secs(90) {
                        done(false);
                        bail!("The GPU did not record this render. Its credits were returned.");
                    }
                    "Checking for the GPU job"
                }
                Err(_) => "Reconnecting to ComfyUI",
            };
            if !matches!(status, Ok(Status::Unknown)) {
                missing_since = None;
            }
            if message != last {
                stage(message);
                last = message;
            }
            sleep(Duration::from_secs(2)).await;
        }
        bail!(
            "The GPU is still unconfirmed. Its credits will settle when the worker is reachable."
        );
    }

    pub async fn cancel(&self, id: Uuid) -> Result<bool> {
        let response = self
            .client
            .post(self.url(&format!("api/jobs/{id}/cancel")))
            .send()
            .await?;
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ) {
            // Legacy ComfyUI can always remove a queued prompt by ID. Running prompts
            // on that API are left to finish because interrupt may affect other jobs.
            if matches!(self.status(id).await?, Status::Queued) {
                self.client
                    .post(self.url("queue"))
                    .json(&json!({"delete":[id]}))
                    .send()
                    .await?
                    .error_for_status()?;
                return Ok(true);
            }
            Ok(false)
        } else {
            let result: Value = response.error_for_status()?.json().await?;
            result["cancelled"]
                .as_bool()
                .context("ComfyUI did not confirm the cancellation request.")
        }
    }

    pub async fn download(
        &self,
        name: String,
        subfolder: String,
        dest: &Path,
        mut stage: impl FnMut(&str),
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(180);
        let partial = dest.with_extension("part");
        stage("Downloading from ComfyUI");
        let mut recovering = false;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let attempt = timeout(remaining, self.download_once(&name, &subfolder, &partial)).await;
            match attempt {
                Ok(Ok(())) => {
                    if fs::rename(&partial, dest).await.is_ok() {
                        return Ok(());
                    }
                    break;
                }
                Ok(Err(error)) if retry_download(&error) && Instant::now() < deadline => {
                    if !recovering {
                        stage("Recovering the download from ComfyUI");
                        recovering = true;
                    }
                    if let Some(tunnel) = &self.tunnel {
                        let _ = timeout(
                            deadline.saturating_duration_since(Instant::now()),
                            tunnel.restore(),
                        )
                        .await;
                    }
                    sleep(
                        Duration::from_secs(2)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    )
                    .await;
                }
                _ => break,
            }
        }
        let _ = fs::remove_file(partial).await;
        bail!("The shot finished on the GPU. EditCastle could not retrieve its video.")
    }

    async fn download_once(&self, name: &str, subfolder: &str, partial: &Path) -> Result<()> {
        const MAX_VIDEO: u64 = 200 * 1024 * 1024;
        let mut response = self
            .client
            .get(self.url("view"))
            .query(&[
                ("filename", name),
                ("subfolder", subfolder),
                ("type", "output"),
            ])
            .timeout(Duration::from_secs(150))
            .send()
            .await?
            .error_for_status()?;
        ensure!(
            response.status() == StatusCode::OK,
            "The rendered video could not be downloaded."
        );
        let expected = response.content_length();
        ensure!(
            expected.is_none_or(|size| size <= MAX_VIDEO),
            "The rendered video is too large."
        );
        let mut file = fs::File::create(partial).await?;
        let mut length = 0u64;
        while let Some(chunk) = response.chunk().await? {
            length += chunk.len() as u64;
            ensure!(length <= MAX_VIDEO, "The rendered video is too large.");
            file.write_all(&chunk).await?;
        }
        ensure!(length > 0, "The rendered video was empty.");
        ensure!(
            expected.is_none_or(|size| size == length),
            "The rendered video's response length did not match."
        );
        file.flush().await?;
        Ok(())
    }
}

fn retry_download(error: &anyhow::Error) -> bool {
    error.downcast_ref::<reqwest::Error>().is_some_and(|error| {
        error.is_connect()
            || error.is_timeout()
            || error.is_body()
            || error.is_decode()
            || error.is_request()
            || error.status().is_some_and(|status| {
                status.is_server_error()
                    || matches!(
                        status,
                        StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
                    )
            })
    })
}

fn workflow(prompt: &str, frames: u32, id: Uuid, anchor: Option<String>, model: Model) -> Value {
    let node = |class: &str, inputs: Value| json!({"class_type":class,"inputs":inputs});
    let seed =
        u32::from_be_bytes(Uuid::new_v4().as_bytes()[0..4].try_into().unwrap()) & 0x7FFF_FFFF;
    let mut graph = json!({
        "1": node("UNETLoader", json!({"unet_name":"minimax_h3_fl2va_pruned_int8_convrot.safetensors","weight_dtype":"default"})),
        "2": node("CLIPLoader", json!({"clip_name":"qwen3vl_32b_minimax_h3_nvfp4_awq.safetensors","type":"minimax","device":"default"})),
        "3": node("VAELoader", json!({"vae_name":"minimax_h3_video_vae_int8_convrot.safetensors"})),
        "4": node("VAELoader", json!({"vae_name":"minimax_h3_audio_vae_fp32.safetensors"})),
        "5": node("MiniMaxH3ImageToVideo", json!({"prompt":prompt,"width":1344,"height":768,"length":frames,"clip":["2",0],"vae":["3",0]})),
        "6": node("pi-Flow Scheduler", json!({"model":["17",0],"steps":2,"min_raw_t":0.0,"max_raw_t":0.93,"final_step_size_scale":0.6})),
        "7": node("KSamplerSelect", json!({"sampler_name":"euler"})),
        "8": node("RandomNoise", json!({"noise_seed":seed})),
        "9": node("BasicGuider", json!({"model":["17",0],"conditioning":["5",0]})),
        "10": node("SamplerCustomAdvanced", json!({"noise":["8",0],"guider":["9",0],"sampler":["7",0],"sigmas":["6",0],"latent_image":["5",1]})),
        "11": node("DWD Decode", json!({"samples":["10",0]})),
        "12": node("VAEDecodeAudio", json!({"samples":["10",0],"vae":["4",0]})),
        "13": node("CreateVideo", json!({"fps":24,"bit_depth":"auto","color_space":"sRGB","codec":"auto","images":["11",0],"audio":["12",0]})),
        "14": node("SaveVideo", json!({"filename_prefix":format!("editroom-web-{id}"),"format":"mp4","format.codec":"auto","codec":"auto","video":["13",0]})),
        "15": node("Load LakonLab Adapter", json!({"model":["1",0],"adapter_strength":1.0,"adapter_name":"pi_h3_2step_dwd_adapter_int8_convrot.safetensors"})),
        "16": node("MiniMaxH3SigmaShift", json!({"model":["15",0],"shift_video":12,"shift_audio":3})),
        "17": node("MiniMaxH3SparkAttentionSM120", json!({
            "model":["16",0],"mode":"speed","steps":2,"warmup_mode":"warmup_ratio","warmup_ratio":0.0,"warmup_steps":4,
            "topk_mode":"topk_ratio","topk_ratio":0.2,"topk_blocks":228,"dense_layers":0,"min_tokens":12288,"strict":true,
            "dense_backend":"inherit","tail_granularity":"query","reblock_layout":"q_reuse_k","sparse_scope":"target-video",
            "producer_chunk":16384,"chunk_path":"none"
        }))
    });
    if matches!(model, Model::Hyperflow) {
        graph["1"] = node(
            "HyperFlowGGUFLoader",
            json!({"unet_name":"fl2va_gguf/minimax_h3_fl2va_hyperflow_flash_14b-Q4_K_M.gguf","weights_in":"auto"}),
        );
        graph["3"] = node(
            "VAELoader",
            json!({"vae_name":"minimax_h3_video_vae_fp16.safetensors"}),
        );
        graph["6"] = node("HyperFlowSigmas", json!({"model":["1",0],"steps":6}));
        graph["9"] = node(
            "BasicGuider",
            json!({"model":["1",0],"conditioning":["5",0]}),
        );
        graph["11"] = node("VAEDecode", json!({"samples":["10",0],"vae":["3",0]}));
        for id in ["15", "16", "17"] {
            graph.as_object_mut().unwrap().remove(id);
        }
    }
    if let Some(image) = anchor {
        graph["20"] = node("LoadImage", json!({"image":image}));
        graph["5"]["inputs"]["first_frame"] = json!(["20", 0]);
    }
    graph
}
