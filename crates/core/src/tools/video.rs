//! Screen recording tools: `record_start`, `record_stop`, `video_render`.
//! The agent records the browser (or a connected app) while it works, then
//! edits the recording from its own event track — see [`crate::video`].

use std::path::PathBuf;

use async_trait::async_trait;
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;
use crate::video::{self, Edl};

fn ok(call: &ToolCall, output: String) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        success: true,
        output,
        error: None,
    }
}

fn fail(call: &ToolCall, e: impl Into<String>) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        success: false,
        output: String::new(),
        error: Some(e.into()),
    }
}

pub struct RecordStartTool {
    pub workspace_root: PathBuf,
}

#[async_trait]
impl ToolExecutor for RecordStartTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "record_start".into(),
            description: "Start recording the browser page you drive (for a demo or tutorial video). \
Then do the task with the `browser` tool — every click, field and page load is logged with where and \
when — and call `record_stop`. Move deliberately: one action at a time, no aimless scrolling."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "goal": {"type": "string", "description": "What the video shows (its working title)"}
                },
                "required": ["goal"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let goal = call.input["goal"].as_str().unwrap_or("").trim();
        match video::record::start(&self.workspace_root, goal).await {
            Ok(id) => ok(
                call,
                format!("Recording {id}. Do the task with `browser` now, then call record_stop."),
            ),
            Err(e) => fail(call, e),
        }
    }
}

pub struct RecordStopTool;

#[async_trait]
impl ToolExecutor for RecordStopTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "record_stop".into(),
            description: "Stop the recording. Returns its event track (what happened when) and a \
draft edit list. Improve the draft — a clear title, one short friendly caption per step, cut or \
speed up anything boring, zoom only on the important clicks — then call `video_render` with it."
                .into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        match video::record::stop().await {
            Ok(s) => ok(
                call,
                format!(
                    "Recorded {id}: {dur:.1}s, {frames} frames.\n\nEvent track (seconds · action · where · what):\n{track}\n\n\
Draft edit list (times are recording seconds; segments with speed > 1 are sped up; zooms use page \
CSS pixels; music \"auto\" = generated background music, \"none\", or a file path):\n{edl}\n\n\
Rewrite the captions as short, friendly sentences, set the title/subtitle/end_text, adjust \
segments and zooms if needed, then call video_render with id \"{id}\" and the edl.",
                    id = s.id,
                    dur = s.duration,
                    frames = s.frames,
                    track = video::record::describe(&s.events),
                    edl = serde_json::to_string_pretty(&s.draft).unwrap_or_default(),
                ),
            ),
            Err(e) => fail(call, e),
        }
    }
}

pub struct VideoRenderTool {
    pub workspace_root: PathBuf,
}

#[async_trait]
impl ToolExecutor for VideoRenderTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "video_render".into(),
            description:
                "Render a recording with an edit list (from record_stop, or edited later) \
into an MP4 with background music (when ffmpeg is installed), a GIF and captions (.srt). Omit \
`edl` to re-render the saved one; omit `id` for the newest recording."
                    .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "edl": {"type": "object", "description": "title, subtitle, end_text, segments[{from,to,speed}], zooms[{at,x,y,scale,hold}], captions[{from,to,text}], music, cursor, width"}
                }
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let dir = match call.input["id"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(id) if !id.contains(['/', '\\']) && id != ".." => {
                video::videos_dir(&self.workspace_root).join(id)
            }
            Some(_) => return fail(call, "bad recording id"),
            None => match video::record::list(&self.workspace_root).first() {
                Some((_, d)) => d.clone(),
                None => return fail(call, "no recordings yet — record_start first"),
            },
        };
        if !dir.join("frames.json").is_file() {
            return fail(call, format!("no recording at {}", dir.display()));
        }
        if let Some(v) = call.input.get("edl").filter(|v| v.is_object()) {
            let edl: Edl = match serde_json::from_value(v.clone()) {
                Ok(e) => e,
                Err(e) => return fail(call, format!("edl: {e}")),
            };
            let text = serde_json::to_string_pretty(&edl).unwrap_or_default();
            if let Err(e) = std::fs::write(dir.join("edl.json"), text) {
                return fail(call, e.to_string());
            }
        }
        let d = dir.clone();
        match tokio::task::spawn_blocking(move || video::render::render(&d, |_| {})).await {
            Ok(Ok(r)) => {
                let rel = |p: &std::path::Path| {
                    p.strip_prefix(&self.workspace_root)
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| p.display().to_string())
                };
                let mut out = format!("Rendered {:.1}s.\n", r.seconds);
                if let Some(m) = &r.mp4 {
                    out.push_str(&format!("MP4: {}\n", rel(m)));
                }
                out.push_str(&format!(
                    "GIF: {}\nCaptions: {}\n",
                    rel(&r.gif),
                    rel(&r.srt)
                ));
                if let Some(n) = r.note {
                    out.push_str(&format!("Note: {n}\n"));
                }
                ok(call, out)
            }
            Ok(Err(e)) => fail(call, e),
            Err(e) => fail(call, e.to_string()),
        }
    }
}
