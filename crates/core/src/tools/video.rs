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
Then do the task — with one `browser_task` (a sub-agent drives the browser; cheaper, and its \
clicks are still logged) or the `browser` tool; every click, field and page load is logged with \
where and when — and call `record_stop`. Move deliberately: one action at a time, no aimless scrolling. \
After each action the recorder waits for the page to settle, and finds private things on it \
(emails, ticket ids, tokens, password fields, plus your `redact` words) to blur in the video. \
mode \"stills\" (vidni-style): one sharp settled screenshot per step with a slow camera move — \
best for dashboards and reports, and works even when the browser window is covered; \"live\" \
(default): a real recording — best for typing, menus and animations."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "goal": {"type": "string", "description": "What the video shows (its working title)"},
                    "mode": {"type": "string", "enum": ["live", "stills", "screen"], "description": "screen: the whole desktop (apps, the terminal) instead of the browser — then call record_note as you reach each step"},
                    "webcam": {"type": "boolean", "description": "Also record the webcam for a presenter bubble in the corner"},
                    "redo": {"type": "object", "properties": {"id": {"type": "string"}, "step": {"type": "integer"}}, "description": "Re-record step N of recording ID: first bring the browser to the state just before that step (browser replay of the earlier steps), then record_start with this, do only that step, record_stop — the new take replaces the old step"},
                    "redact": {"type": "array", "items": {"type": "string"}, "description": "Words to hide wherever they show: people's names, customer names…"},
                    "viewport": {"type": "string", "enum": ["standard", "window"], "description": "standard (default): the page is laid out at 1440×810 and recorded at 1920×1080, sharp and readable whatever the window size or zoom; window: record the window as it is"}
                },
                "required": ["goal"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let goal = call.input["goal"].as_str().unwrap_or("").trim();
        let opts = video::record::Options {
            mode: match call.input["mode"].as_str() {
                Some("stills") => video::record::Mode::Stills,
                Some("screen") => video::record::Mode::Screen,
                _ => video::record::Mode::Live,
            },
            webcam: call.input["webcam"].as_bool().unwrap_or(false),
            redo: match (
                call.input["redo"]["id"].as_str(),
                call.input["redo"]["step"].as_u64(),
            ) {
                (Some(id), Some(n)) => Some((id.to_string(), n as usize)),
                _ => None,
            },
            redact: call.input["redact"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            keep_viewport: call.input["viewport"].as_str() == Some("window"),
        };
        match video::record::start(&self.workspace_root, goal, opts).await {
            Ok(started) => ok(
                call,
                format!(
                    "Recording {}. Do the task now (one `browser_task`, or `browser`), then call record_stop.{}",
                    started.id,
                    if started.visible {
                        ""
                    } else {
                        " Note: the page reports itself hidden (its window is covered or in the \
background), so it is recorded by screenshots — about 4 a second, enough for a demo. For smooth \
video, ask the user to bring the browser window to the front."
                    }
                ),
            ),
            Err(e) => fail(call, e),
        }
    }
}

pub struct RecordPauseTool {
    pub resume: bool,
}

#[async_trait]
impl ToolExecutor for RecordPauseTool {
    fn definition(&self) -> ToolDefinition {
        if self.resume {
            ToolDefinition {
                name: "record_resume".into(),
                description: "Go on recording after record_pause.".into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            }
        } else {
            ToolDefinition {
                name: "record_pause".into(),
                description: "Pause the recording — for a sign-in, a long wait, setup the video \
shouldn't show. Nothing is recorded until record_resume, and the paused time is left out of the \
video."
                    .into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            }
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let r = if self.resume {
            video::record::resume()
        } else {
            video::record::pause()
        };
        match r {
            Ok(m) => ok(call, m),
            Err(e) => fail(call, e),
        }
    }
}

pub struct RecordNoteTool;

#[async_trait]
impl ToolExecutor for RecordNoteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "record_note".into(),
            description: "While recording (above all in screen mode, where there are no browser \
actions to log): mark that you reached a step, e.g. \"opened the settings window\". It goes on the \
event track, so narration can be timed to it."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"label": {"type": "string"}},
                "required": ["label"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        if !video::record::is_recording() {
            return fail(call, "not recording — record_start first");
        }
        let label = call.input["label"].as_str().unwrap_or("").trim();
        video::record::mark(label);
        match video::record::cap_warning() {
            Some(w) => ok(call, format!("noted: {label}\n{w}")),
            None => ok(call, format!("noted: {label}")),
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
draft edit list. Improve the draft — a clear title, a spoken narration line or two per step \
(`lines`, with sections for chapters), cut or speed up anything boring, zoom only on the \
important clicks — then call `video_render` with it."
                .into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        match video::record::stop().await {
            Ok(s) if s.spliced.is_some() => ok(
                call,
                format!(
                    "{} in {}. Its edit list kept the narration and moved later times; check the \
narration still fits the step, then video_render with id \"{}\".\n\nEvent track now:\n{}",
                    s.spliced.unwrap_or_default(),
                    s.id,
                    s.id,
                    video::record::describe(&s.events)
                ),
            ),
            Ok(s) => {
                let notes = if s.notes.is_empty() {
                    String::new()
                } else {
                    format!("\nNotes: {}.\n", s.notes.join("; "))
                };
                let script: Vec<String> = s
                    .draft
                    .lines
                    .iter()
                    .map(|l| format!("  {} @{:.1}s: {}", l.id, l.at, l.text))
                    .collect();
                ok(
                    call,
                    format!(
                        "Recorded {id}: {dur:.1}s, {frames} frames.{notes}\n\nEvent track (seconds · action · where · what):\n{track}\n\n\
A draft edit list is saved (.dcode-ai/videos/{id}/edl.json): {segs} segments (waiting sped up), \
{zooms} zooms on clicks, title \"{title}\", and this narration script (id @recording-second: text):\n{script}\n\n\
Now call video_render with id \"{id}\" and an `edl` holding ONLY the fields you change — they \
replace the saved draft's, the rest is kept. Usually: title (≤ 6 words), subtitle, end_text, and \
`lines` [{{id, at, text, section}}]: short, friendly spoken sentences, one or two per step, `at` \
about a second before the action, a `section` on each (chapters), ids short and stable (the \
user's own takes are filed under them). Optional: `respell` [{{text, say}}] for words the voice \
says wrong (\"UX\" → \"you ex\"); `inserts` [{{at, seconds, kind: section|bullets|quote|code|terminal, \
title, items}}] for cards; `cuts` [\"long\",\"short\"] for a 9:16 short too; `blur` \
[{{x,y,w,h,from,to}}] for anything private the automatic blur missed.",
                        id = s.id,
                        dur = s.duration,
                        frames = s.frames,
                        track = video::record::describe(&s.events),
                        segs = s.draft.segments.len(),
                        zooms = s.draft.zooms.len(),
                        title = s.draft.title,
                        script = script.join("\n"),
                    ),
                )
            }
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
                "Render a recording with an edit list (from record_stop, or edited later): \
narration (the user's takes, else the draft voice) over background music, captions (.srt/.vtt), \
cards, chapters and thumbnails, as an MP4 (16:9, plus a 9:16 short when cuts has \"short\"; \
needs ffmpeg) and a GIF. Omit `edl` to re-render the saved one; omit `id` for the newest \
recording. Never touch the files in `voice/` (the user's takes) or `approved.json`."
                    .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "preview": {"type": "boolean", "description": "A quick low-resolution draft (preview.mp4) to check timing"},
                    "lang": {"type": "string", "description": "Render a translation from edl.translations.<lang> (e.g. \"de\")"},
                    "check": {"type": "boolean", "description": "Only report what the render would do and what looks off (length, narration longer than its footage, lines or zooms in cut parts, merged zooms, anchors not found) — no rendering"},
                    "edl": {"type": "object", "description": "title, subtitle, end_text, segments[{from,to,speed}], zooms[{at,x,y,scale,hold}], lines[{id,at,text,section,only}] (narration), inserts[{at,seconds,kind,title,items}], cuts[\"long\",\"short\"], voice (auto|draft|takes|none), draft_voice, music, captions[{from,to,text}] (used only without lines), cursor, width, clean_master, platforms"}
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
            // The fields given replace the saved edit list's; the rest stay.
            let mut merged: serde_json::Value = std::fs::read_to_string(dir.join("edl.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .filter(|v: &serde_json::Value| v.is_object())
                .unwrap_or_else(|| serde_json::json!({}));
            if let (Some(m), Some(new)) = (merged.as_object_mut(), v.as_object()) {
                for (k, val) in new {
                    m.insert(k.clone(), val.clone());
                }
            }
            let edl: Edl = match serde_json::from_value(merged) {
                Ok(e) => e,
                Err(e) => return fail(call, format!("edl: {e}")),
            };
            let text = serde_json::to_string_pretty(&edl).unwrap_or_default();
            if let Err(e) = std::fs::write(dir.join("edl.json"), text) {
                return fail(call, e.to_string());
            }
        }
        if call.input["check"].as_bool() == Some(true) {
            return match video::render::check(&dir) {
                Ok(lines) => ok(
                    call,
                    format!("Edit report (nothing rendered):\n{}", lines.join("\n")),
                ),
                Err(e) => fail(call, e),
            };
        }
        let d = dir.clone();
        let opts = video::render::Options {
            preview: call.input["preview"].as_bool().unwrap_or(false),
            lang: call.input["lang"]
                .as_str()
                .map(|l| l.trim().to_ascii_lowercase())
                .filter(|l| {
                    !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                }),
        };
        match tokio::task::spawn_blocking(move || video::render::render_with(&d, &opts, |_| {}))
            .await
        {
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
                let names: Vec<String> = r
                    .files
                    .iter()
                    .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .collect();
                if !names.is_empty() {
                    out.push_str(&format!("All files: {}\n", names.join(", ")));
                }
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
