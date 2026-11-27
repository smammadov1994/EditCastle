use crate::editor::{Project, VisibleEdit};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;

const RETAINED_FRAMES: usize = 12;

pub struct Frame {
    pub sequence: u64,
    pub project: Project,
    scene: Value,
    edits: Vec<VisibleEdit>,
}

impl Frame {
    pub fn new(sequence: u64, project: &Project, edits: Vec<VisibleEdit>) -> Self {
        let mut project = project.clone();
        project.undo_steps.clear();
        project.redo_steps.clear();
        project.messages.clear();
        let scene = project.public();
        Self {
            sequence,
            project,
            scene,
            edits,
        }
    }

    pub fn json(&self) -> Value {
        json!({"sequence":self.sequence,"base_revision":self.project.revision,"scene":self.scene,"edits":self.edits})
    }
}

#[derive(Clone, Copy, Default)]
struct State {
    frame: u64,
    activity: u64,
    closed: bool,
}

pub struct Feed {
    signal: watch::Sender<State>,
    frames: Mutex<VecDeque<Arc<Frame>>>,
}

impl Feed {
    pub fn new() -> Self {
        let (signal, _) = watch::channel(State::default());
        Self {
            signal,
            frames: Mutex::new(VecDeque::new()),
        }
    }

    pub fn push(&self, project: &Project, edits: Vec<VisibleEdit>) {
        let mut frames = self.frames.lock().expect("director draft frames");
        let previous = *self.signal.borrow();
        if previous.closed {
            return;
        }
        let sequence = previous.frame + 1;
        frames.push_back(Arc::new(Frame::new(sequence, project, edits)));
        if frames.len() > RETAINED_FRAMES {
            frames.pop_front();
        }
        self.signal.send_modify(|state| state.frame = sequence);
    }

    pub fn activity(&self) {
        self.signal.send_modify(|state| state.activity += 1);
    }

    pub async fn after(&self, after: u64, activity: Option<u64>) -> Value {
        let mut signal = self.signal.subscribe();
        let latest = *signal.borrow_and_update();
        if latest.frame <= after
            && activity.is_none_or(|seen| latest.activity <= seen)
            && !latest.closed
        {
            let _ = tokio::time::timeout(
                Duration::from_secs(15),
                signal.wait_for(|state| {
                    state.closed
                        || state.frame > after
                        || activity.is_some_and(|seen| state.activity > seen)
                }),
            )
            .await;
        }
        let frames = self.frames.lock().expect("director draft frames");
        let latest = *self.signal.borrow();
        let mut result = json!({"finished":latest.closed,"frames":frames.iter().filter(|frame|frame.sequence>after)
            .map(|frame|frame.json()).collect::<Vec<_>>()});
        if activity.is_some_and(|seen| latest.activity > seen) {
            result["activity_version"] = json!(latest.activity);
        }
        result
    }

    pub fn frame(&self, sequence: Option<u64>) -> Option<Arc<Frame>> {
        let frames = self.frames.lock().expect("director draft frames");
        if let Some(sequence) = sequence {
            frames
                .iter()
                .find(|frame| frame.sequence == sequence)
                .cloned()
        } else {
            frames.back().cloned()
        }
    }

    pub fn close(&self) {
        let _frames = self.frames.lock().expect("director draft frames");
        self.signal.send_modify(|state| state.closed = true);
    }

    pub fn is_closed(&self) -> bool {
        self.signal.borrow().closed
    }

    pub async fn closed(&self) {
        let mut receiver = self.signal.subscribe();
        let _ = receiver.wait_for(|state| state.closed).await;
    }
}
