use crate::provider::Name;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

const STREAM_LIMIT: usize = 32 * 1024 * 1024;

pub fn research_activity(name: &str) -> Option<&'static str> {
    match name {
        "search" | "web_search" => Some("Searching public sources"),
        "open_page" | "find_in_page" | "web_fetch" => Some("Reading a public source page"),
        _ => None,
    }
}

#[derive(Default)]
struct EventReader {
    line: Vec<u8>,
    data: Vec<u8>,
    total: usize,
}

impl EventReader {
    fn read(&mut self, fragment: &[u8]) -> Result<Vec<Value>> {
        self.total += fragment.len();
        ensure!(
            self.total <= STREAM_LIMIT,
            "The provider response was too large."
        );
        let mut events = Vec::new();
        for slice in fragment.split_inclusive(|byte| *byte == b'\n') {
            let Some(line) = slice.strip_suffix(b"\n") else {
                self.line.extend_from_slice(slice);
                break;
            };
            self.line.extend_from_slice(line);
            let line = self.line.strip_suffix(b"\r").unwrap_or(&self.line);
            if line.is_empty() {
                if !self.data.is_empty() && self.data != b"[DONE]" {
                    let value = serde_json::from_slice(&self.data)
                        .context("The provider returned an invalid stream event.")?;
                    events.push(value);
                }
                self.data.clear();
            } else if let Some(part) = line.strip_prefix(b"data:") {
                if !self.data.is_empty() {
                    self.data.push(b'\n');
                }
                self.data
                    .extend_from_slice(part.strip_prefix(b" ").unwrap_or(part));
            }
            self.line.clear();
        }
        Ok(events)
    }
}

pub struct Hosted {
    reader: EventReader,
    provider: ProviderStream,
    activities: HashSet<String>,
    notes: usize,
}

enum ProviderStream {
    OpenAi,
    Claude(ClaudeMessage),
}

impl Hosted {
    pub fn new(name: Name) -> Result<Self> {
        let provider = match name {
            Name::OpenAi => ProviderStream::OpenAi,
            Name::Claude => ProviderStream::Claude(ClaudeMessage::default()),
            _ => bail!("This planner does not use a hosted response stream."),
        };
        Ok(Self {
            reader: EventReader::default(),
            provider,
            activities: HashSet::new(),
            notes: 0,
        })
    }

    pub fn showed(&self, message: &str) -> bool {
        self.activities.contains(message)
    }

    pub fn push(&mut self, bytes: &[u8], report: &mut impl FnMut(&str)) -> Result<Option<Value>> {
        for mut event in self.reader.read(bytes)? {
            let kind = event["type"].as_str().unwrap_or_default();
            if kind == "error" {
                let code = event["error"]["type"]
                    .as_str()
                    .or_else(|| event["code"].as_str())
                    .unwrap_or_default();
                match code {
                    "overloaded_error" | "server_overloaded" => {
                        bail!("The planner is overloaded. Try again shortly.")
                    }
                    "rate_limit_error" | "rate_limit_exceeded" => {
                        bail!("The planner is out of quota or rate limited.")
                    }
                    _ => bail!("The provider stopped the response before completing the edit."),
                }
            }
            let mut publish = |message: &str| {
                if self.activities.insert(message.to_owned()) {
                    report(message);
                }
            };
            match &mut self.provider {
                ProviderStream::OpenAi => match kind {
                    "response.output_item.added" => match event["item"]["type"].as_str() {
                        Some("reasoning") => publish("Planning the edit"),
                        Some("function_call") => publish("Preparing an EditCastle tool"),
                        _ => {}
                    },
                    "response.web_search_call.in_progress"
                    | "response.web_search_call.searching" => publish("Searching public sources"),
                    "response.output_text.delta" => publish("Writing a response"),
                    "response.output_item.done" => {
                        let item = &event["item"];
                        if item["type"] == "message" && item["phase"] == "commentary" {
                            for part in item["content"].as_array().into_iter().flatten() {
                                if let Some(text) = part["text"].as_str().map(str::trim)
                                    && !text.is_empty()
                                    && self.notes < 4
                                {
                                    publish(&text.chars().take(240).collect::<String>());
                                    self.notes += 1;
                                }
                            }
                        }
                        if item["type"] == "web_search_call"
                            && let Some(activity) = research_activity(
                                item["action"]["type"].as_str().unwrap_or_default(),
                            )
                        {
                            publish(activity);
                        }
                    }
                    "response.completed" => {
                        let body = event["response"].take();
                        ensure!(
                            body["output"].is_array(),
                            "OpenAI returned no completed output."
                        );
                        return Ok(Some(body));
                    }
                    "response.incomplete" | "response.failed" => {
                        bail!("The provider stopped the response before completing the edit.")
                    }
                    _ => {}
                },
                ProviderStream::Claude(message) => {
                    if let Some(body) = message.push(event, &mut publish)? {
                        return Ok(Some(body));
                    }
                }
            }
        }
        Ok(None)
    }
}

#[derive(Default)]
struct ClaudeMessage {
    blocks: BTreeMap<u64, Block>,
    started: bool,
    stop_reason: Option<String>,
}

struct Block {
    value: Value,
    input: String,
    ended: bool,
}

impl ClaudeMessage {
    fn push(&mut self, mut event: Value, publish: &mut impl FnMut(&str)) -> Result<Option<Value>> {
        let index = || {
            event["index"]
                .as_u64()
                .context("Claude returned an invalid content block.")
        };
        match event["type"].as_str().unwrap_or_default() {
            "message_start" => self.started = true,
            "content_block_start" => {
                let id = index()?;
                let value = event["content_block"].take();
                match value["type"].as_str() {
                    Some("thinking" | "redacted_thinking") => publish("Planning the edit"),
                    Some("tool_use") => publish("Preparing an EditCastle tool"),
                    Some("text") => publish("Writing a response"),
                    Some("server_tool_use") => {
                        if let Some(activity) =
                            research_activity(value["name"].as_str().unwrap_or_default())
                        {
                            publish(activity);
                        }
                    }
                    _ => {}
                }
                ensure!(
                    value.is_object() && !self.blocks.contains_key(&id),
                    "Claude returned an invalid content block."
                );
                self.blocks.insert(
                    id,
                    Block {
                        value,
                        input: String::new(),
                        ended: false,
                    },
                );
            }
            "content_block_delta" => {
                let block = self
                    .blocks
                    .get_mut(&index()?)
                    .filter(|block| !block.ended)
                    .context("Claude returned a delta outside its content block.")?;
                let delta = &event["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => append(&mut block.value, "text", delta),
                    Some("thinking_delta") => append(&mut block.value, "thinking", delta),
                    Some("signature_delta") => append(&mut block.value, "signature", delta),
                    Some("input_json_delta") => block
                        .input
                        .push_str(delta["partial_json"].as_str().unwrap_or_default()),
                    Some("citations_delta") => {
                        if !block.value["citations"].is_array() {
                            block.value["citations"] = json!([]);
                        }
                        block.value["citations"]
                            .as_array_mut()
                            .unwrap()
                            .push(delta["citation"].clone());
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let block = self
                    .blocks
                    .get_mut(&index()?)
                    .filter(|block| !block.ended)
                    .context("Claude returned an invalid content block end.")?;
                if !block.input.is_empty() {
                    let input: Value = serde_json::from_str(&block.input)
                        .context("Claude did not finish building a tool call.")?;
                    ensure!(input.is_object(), "Claude returned invalid tool arguments.");
                    block.value["input"] = input;
                    block.input.clear();
                }
                block.ended = true;
            }
            "message_delta" => {
                if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(reason.to_owned());
                }
            }
            "message_stop" => {
                ensure!(
                    self.started && self.blocks.values().all(|block| block.ended),
                    "Claude ended the response before completing it."
                );
                let reason = self
                    .stop_reason
                    .take()
                    .context("Claude ended the response before completing it.")?;
                ensure!(
                    matches!(
                        reason.as_str(),
                        "end_turn" | "tool_use" | "pause_turn" | "stop_sequence"
                    ),
                    "Claude stopped the response before completing the edit."
                );
                return Ok(Some(
                    json!({"content":std::mem::take(&mut self.blocks).into_values().map(|block| block.value).collect::<Vec<_>>(),
                    "stop_reason":reason}),
                ));
            }
            _ => {}
        }
        Ok(None)
    }
}

fn append(block: &mut Value, field: &str, delta: &Value) {
    if let Some(text) = delta[field].as_str() {
        if let Some(existing) = block.get_mut(field)
            && let Value::String(buffer) = existing
        {
            buffer.push_str(text);
        } else {
            block[field] = Value::String(text.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(
        stream: &mut Hosted,
        raw: Value,
        activities: &mut Vec<String>,
    ) -> Result<Option<Value>> {
        stream.push(
            format!("event: sample\r\ndata: {raw}\r\n\r\n").as_bytes(),
            &mut |text| activities.push(text.into()),
        )
    }

    #[test]
    fn openai_emits_only_public_activity_and_waits_for_the_full_tool() {
        let mut stream = Hosted::new(Name::OpenAi).unwrap();
        let mut cues = vec![];
        let events = [
            json!({"type":"response.output_item.added","item":{"type":"reasoning"}}),
            json!({"type":"response.reasoning_summary_text.delta","delta":"internal text"}),
            json!({"type":"response.output_item.done","item":{"type":"message","phase":"commentary","content":[{"type":"output_text","text":"Checking the résumé."}]}}),
            json!({"type":"response.output_item.added","item":{"type":"function_call","name":"move_clip"}}),
            json!({"type":"response.function_call_arguments.delta","delta":"{\"clip_id\":"}),
        ];
        for event in events {
            // Reassemble some multi-byte characters and line endings across separate chunks.
            let s = format!("event: response\r\ndata: {event}\r\n\r\n");
            for bytes in s.as_bytes().chunks(3) {
                assert!(
                    stream
                        .push(bytes, &mut |cue| cues.push(cue.into()))
                        .unwrap()
                        .is_none()
                );
            }
        }
        assert_eq!(
            cues,
            [
                "Planning the edit",
                "Checking the résumé.",
                "Preparing an EditCastle tool"
            ]
        );
        let final_response = push(
            &mut stream,
            json!({"type":"response.completed","response":{"output":[
                {"type":"function_call","name":"move_clip","arguments":"{\"clip_id\":\"clip\"}"}
            ]}}),
            &mut cues,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            final_response["output"][0]["arguments"],
            "{\"clip_id\":\"clip\"}"
        );
        assert!(stream.showed("Checking the résumé."));
    }

    #[test]
    fn claude_preserves_tool_json_citations_and_private_signatures_without_displaying_them() {
        let mut stream = Hosted::new(Name::Claude).unwrap();
        let mut cues = vec![];
        let packets = [
            json!({"type":"message_start","message":{"content":[]}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"private thought"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"signed"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":"","citations":null}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"License verified."}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"citations_delta","citation":{"url":"https://music.example/track","title":"License"}}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","name":"set_marker","input":{},"id":"tool-1"}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"time\":"}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"2}"}}),
            json!({"type":"content_block_stop","index":2}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
        ];
        for packet in packets {
            assert!(push(&mut stream, packet, &mut cues).unwrap().is_none());
        }
        let finished = push(&mut stream, json!({"type":"message_stop"}), &mut cues)
            .unwrap()
            .unwrap();
        assert_eq!(
            cues,
            [
                "Planning the edit",
                "Writing a response",
                "Preparing an EditCastle tool"
            ]
        );
        assert_eq!(finished["content"][0]["signature"], "signed");
        assert_eq!(finished["content"][0]["thinking"], "private thought");
        assert_eq!(finished["content"][1]["text"], "License verified.");
        assert_eq!(finished["content"][1]["citations"][0]["title"], "License");
        assert_eq!(finished["content"][2]["input"], json!({"time":2}));
    }

    #[test]
    fn incomplete_responses_do_not_produce_edits() {
        let mut cues = vec![];
        let mut openai = Hosted::new(Name::OpenAi).unwrap();
        assert!(
            push(
                &mut openai,
                json!({"type":"response.incomplete"}),
                &mut cues
            )
            .is_err()
        );
        let mut claude = Hosted::new(Name::Claude).unwrap();
        assert!(push(&mut claude, json!({"type":"message_stop"}), &mut cues).is_err());
    }
}
