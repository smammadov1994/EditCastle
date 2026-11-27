use crate::{clock, editor::Project};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Play,
    Pause,
    Seek,
    SetLoop,
}

#[derive(Deserialize)]
pub struct Command {
    pub action: Action,
    pub at: Option<u32>,
    pub looping: Option<bool>,
    pub origin: Option<Uuid>,
}

impl Command {
    pub fn from_tool(args: &serde_json::Value, project: &Project) -> Result<Self> {
        let (action, at) = match args["action"].as_str() {
            Some("play") => (Action::Play, None),
            Some("pause") => (Action::Pause, None),
            Some("seek") => {
                let seconds = args["time"].as_f64().context("Give a time in seconds.")?;
                (
                    Action::Seek,
                    Some(clock::snap_seconds(seconds, project.frame_step())),
                )
            }
            _ => anyhow::bail!("Choose play, pause or seek."),
        };
        Ok(Self {
            action,
            at,
            looping: None,
            origin: None,
        })
    }
}

#[derive(Clone, Copy)]
pub struct Transport {
    pub version: u64,
    at: u32,
    started: Option<Instant>,
    looping: bool,
    origin: Option<Uuid>,
}

#[derive(Serialize)]
pub struct View {
    version: u64,
    cut_revision: u64,
    sampled_at_ms: u64,
    pub at: u32,
    pub playing: bool,
    looping: bool,
    origin: Option<Uuid>,
}

impl Transport {
    fn new(project: &Project) -> Self {
        Self {
            version: 0,
            at: project.sequence.playhead,
            started: None,
            looping: false,
            origin: None,
        }
    }

    fn span(project: &Project, looping: bool) -> (u32, u32) {
        let end = project.duration();
        if looping
            && let Some((first, last)) = project.play_range()
            && first < last.min(end)
        {
            return (first, last.min(end));
        }
        (0, end)
    }

    pub fn view(self, project: &Project, now: Instant) -> View {
        let (mut at, mut playing) = (self.at, false);
        if let Some(started) = self.started {
            let elapsed = now.saturating_duration_since(started).as_secs_f64();
            let (first, last) = Self::span(project, self.looping);
            if first < last {
                let continuous = u64::from(self.at)
                    .saturating_add((elapsed * f64::from(clock::TICKS_PER_SECOND)) as u64);
                if self.looping {
                    at = if continuous >= u64::from(last) {
                        first + ((continuous - u64::from(last)) % u64::from(last - first)) as u32
                    } else {
                        continuous as u32
                    };
                    playing = true;
                } else {
                    at = continuous.min(u64::from(last)) as u32;
                    playing = at < last;
                }
                if playing {
                    at = (at / project.frame_step() * project.frame_step()).max(first);
                }
            }
        }
        View {
            version: self.version,
            cut_revision: project.revision,
            sampled_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            at,
            playing,
            looping: self.looping,
            origin: self.origin,
        }
    }

    fn apply(&mut self, project: &Project, command: Command, now: Instant) {
        let view = self.view(project, now);
        let was_playing = view.playing;
        self.at = command.at.unwrap_or(view.at);
        self.looping = command.looping.unwrap_or(self.looping);
        self.origin = command.origin;
        self.version += 1;
        self.started = None;
        if matches!(command.action, Action::Play)
            || matches!(command.action, Action::SetLoop) && was_playing
        {
            let (first, last) = Self::span(project, self.looping);
            if first < last {
                let end = if self.looping {
                    last
                } else {
                    project.duration()
                };
                if self.at < first || self.at >= end.saturating_sub(project.frame_step()) {
                    self.at = first;
                }
                self.started = Some(now);
            }
        }
    }
}

#[derive(Default)]
pub struct Hub(Mutex<HashMap<Uuid, watch::Sender<Transport>>>);

impl Hub {
    pub fn view(&self, project: &Project) -> View {
        let feeds = self.0.lock().expect("playback");
        feeds
            .get(&project.id)
            .map(|feed| *feed.borrow())
            .unwrap_or_else(|| Transport::new(project))
            .view(project, Instant::now())
    }

    pub fn watch(&self, project: &Project) -> watch::Receiver<Transport> {
        self.0
            .lock()
            .expect("playback")
            .entry(project.id)
            .or_insert_with(|| watch::channel(Transport::new(project)).0)
            .subscribe()
    }

    pub fn pause(&self, project: &Project) -> u32 {
        let feeds = self.0.lock().expect("playback");
        let Some(feed) = feeds.get(&project.id) else {
            return project.sequence.playhead;
        };
        let now = Instant::now();
        let mut transport = *feed.borrow();
        let view = transport.view(project, now);
        if transport.started.is_some() {
            transport.apply(
                project,
                Command {
                    action: Action::Pause,
                    at: Some(view.at),
                    looping: None,
                    origin: None,
                },
                now,
            );
            feed.send_replace(transport);
        }
        view.at
    }

    pub fn command(&self, project: &Project, command: Command) -> Result<View> {
        ensure!(
            command.at.is_none_or(|at| at <= clock::MAX_TIME),
            "The playhead is outside the edit clock."
        );
        ensure!(
            !matches!(command.action, Action::Seek) || command.at.is_some(),
            "Choose a time to seek to."
        );
        ensure!(
            !matches!(command.action, Action::SetLoop) || command.looping.is_some(),
            "Choose whether to loop playback."
        );
        let mut feeds = self.0.lock().expect("playback");
        let feed = feeds
            .entry(project.id)
            .or_insert_with(|| watch::channel(Transport::new(project)).0);
        let now = Instant::now();
        let mut transport = *feed.borrow();
        transport.apply(project, command, now);
        feed.send_replace(transport);
        Ok(transport.view(project, now))
    }
}
