use crate::{
    editor::{Project, Store},
    media::{self, ExportObserver, ExportOptions},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Notify, Semaphore, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const WORK_PREFIX: &str = "export-job-";
const RECEIPT: &str = "export-receipt.json";

#[derive(Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Queued,
    Running,
    Cancelling,
    Done,
    Failed,
    Cancelled,
}

impl Status {
    fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Cancelling)
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Job {
    pub id: Uuid,
    pub project: Uuid,
    pub name: String,
    pub status: Status,
    pub progress: f64,
    pub eta_seconds: Option<u64>,
    pub error: Option<String>,
    #[serde(skip)]
    pub output: Option<PathBuf>,
}

#[derive(Deserialize, Serialize)]
struct Receipt {
    finished_ms: u64,
    job: Job,
}

impl Receipt {
    async fn open(dir: &Path, id: Uuid) -> Result<Option<Self>> {
        let bytes = match tokio::fs::read(dir.join(RECEIPT)).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut receipt: Self = serde_json::from_slice(&bytes)?;
        ensure!(
            receipt.job.id == id && receipt.job.status == Status::Done,
            "Invalid export receipt."
        );
        let output = match receipt
            .job
            .name
            .rsplit_once('.')
            .map(|(_, extension)| extension)
        {
            Some("mp4") => dir.join("editroom.mp4"),
            Some("wav") => dir.join("editroom.wav"),
            _ => anyhow::bail!("Invalid export format."),
        };
        let metadata = tokio::fs::symlink_metadata(&output).await?;
        ensure!(
            metadata.is_file() && metadata.len() > 0,
            "Saved export is missing."
        );
        receipt.job.output = Some(output);
        Ok(Some(receipt))
    }

    fn write(job: &Job, file: &Path) -> Result<()> {
        let receipt = Self {
            finished_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            job: job.clone(),
        };
        let dir = file.parent().context("Export directory is missing.")?;
        crate::editor::private_write(&dir.join(RECEIPT), &serde_json::to_vec(&receipt)?)
    }
}

struct Pending {
    project: Project,
    options: ExportOptions,
}

struct Entry {
    job: Job,
    pending: Option<Pending>,
    cancel: CancellationToken,
    signal: watch::Sender<Job>,
}

#[derive(Default)]
pub struct Queue {
    entries: Mutex<Vec<Entry>>,
    ready: Notify,
}

impl Queue {
    pub async fn open(store: &Store) -> Result<Self> {
        let mut receipts = vec![];
        let mut files = tokio::fs::read_dir(&store.root).await?;
        while let Some(entry) = files.next_entry().await? {
            let name = entry.file_name();
            let Some(id) = name
                .to_str()
                .and_then(|name| name.strip_prefix(WORK_PREFIX))
                .and_then(|id| Uuid::parse_str(id).ok())
            else {
                continue;
            };
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            match Receipt::open(&entry.path(), id).await {
                Ok(Some(receipt)) => receipts.push(receipt),
                Ok(None) => {
                    if let Err(error) = tokio::fs::remove_dir_all(entry.path()).await {
                        eprintln!("Could not clean an unfinished export: {error}");
                    }
                }
                Err(error) => eprintln!("Could not read saved export {id}: {error}"),
            }
        }
        receipts.sort_by_key(|receipt| (receipt.finished_ms, receipt.job.id));
        let entries = receipts
            .into_iter()
            .map(|receipt| {
                let job = receipt.job;
                let (signal, _) = watch::channel(job.clone());
                Entry {
                    job,
                    pending: None,
                    cancel: CancellationToken::new(),
                    signal,
                }
            })
            .collect();
        Ok(Self {
            entries: Mutex::new(entries),
            ready: Notify::new(),
        })
    }

    pub fn list(&self, project: Option<Uuid>) -> Vec<Job> {
        self.entries
            .lock()
            .expect("exports")
            .iter()
            .filter(|entry| project.is_none_or(|id| id == entry.job.project))
            .map(|entry| entry.job.clone())
            .collect()
    }

    pub fn get(&self, id: Uuid) -> Option<Job> {
        self.entries
            .lock()
            .expect("exports")
            .iter()
            .find(|entry| entry.job.id == id)
            .map(|entry| entry.job.clone())
    }

    pub fn enqueue(
        &self,
        project: Project,
        options: ExportOptions,
        name: String,
    ) -> Result<(Job, watch::Receiver<Job>)> {
        let job = Job {
            id: Uuid::new_v4(),
            project: project.id,
            name,
            status: Status::Queued,
            progress: 0.0,
            eta_seconds: None,
            error: None,
            output: None,
        };
        let (signal, done) = watch::channel(job.clone());
        let mut entries = self.entries.lock().expect("exports");
        ensure!(
            entries
                .iter()
                .filter(|entry| entry.job.status.active())
                .count()
                < 4,
            "The export queue is full."
        );
        if entries.len() >= 30
            && let Some(index) = entries.iter().position(|entry| !entry.job.status.active())
        {
            discard_output(&entries[index].job)?;
            entries.remove(index);
        }
        entries.push(Entry {
            job: job.clone(),
            pending: Some(Pending { project, options }),
            cancel: CancellationToken::new(),
            signal,
        });
        self.ready.notify_one();
        Ok((job, done))
    }

    pub fn cancel(&self, id: Uuid, project: Option<Uuid>) -> Result<Job> {
        let mut entries = self.entries.lock().expect("exports");
        let entry = entries
            .iter_mut()
            .find(|entry| entry.job.id == id && project.is_none_or(|id| id == entry.job.project))
            .context("No such export.")?;
        match entry.job.status {
            Status::Queued => {
                entry.pending.take();
                entry.job.status = Status::Cancelled;
            }
            Status::Running => {
                entry.job.status = Status::Cancelling;
                entry.cancel.cancel();
            }
            _ => {}
        }
        entry.job.eta_seconds = None;
        entry.signal.send_replace(entry.job.clone());
        Ok(entry.job.clone())
    }

    pub fn clear_finished(&self, project: Option<Uuid>) -> Result<()> {
        let mut entries = self.entries.lock().expect("exports");
        let mut result = Ok(());
        entries.retain(|entry| {
            if project.is_some_and(|id| id != entry.job.project) || entry.job.status.active() {
                return true;
            }
            match discard_output(&entry.job) {
                Ok(()) => false,
                Err(error) => {
                    result = Err(error);
                    true
                }
            }
        });
        result
    }

    pub async fn serve(&self, store: &Store, slot: &Semaphore) {
        loop {
            self.ready.notified().await;
            loop {
                let _permit = slot.acquire().await.expect("export slot");
                let Some((id, pending, stop)) = self.next() else {
                    break;
                };
                let started = Instant::now();
                let progress =
                    |fraction: f64| self.report_progress(id, fraction, started.elapsed());
                let monitor = ExportObserver {
                    stop: &stop,
                    progress: &progress,
                };
                let path = store.root.join(format!("{WORK_PREFIX}{id}"));
                let directory = tokio::fs::create_dir(&path).await;
                let created = directory.is_ok();
                let result = match directory {
                    Ok(()) => {
                        media::observed_export(
                            store,
                            &pending.project,
                            &path,
                            &pending.options,
                            &monitor,
                        )
                        .await
                    }
                    Err(error) => Err(anyhow::anyhow!("Could not prepare the export: {error}")),
                };

                // Decide whether the file is downloadable while holding the cancellation lock.
                // For cancellation, keep the active slot until the files are gone.
                if self.publish_result(id, result) {
                    if created {
                        let _ = tokio::fs::remove_dir_all(path).await;
                    }
                    self.set_cancelled(id);
                }
            }
        }
    }

    fn next(&self) -> Option<(Uuid, Pending, CancellationToken)> {
        let mut entries = self.entries.lock().expect("exports");
        let entry = entries
            .iter_mut()
            .find(|entry| entry.job.status == Status::Queued)?;
        let pending = entry.pending.take().expect("queued export snapshot");
        entry.job.status = Status::Running;
        entry.signal.send_replace(entry.job.clone());
        Some((entry.job.id, pending, entry.cancel.clone()))
    }

    fn report_progress(&self, id: Uuid, fraction: f64, elapsed: Duration) {
        let mut entries = self.entries.lock().expect("exports");
        let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.job.id == id && entry.job.status == Status::Running)
        else {
            return;
        };
        let fraction = fraction.clamp(entry.job.progress, 0.99);
        entry.job.progress = fraction;
        entry.job.eta_seconds = (fraction > 0.03)
            .then(|| (elapsed.as_secs_f64() / fraction * (1.0 - fraction)).ceil() as u64);
    }

    fn publish_result(&self, id: Uuid, result: Result<PathBuf>) -> bool {
        let mut entries = self.entries.lock().expect("exports");
        let entry = entries
            .iter_mut()
            .find(|entry| entry.job.id == id)
            .expect("running export");
        if entry.job.status == Status::Cancelling {
            return true;
        }
        entry.job.eta_seconds = None;
        let needs_cleanup = match result {
            Ok(file) => {
                entry.job.progress = 1.0;
                entry.job.status = Status::Done;
                if let Err(error) = Receipt::write(&entry.job, &file) {
                    eprintln!("Could not save export receipt {id}: {error}");
                    entry.job.error =
                        Some("Download this file before restarting the studio.".into());
                }
                entry.job.output = Some(file);
                false
            }
            Err(error) => {
                entry.job.status = Status::Failed;
                entry.job.error = Some(error.to_string());
                true
            }
        };
        entry.signal.send_replace(entry.job.clone());
        needs_cleanup
    }

    fn set_cancelled(&self, id: Uuid) {
        let mut entries = self.entries.lock().expect("exports");
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.job.id == id && entry.job.status == Status::Cancelling)
        {
            entry.job.status = Status::Cancelled;
            entry.signal.send_replace(entry.job.clone());
        }
    }
}

fn discard_output(job: &Job) -> Result<()> {
    if let Some(dir) = job.output.as_deref().and_then(std::path::Path::parent) {
        let dir = dir.to_owned();
        match std::fs::remove_file(dir.join(RECEIPT)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Could not clear the saved export."),
        }
        tokio::spawn(async move {
            let _ = tokio::fs::remove_dir_all(dir).await;
        });
    }
    Ok(())
}

pub async fn wait(mut updates: watch::Receiver<Job>) -> Job {
    let _ = tokio::time::timeout(
        Duration::from_secs(110),
        updates.wait_for(|job| !job.status.active()),
    )
    .await;
    updates.borrow().clone()
}
