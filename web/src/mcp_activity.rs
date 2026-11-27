use crate::director_preview::Frame;
use anyhow::{Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{OwnedMutexGuard, watch};
use uuid::Uuid;

const CLIENT_IDLE: Duration = Duration::from_secs(12 * 60 * 60);
const VIEWER_IDLE: Duration = Duration::from_secs(22);
const WINDOW: usize = 48;
pub const ANIMATION: Duration = Duration::from_millis(730);
pub const SESSION_STATUS: &str = "editroom-mcp-session";
pub const EXPIRED: &str = "expired";

#[derive(Clone, PartialEq, Eq)]
pub enum Owner {
    Bearer(u64),
    Browser(String),
}

#[derive(Clone, Serialize)]
pub struct Agent {
    id: Uuid,
    name: String,
}

impl Agent {
    pub fn anonymous() -> Self {
        Self {
            id: Uuid::nil(),
            name: "MCP agent".into(),
        }
    }
}

struct Client {
    agent: Agent,
    owner: Owner,
    project: Uuid,
    expires: Instant,
}

#[derive(Default)]
pub struct Hub {
    projects: Mutex<HashMap<Uuid, Arc<Channel>>>,
    clients: Mutex<HashMap<Uuid, Client>>,
}

impl Hub {
    pub fn channel(&self, project: Uuid) -> Arc<Channel> {
        self.projects
            .lock()
            .expect("MCP activity")
            .entry(project)
            .or_default()
            .clone()
    }

    pub fn add_client(&self, project: Uuid, owner: Owner, name: &str) -> Uuid {
        let mut clients = self.clients.lock().expect("MCP sessions");
        let now = Instant::now();
        clients.retain(|_, client| client.expires > now);
        if clients.len() >= 256
            && let Some((&oldest, _)) = clients.iter().min_by_key(|(_, client)| client.expires)
        {
            clients.remove(&oldest);
        }
        let id = Uuid::new_v4();
        let agent = Agent {
            id,
            name: display_name(name),
        };
        clients.insert(
            id,
            Client {
                agent,
                owner,
                project,
                expires: now + CLIENT_IDLE,
            },
        );
        id
    }

    pub fn client(&self, project: Uuid, owner: &Owner, id: Uuid) -> Option<Agent> {
        let mut clients = self.clients.lock().expect("MCP sessions");
        let now = Instant::now();
        let client = clients.get_mut(&id).filter(|client| {
            client.expires > now && client.project == project && &client.owner == owner
        })?;
        client.expires = now + CLIENT_IDLE;
        Some(client.agent.clone())
    }

    pub fn close_client(&self, project: Uuid, owner: &Owner, id: Uuid) -> bool {
        let agent = {
            let mut clients = self.clients.lock().expect("MCP sessions");
            if !clients
                .get(&id)
                .is_some_and(|client| client.project == project && &client.owner == owner)
            {
                return false;
            }
            clients.remove(&id).expect("found MCP session").agent
        };
        self.channel(project)
            .publish(&agent, json!({"kind":"disconnected"}));
        true
    }

    pub fn active_attached(&self) -> Vec<Agent> {
        let cutoff = Instant::now() + CLIENT_IDLE - Duration::from_secs(60);
        let clients = self.clients.lock().expect("MCP sessions");
        let mut attached = clients
            .values()
            .filter(|client| matches!(client.owner, Owner::Bearer(_)) && client.expires > cutoff)
            .map(|client| client.agent.clone())
            .collect::<Vec<_>>();
        attached.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        attached.dedup_by(|a, b| a.name == b.name);
        attached
    }

    pub fn disconnect_attached(&self) {
        let mut removed = Vec::new();
        self.clients
            .lock()
            .expect("MCP sessions")
            .retain(|_, client| {
                if matches!(client.owner, Owner::Bearer(_)) {
                    removed.push((client.project, client.agent.clone()));
                    false
                } else {
                    true
                }
            });
        for (project, agent) in removed {
            self.channel(project)
                .publish(&agent, json!({"kind":"disconnected"}));
        }
    }

    pub fn frame(&self, edit: Uuid, sequence: Option<u64>) -> Option<Arc<Frame>> {
        self.projects
            .lock()
            .expect("MCP activity")
            .values()
            .find_map(|channel| {
                channel
                    .state
                    .lock()
                    .expect("MCP edit feed")
                    .preview
                    .as_ref()
                    .filter(|preview| {
                        preview.id == edit && sequence.is_none_or(|n| preview.frame.sequence == n)
                    })
                    .map(|preview| preview.frame.clone())
            })
    }
}

fn display_name(raw: &str) -> String {
    let raw = raw
        .chars()
        .filter(|character| !character.is_control())
        .take(80)
        .collect::<String>();
    let lower = raw.to_ascii_lowercase();
    if lower.contains("claude") {
        return "Claude".into();
    }
    if lower.contains("codex") {
        return "Codex".into();
    }
    if lower.contains("chatgpt") || lower.contains("openai") {
        return "ChatGPT".into();
    }
    let words = raw.replace(['-', '_'], " ");
    let name = words.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "MCP agent".into(),
    }
}

struct Packet {
    cursor: u64,
    value: Value,
}
struct Active {
    id: Uuid,
    frame: Arc<Frame>,
    packet: Value,
}

#[derive(Default)]
struct State {
    cursor: u64,
    events: VecDeque<Packet>,
    preview: Option<Active>,
    viewers: HashMap<Uuid, Instant>,
}

#[derive(Default)]
pub struct Channel {
    pub edit: Arc<tokio::sync::Mutex<()>>,
    state: Mutex<State>,
    signal: watch::Sender<u64>,
}

impl Channel {
    pub fn director_turn(&self) -> Result<OwnedMutexGuard<()>> {
        self.edit
            .clone()
            .try_lock_owned()
            .map_err(|_| anyhow::anyhow!("An attached agent is finishing an edit."))
    }

    fn record(&self, state: &mut State, value: Value) {
        state.cursor += 1;
        state.events.push_back(Packet {
            cursor: state.cursor,
            value,
        });
        while state.events.len() > WINDOW {
            state.events.pop_front();
        }
        self.signal.send_replace(state.cursor);
    }

    fn event(agent: &Agent, mut value: Value) -> Value {
        value["agent"] = json!(agent);
        value
    }

    pub fn publish(&self, agent: &Agent, value: Value) {
        self.record(
            &mut self.state.lock().expect("MCP edit feed"),
            Self::event(agent, value),
        );
    }

    pub fn focus(&self, agent: &Agent, label: &str, asset: Option<Uuid>) {
        self.publish(agent, json!({"kind":"focus","label":label,"asset":asset}));
    }

    pub fn has_viewer(&self) -> bool {
        let mut state = self.state.lock().expect("MCP edit feed");
        state.viewers.retain(|_, expires| *expires > Instant::now());
        !state.viewers.is_empty()
    }

    pub fn prepare(
        self: &Arc<Self>,
        agent: &Agent,
        label: &str,
        frame: Frame,
    ) -> Result<PreviewGuard> {
        let id = Uuid::new_v4();
        let frame = Arc::new(frame);
        let packet = Self::event(
            agent,
            json!({"kind":"preview","edit":id,"label":label,"frame":frame.json()}),
        );
        let mut state = self.state.lock().expect("MCP edit feed");
        ensure!(
            state.preview.is_none(),
            "An attached agent is finishing an edit."
        );
        state.preview = Some(Active {
            id,
            frame,
            packet: packet.clone(),
        });
        self.record(&mut state, packet);
        Ok(PreviewGuard {
            channel: self.clone(),
            id,
            agent: agent.clone(),
            finished: false,
        })
    }

    pub async fn observe(&self, viewer: Uuid, after: Option<u64>) -> Value {
        let mut signal = self.signal.subscribe();
        {
            let mut state = self.state.lock().expect("MCP edit feed");
            let now = Instant::now();
            state.viewers.retain(|_, expires| *expires > now);
            if state.viewers.len() < 64 || state.viewers.contains_key(&viewer) {
                state.viewers.insert(viewer, now + VIEWER_IDLE);
            }
            if after.is_none() {
                let events = state
                    .preview
                    .as_ref()
                    .map(|preview| &preview.packet)
                    .into_iter()
                    .collect::<Vec<_>>();
                return json!({"cursor":state.cursor,"events":events,"active":state.preview.as_ref().map(|preview|preview.id)});
            }
        }
        let after = after.expect("checked cursor");
        let _ = tokio::time::timeout(
            Duration::from_secs(14),
            signal.wait_for(|version| *version != after),
        )
        .await;
        let state = self.state.lock().expect("MCP edit feed");
        let events = if after > state.cursor
            || state
                .events
                .front()
                .is_some_and(|entry| after + 1 < entry.cursor)
        {
            state
                .preview
                .as_ref()
                .map(|preview| &preview.packet)
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            state
                .events
                .iter()
                .filter(|event| event.cursor > after)
                .map(|event| &event.value)
                .collect()
        };
        json!({"cursor":state.cursor,"events":events,"active":state.preview.as_ref().map(|preview|preview.id)})
    }
}

pub struct PreviewGuard {
    channel: Arc<Channel>,
    id: Uuid,
    agent: Agent,
    finished: bool,
}

impl PreviewGuard {
    pub fn finish(mut self, revision: u64) {
        self.end(Some(revision));
    }
    fn end(&mut self, revision: Option<u64>) {
        if self.finished {
            return;
        }
        let mut state = self.channel.state.lock().expect("MCP edit feed");
        if state
            .preview
            .as_ref()
            .is_some_and(|preview| preview.id == self.id)
        {
            state.preview = None;
        }
        // Keep a small replay for a slow browser and release the complete scene promptly.
        let id = self.id.to_string();
        if let Some(packet) = state
            .events
            .iter_mut()
            .rev()
            .find(|packet| packet.value["edit"] == id && packet.value["kind"] == "preview")
        {
            if let Some(revision) = revision {
                packet.value["kind"] = json!("replay");
                packet.value["revision"] = json!(revision);
                if let Some(frame) = packet.value["frame"].as_object_mut() {
                    frame.remove("scene");
                }
            } else {
                packet.value = Channel::event(
                    &self.agent,
                    json!({"kind":"finish","edit":self.id,"revision":null}),
                );
            }
        }
        self.channel.record(
            &mut state,
            Channel::event(
                &self.agent,
                json!({"kind":"finish","edit":self.id,"revision":revision}),
            ),
        );
        self.finished = true;
    }
}

impl Drop for PreviewGuard {
    fn drop(&mut self) {
        self.end(None);
    }
}
