use crate::editor::{self, Citation, Message, Project, Run};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub const MAX_MESSAGES: usize = 100;
pub const MAX_EVENTS: usize = 300;
const MAX_TURNS: usize = 20;
const COCOA_EPOCH: u64 = 978_307_200;
const RUN_FAILED: &str = "This run ended before completion.";

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptEntry {
    pub text: String,
    pub tab: String,
    pub date: f64,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShotPlan {
    continuity: String,
    shots: Vec<Shot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    total_seconds: Option<f64>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Shot {
    title: String,
    direction: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_seconds: Option<f64>,
}

impl From<&crate::provider::Plan> for ShotPlan {
    fn from(plan: &crate::provider::Plan) -> Self {
        Self {
            continuity: plan.continuity.clone(),
            shots: plan
                .shots
                .iter()
                .map(|shot| Shot {
                    title: shot.title.clone(),
                    direction: shot.direction.clone(),
                    duration_seconds: Some(shot.duration_seconds),
                })
                .collect(),
            total_seconds: plan.total_seconds,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    id: Uuid,
    title: String,
    detail: Option<String>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<ShotPlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<Activity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asset_ids: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
}

impl Turn {
    pub fn new(id: Uuid, plan: Option<ShotPlan>, asset_ids: Vec<Uuid>) -> Self {
        Self {
            id,
            plan,
            events: vec![],
            asset_ids,
            failed: false,
        }
    }

    pub fn remember_events(&mut self, events: &[String]) {
        self.events = events
            .iter()
            .filter_map(|description| {
                Some(Activity {
                    id: Uuid::new_v4(),
                    title: bounded(description, 600)?,
                    detail: None,
                })
            })
            .take(MAX_EVENTS)
            .collect();
    }

    pub fn record_event(&mut self, description: &str) {
        let Some(title) = bounded(description, 600) else {
            return;
        };
        if self.events.len() >= MAX_EVENTS {
            self.events.remove(0);
        }
        self.events.push(Activity {
            id: Uuid::new_v4(),
            title,
            detail: None,
        });
    }

    fn import(&mut self, assets: &HashMap<Uuid, Uuid>) {
        self.asset_ids = self
            .asset_ids
            .iter()
            .filter_map(|id| assets.get(id).copied())
            .take(100)
            .collect();
        self.events = self
            .events
            .drain(..)
            .filter_map(|mut event| {
                event.title = bounded(&event.title, 600)?;
                event.detail = event
                    .detail
                    .as_deref()
                    .and_then(|detail| bounded(detail, 600));
                Some(event)
            })
            .take(MAX_EVENTS)
            .collect();
        if let Some(plan) = &mut self.plan {
            plan.continuity = plan.continuity.chars().take(2000).collect();
            plan.shots = plan
                .shots
                .drain(..)
                .filter_map(|mut shot| {
                    shot.title = bounded(&shot.title, 100)?;
                    shot.direction = bounded(&shot.direction, 2000)?;
                    shot.duration_seconds = shot
                        .duration_seconds
                        .filter(|duration| (1.0..=15.0).contains(duration));
                    Some(shot)
                })
                .take(crate::MAX_SHOTS)
                .collect();
            if plan.shots.is_empty() {
                self.plan = None;
            }
        }
    }
}

// The Swift document keeps its Quick Clips exchanges in this shape.
#[derive(Deserialize, Serialize)]
struct NativeTurn {
    id: Uuid,
    prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reply: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan: Option<ShotPlan>,
    #[serde(default)]
    events: Vec<Activity>,
    #[serde(default, rename = "assetIDs")]
    asset_ids: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
}

fn bounded(text: &str, max: usize) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.chars().take(max).collect())
}

fn recent_prompts(entries: impl IntoIterator<Item = PromptEntry>) -> Vec<PromptEntry> {
    let mut seen = HashSet::new();
    entries
        .into_iter()
        .filter_map(|mut entry| {
            entry.text = bounded(&entry.text, 3000)?;
            if !seen.insert(entry.text.clone()) {
                return None;
            }
            if !matches!(entry.tab.as_str(), "video" | "claude" | "chatgpt" | "local") {
                entry.tab = "local".into();
            }
            Some(entry)
        })
        .take(MAX_TURNS)
        .collect()
}

pub fn remember_prompt(history: &mut Vec<PromptEntry>, text: &str, tab: &str) {
    let entry = PromptEntry {
        text: text.into(),
        tab: tab.into(),
        date: editor::now().saturating_sub(COCOA_EPOCH) as f64,
    };
    *history = recent_prompts(std::iter::once(entry).chain(std::mem::take(history)));
}

pub fn context(messages: &[Message], skip: usize, limit: usize) -> Vec<Value> {
    let start = messages.len().saturating_sub(skip + limit);
    let end = messages.len().saturating_sub(skip);
    messages[start..end].iter().map(|message| {
        let mut value = serde_json::json!({"from":message.from,"text":message.text,"citations":message.citations});
        if let Some(studio) = &message.studio {
            value["shot_plan"] = serde_json::json!(studio.plan);
            value["footage_ids"] = serde_json::json!(studio.asset_ids);
        }
        value
    }).collect()
}

pub fn run_status(project: &Project, run: Option<&Run>, id: Uuid) -> Option<Value> {
    let archived = project
        .messages
        .iter()
        .rev()
        .find_map(|message| message.studio.as_ref().filter(|turn| turn.id == id));
    let events = if let Some(run) = run {
        run.events.iter().map(String::as_str).collect::<Vec<_>>()
    } else {
        archived?
            .events
            .iter()
            .map(|event| event.title.as_str())
            .collect()
    };
    let status = match run.map(|run| run.status.as_str()) {
        Some("running") => "running",
        Some("failed") => "failed",
        None if archived.is_some_and(|turn| turn.failed) => "failed",
        _ => "completed",
    };
    let asset_ids = run
        .and_then(|run| run.studio_turn.as_ref())
        .or(archived)
        .into_iter()
        .flat_map(|turn| &turn.asset_ids)
        .filter(|id| project.source(**id).is_some())
        .collect::<Vec<_>>();
    let mut summary = json!({"run_id":id,"status":status,"asset_ids":asset_ids,"events":events});
    if let Some(latest) = archived
        .and_then(|turn| turn.events.last().map(|event| event.title.as_str()))
        .or_else(|| events.last().copied())
    {
        summary["latest_activity"] = json!(latest);
    }
    if status == "failed" {
        summary["error"] = json!(
            run.and_then(|run| run.events.last().map(String::as_str))
                .unwrap_or(RUN_FAILED)
        );
    }
    Some(summary)
}

pub fn import(
    native: &Value,
    web: &Value,
    assets: &HashMap<Uuid, Uuid>,
) -> (Vec<Message>, Vec<PromptEntry>) {
    let mut messages = if let Some(stored) = web["messages"].as_array() {
        stored
            .iter()
            .rev()
            .filter_map(|input| {
                let mut message: Message = serde_json::from_value(input.clone()).ok()?;
                if !matches!(message.from.as_str(), "you" | "director") {
                    return None;
                }
                if message.text.trim().is_empty() && message.studio.is_none() {
                    return None;
                }
                message.text = message.text.chars().take(16_000).collect();
                let length = message.text.chars().count();
                message.citations = message
                    .citations
                    .into_iter()
                    .filter_map(|source| {
                        Citation::new(
                            &source.url,
                            &source.label,
                            source.after.map(|at| at.min(length)),
                        )
                    })
                    .take(12)
                    .collect();
                if let Some(studio) = &mut message.studio {
                    studio.import(assets);
                }
                Some(message)
            })
            .take(MAX_MESSAGES)
            .collect::<Vec<_>>()
    } else {
        native["videoStudioTurns"]
            .as_array()
            .into_iter()
            .flatten()
            .rev()
            .filter_map(|input| {
                let turn: NativeTurn = serde_json::from_value(input.clone()).ok()?;
                let prompt = bounded(&turn.prompt, 3000)?;
                let mut metadata = Turn {
                    id: turn.id,
                    plan: turn.plan,
                    events: turn.events,
                    asset_ids: turn.asset_ids,
                    failed: turn.failure.is_some(),
                };
                metadata.import(assets);
                let mut reply = Message::new(
                    "director",
                    turn.reply
                        .as_deref()
                        .and_then(|text| bounded(text, 16_000))
                        .unwrap_or_default(),
                );
                if !reply.text.is_empty()
                    || metadata.plan.is_some()
                    || !metadata.events.is_empty()
                    || !metadata.asset_ids.is_empty()
                    || metadata.failed
                {
                    reply.studio = Some(metadata);
                }
                Some(
                    [reply, Message::new("you", prompt)]
                        .into_iter()
                        .filter(|message| !message.text.is_empty() || message.studio.is_some()),
                )
            })
            .take(MAX_TURNS)
            .flatten()
            .collect()
    };
    messages.reverse();
    let entries = web["promptHistory"]
        .as_array()
        .or_else(|| native["magicHistory"].as_array());
    let prompts = recent_prompts(
        entries
            .into_iter()
            .flatten()
            .filter_map(|entry| serde_json::from_value(entry.clone()).ok()),
    );
    (messages, prompts)
}

pub fn native_turns(messages: &[Message], assets: &HashSet<Uuid>) -> Value {
    let mut turns = Vec::new();
    let mut prompt = None;
    for message in messages {
        if message.from == "you" {
            if let Some(previous) = prompt.replace(message.text.clone()) {
                turns.push(native_turn(previous, None, assets));
            }
        } else if let Some(prompt) = prompt.take() {
            turns.push(native_turn(prompt, Some(message), assets));
        }
    }
    if let Some(prompt) = prompt {
        turns.push(native_turn(prompt, None, assets));
    }
    let start = turns.len().saturating_sub(MAX_TURNS);
    serde_json::json!(&turns[start..])
}

fn native_turn(prompt: String, reply: Option<&Message>, assets: &HashSet<Uuid>) -> NativeTurn {
    let studio = reply.and_then(|message| message.studio.as_ref());
    NativeTurn {
        id: studio.map_or_else(Uuid::new_v4, |studio| studio.id),
        prompt,
        reply: reply.and_then(|message| bounded(&message.text, 16_000)),
        plan: studio.and_then(|studio| studio.plan.clone()),
        events: studio
            .map(|studio| studio.events.clone())
            .unwrap_or_default(),
        asset_ids: studio
            .map(|studio| {
                studio
                    .asset_ids
                    .iter()
                    .copied()
                    .filter(|id| assets.contains(id))
                    .collect()
            })
            .unwrap_or_default(),
        failure: studio
            .filter(|studio| studio.failed)
            .map(|_| RUN_FAILED.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{media, room};
    use serde_json::json;

    #[test]
    fn native_turns_keep_shot_timing_activity_and_remapped_footage() {
        let run = Uuid::new_v4();
        let old_asset = Uuid::new_v4();
        let new_asset = Uuid::new_v4();
        let event = Uuid::new_v4();
        let native = json!({"videoStudioTurns":[
            {"id":run,"prompt":"Cross the bridge","reply":"One shot is ready.",
             "plan":{"continuity":"Fog over the bay","shots":[
                 {"title":"Crossing","direction":"Follow the cyclist"},
                 {"title":"Arrival","direction":"Stop at the pier","durationSeconds":10.0}
             ]},"events":[{"id":event,"title":"Shot one ready","detail":"Reviewed"}],
             "assetIDs":[old_asset,Uuid::new_v4()],"failure":"Private worker diagnostics"}
        ]});
        let (messages, _) = import(
            &native,
            &Value::Null,
            &HashMap::from([(old_asset, new_asset)]),
        );
        assert_eq!(messages[0].text, "Cross the bridge");
        assert_eq!(messages[1].text, "One shot is ready.");
        let turn = messages[1].studio.as_ref().unwrap();
        assert!(turn.failed);
        assert_eq!(turn.asset_ids, [new_asset]);
        let exported = native_turns(&messages, &HashSet::from([new_asset]));
        assert_eq!(exported[0]["id"], json!(run));
        assert_eq!(exported[0]["plan"], native["videoStudioTurns"][0]["plan"]);
        assert_eq!(
            exported[0]["events"],
            native["videoStudioTurns"][0]["events"]
        );
        assert_eq!(exported[0]["assetIDs"], json!([new_asset]));
        assert!(!exported.to_string().contains("Private worker diagnostics"));
        let recent = context(&messages, 0, 1);
        assert_eq!(recent[0]["shot_plan"], exported[0]["plan"]);
        assert_eq!(recent[0]["footage_ids"], json!([new_asset]));
        assert!(recent[0].get("events").is_none());
        let mut project = Project::new("Bridge".into());
        project.sources.push(editor::Source {
            id: new_asset,
            storage: None,
            title: "Crossing".into(),
            duration: crate::clock::TICKS_PER_SECOND,
            video: true,
            audio: false,
            image: false,
            notes: String::new(),
            origin_stamp: None,
            device_digest: None,
        });
        project.messages = messages;
        let archived = run_status(&project, None, run).unwrap();
        assert_eq!(archived["latest_activity"], "Shot one ready");
        assert_eq!(archived["error"], RUN_FAILED);
        assert_eq!(archived["asset_ids"], json!([new_asset]));
        let copy = project.copy_as("Another cut".into());
        assert_eq!(
            run_status(&copy, None, run).unwrap()["asset_ids"],
            json!([copy.sources[0].id])
        );
        assert_ne!(copy.sources[0].id, new_asset);
        assert!(run_status(&Project::new("Unrelated".into()), None, run).is_none());
    }

    #[tokio::test]
    async fn room_round_trip_preserves_mac_prompts_and_web_replies() -> anyhow::Result<()> {
        let scratch = std::env::temp_dir().join(format!("editroom-history-{}", Uuid::new_v4()));
        let result = async {
            let store = editor::Store::open(&scratch)?;
            let native = json!({"project":{"assets":[],"clips":[],
                "videoStudioTurns":[{"id":Uuid::new_v4(),"prompt":"Animate the river","reply":"I can plan that shot."}],
                "magicHistory":[{"text":"Find music","tab":"claude","date":800000000.5},
                                {"text":"Animate the river","tab":"video","date":800000000}]
            }});
            let original = room::import_project(&store, &native, "River", room::RoomFiles::Host(&scratch)).await?;
            let mut project = store.read(|db| db.projects.iter().find(|project| project.id == original).unwrap().clone());
            assert_eq!(project.messages.len(), 2);
            remember_prompt(&mut project.prompt_history, "Animate the river", "chatgpt");
            assert_eq!(project.prompt_history.len(), 2);
            assert_eq!(project.prompt_history[0].tab, "chatgpt");
            assert_eq!(project.prompt_history[1].date, 800000000.5);
            let mut message = Message::new("director", "Found an example.".into());
            message.citations.push(Citation::new("https://example.org/example", "Example", None).unwrap());
            project.messages.push(message);
            let dir = media::temp(&store).await?;
            let archive = room::bundle(&store, &project, &dir).await?;
            let unpacked = media::temp(&store).await?;
            let file = room::unpack(&archive, &unpacked)?;
            let doc = serde_json::from_slice(&std::fs::read(&file)?)?;
            let reopened = room::import_project(&store, &doc, "River copy", room::RoomFiles::Archive {
                root: &unpacked, document: &file
            }).await?;
            store.read(|db| {
                let copy = db.projects.iter().find(|project| project.id == reopened).unwrap();
                assert!(copy.messages == project.messages);
                assert!(copy.prompt_history == project.prompt_history);
            });
            Ok(())
        }.await;
        let _ = std::fs::remove_dir_all(scratch);
        result
    }
}
