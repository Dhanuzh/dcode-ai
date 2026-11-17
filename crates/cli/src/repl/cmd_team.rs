//! Seeing and sharing a session: `/timeline`, `/files`, `/sounds`,
//! `/speak`, `/watch`, `/handoff`.

use super::*;

/// `/standup`: turn what you said (or typed) about your day into board
/// cards, Jira comments and a stand-up note.
const STANDUP_PROMPT: &str = "Here is my stand-up, spoken or typed — rough notes about what I did, what I'm doing next and what blocks me. Turn it into action:
1. Write a short stand-up note: **Yesterday** · **Today** · **Blockers** (bullets, my words tidied, nothing invented).
2. Look at the task board (`board_view`). For work I mentioned that matches a card: update it (`board_update` — status, a note). For new work I said I'll do: add cards (`board_add`) in To do.
3. For Jira tickets I named (or cards linked to Jira) where a comment would help the team, draft the comments — then ask me once (`ask_question`) which to post before posting any.
4. End with what you changed and the stand-up note to paste.

What I said: ";

/// Who is handing a session over: git's user name, else the login name.
fn handoff_author(workspace: &std::path::Path) -> String {
    dcode_ai_cli::people::my_name(workspace)
}

/// Working-tree changes (paths), for the handoff note.
fn changed_files(workspace: &std::path::Path) -> Vec<String> {
    std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.get(3..).map(str::to_string))
                .filter(|p| !p.starts_with(".dcode-ai/"))
                .collect()
        })
        .unwrap_or_default()
}

/// Set while `/talk` is active; dropped to restore normal Esc handling.
pub(super) struct TalkStopGuard {
    state: Option<Arc<Mutex<TuiSessionState>>>,
}

fn run_talk_stop_guard_with_state(
    state: Option<Arc<Mutex<TuiSessionState>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> TalkStopGuard {
    if let Some(state) = state {
        if let Ok(mut g) = state.lock() {
            g.talk_stop = Some(stop);
        }
        TalkStopGuard { state: Some(state) }
    } else {
        TalkStopGuard { state: None }
    }
}

#[cfg(test)]
fn talk_stop_guard_for_tui(
    state: Arc<Mutex<TuiSessionState>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> TalkStopGuard {
    run_talk_stop_guard_with_state(Some(state), stop)
}

impl TalkStopGuard {
    pub(super) fn install(out: ReplOutput<'_>, stop: Arc<std::sync::atomic::AtomicBool>) -> Self {
        let state = match out {
            ReplOutput::Tui(st) => Some(Arc::clone(st)),
            ReplOutput::Stdio => None,
        };
        run_talk_stop_guard_with_state(state, stop)
    }
}

impl Drop for TalkStopGuard {
    fn drop(&mut self) {
        if let Some(state) = &self.state
            && let Ok(mut g) = state.lock()
        {
            g.talk_stop = None;
            g.talk_ptt = None;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum TalkMode {
    /// What you say goes into the prompt.
    Prompt,
    /// …and is sent.
    Send,
    /// Hands-free: listen, send, read the reply aloud, listen again.
    Loop,
    /// `/standup voice`.
    Standup,
    /// Microphone check.
    Test,
    /// Push-to-talk: no pause detection; Enter ends the take and sends it.
    Ptt,
    /// Waits for "hey dcode", then takes the request (hands-free).
    Wake,
}

fn talk_say(st: &Arc<Mutex<TuiSessionState>>, line: String) {
    if let Ok(mut g) = st.lock() {
        g.push_block(crate::tui::state::DisplayBlock::System(line));
        g.touch_transcript();
    }
}

/// A status line rewritten in place (listening… / transcribing… / level).
fn talk_live(st: &Arc<Mutex<TuiSessionState>>, line: String) {
    if let Ok(mut g) = st.lock() {
        let live = |b: &crate::tui::state::DisplayBlock| {
            matches!(b, crate::tui::state::DisplayBlock::System(t)
                if t.starts_with("[talk] 🎙") || t.starts_with("[talk] ✎"))
        };
        if g.blocks.last().is_some_and(live) {
            g.blocks.pop();
        }
        g.push_block(crate::tui::state::DisplayBlock::System(line));
        g.touch_transcript();
    }
}

/// Waits for the agent to take and finish the turn just sent. False when
/// Esc stopped it.
async fn talk_wait_for_reply(
    st: &Arc<Mutex<TuiSessionState>>,
    stop: &std::sync::atomic::AtomicBool,
) -> bool {
    use std::sync::atomic::Ordering;
    let busy = || st.lock().is_ok_and(|g| g.busy);
    let started = std::time::Instant::now();
    while !busy() && started.elapsed() < std::time::Duration::from_secs(8) {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    while busy() {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    !stop.load(Ordering::SeqCst)
}

/// `/talk` in the TUI, in the background. Esc is the one key that stops it.
async fn talk_session(
    st: Arc<Mutex<TuiSessionState>>,
    tx: tokio::sync::mpsc::UnboundedSender<TuiCmd>,
    model: String,
    mode: TalkMode,
) {
    use dcode_ai_cli::voice::stt;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _armed = run_talk_stop_guard_with_state(Some(Arc::clone(&st)), stop.clone());
    if mode == TalkMode::Test {
        talk_mic_test(&st, &model, &stop).await;
        return;
    }
    if mode == TalkMode::Wake {
        talk_wake(&st, &tx, &model, &stop).await;
        return;
    }
    let (max, quiet) = if mode == TalkMode::Standup {
        (
            std::time::Duration::from_secs(180),
            std::time::Duration::from_secs(3),
        )
    } else if mode == TalkMode::Ptt {
        // Pauses don't end it: only Enter (send) or Esc (to the prompt).
        if let Ok(mut g) = st.lock() {
            g.talk_ptt = Some(false);
        }
        talk_say(
            &st,
            "[talk] push-to-talk — speak, then Enter sends it (Esc puts it in the prompt instead)"
                .into(),
        );
        (
            std::time::Duration::from_secs(180),
            std::time::Duration::from_secs(180),
        )
    } else {
        (
            std::time::Duration::from_secs(90),
            std::time::Duration::from_millis(1500),
        )
    };
    let progress = {
        let st = Arc::clone(&st);
        move |s: String| talk_live(&st, format!("[talk] {s}"))
    };
    if mode == TalkMode::Loop {
        talk_say(
            &st,
            "[talk] hands-free — say \"stop\" or press Esc to end (Esc also cuts off a reply being read)".into(),
        );
    }
    for _ in 0..30 {
        let heard = match stt::listen(&model, max, quiet, stop.clone(), &progress).await {
            Ok(h) => h,
            Err(e) => {
                talk_say(&st, format!("[talk] {e}"));
                break;
            }
        };
        let text = heard.text;
        let to_prompt = |text: &str| {
            if let Ok(mut g) = st.lock() {
                g.set_input_text(text.to_string());
            }
        };
        if mode != TalkMode::Loop || heard.by_esc {
            if text.is_empty() {
                talk_say(
                    &st,
                    if heard.by_esc {
                        "[talk] stopped".into()
                    } else {
                        "[talk] didn't catch that — try again".into()
                    },
                );
            } else {
                let sent_by_enter =
                    mode == TalkMode::Ptt && st.lock().is_ok_and(|g| g.talk_ptt == Some(true));
                match mode {
                    TalkMode::Ptt if sent_by_enter => {
                        talk_say(&st, format!("[talk] you: {text}"));
                        let _ = tx.send(TuiCmd::Submit(text));
                    }
                    TalkMode::Send => {
                        talk_say(&st, format!("[talk] you: {text}"));
                        let _ = tx.send(TuiCmd::Submit(text));
                    }
                    TalkMode::Standup => {
                        talk_say(&st, format!("[talk] you said: {text}"));
                        let _ = tx.send(TuiCmd::Submit(format!("/standup {text}")));
                    }
                    _ => {
                        to_prompt(&text);
                        talk_say(
                            &st,
                            "[talk] in the prompt — edit it or press Enter to send".into(),
                        );
                    }
                }
            }
            break;
        }
        if text.is_empty() {
            continue;
        }
        if stt::is_stop(&text) {
            talk_say(&st, "[talk] ended".into());
            break;
        }
        talk_say(&st, format!("[talk] you: {text}"));
        let _ = tx.send(TuiCmd::Submit(spoken_request(
            &text,
            heard.language.as_deref(),
        )));
        if !talk_wait_for_reply(&st, &stop).await {
            talk_say(&st, "[talk] stopped".into());
            break;
        }
        let reply = st
            .lock()
            .ok()
            .and_then(|g| {
                g.blocks.iter().rev().find_map(|b| match b {
                    crate::tui::state::DisplayBlock::Assistant(t) => Some(t.clone()),
                    _ => None,
                })
            })
            .unwrap_or_default();
        let s2 = stop.clone();
        let spoken =
            tokio::task::spawn_blocking(move || crate::tui::notify::speak_until(&reply, &s2)).await;
        if matches!(spoken, Ok(Ok(true))) {
            talk_say(&st, "[talk] stopped".into());
            break;
        }
    }
}

/// The voice persona (`/talk persona`): how spoken replies sound.
fn voice_persona() -> String {
    dcode_ai_common::config::dcode_ai_home_dir()
        .and_then(|d| std::fs::read_to_string(d.join("voice-persona")).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// (tone asked of the agent, speech speed) for a persona.
fn persona_style(p: &str) -> Option<(&'static str, i32)> {
    match p {
        "calm" => Some(("calmly and warmly, in plain, unhurried sentences", -1)),
        "brief" => Some(("in one or two short sentences, no lists", 1)),
        "cheerful" => Some(("in an upbeat, friendly way", 1)),
        _ => None,
    }
}

/// What is sent for something said in a spoken conversation: the words,
/// plus how to answer — in the language it was said in, in the persona's
/// tone. Also sets the voice that reads the answer.
fn spoken_request(text: &str, language: Option<&str>) -> String {
    use dcode_ai_cli::voice::stt;
    let lang = language.filter(|l| *l != "en");
    let persona = voice_persona();
    let style = persona_style(&persona);
    crate::tui::notify::set_speech(crate::tui::notify::Speech {
        lang: lang.map(str::to_string),
        rate: style.map(|s| s.1).unwrap_or(0),
    });
    let mut asks = Vec::new();
    if let Some(l) = lang {
        asks.push(format!("answer in {}", stt::language_name(l)));
    }
    if let Some((tone, _)) = style {
        asks.push(format!("it will be read aloud: answer {tone}"));
    }
    if asks.is_empty() {
        text.to_string()
    } else {
        format!("{text}\n\n({}.)", asks.join("; "))
    }
}

/// The wake phrase in what was heard: what came after it (maybe empty), or
/// `None` when it wasn't said. Whisper writes "hey dcode" many ways.
fn after_wake_word(heard: &str) -> Option<String> {
    let lower = heard.to_lowercase();
    let words: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    let joined = words.join(" ");
    for wake in [
        "hey dcode",
        "hey d code",
        "hey decode",
        "hey dee code",
        "hey the code",
        "hey decoder",
        "hi dcode",
        "hi decode",
        "okay dcode",
        "ok dcode",
        "a decode",
        "hey d c o d e",
    ] {
        if let Some(pos) = joined.find(wake) {
            let rest = joined[pos + wake.len()..].trim();
            // Back to the original casing and punctuation for the request.
            let n = rest.split_whitespace().count();
            let original: Vec<&str> = heard.split_whitespace().collect();
            let tail = original[original.len().saturating_sub(n)..].join(" ");
            return Some(
                tail.trim_matches(|c: char| c == ',' || c == '.')
                    .trim()
                    .to_string(),
            );
        }
    }
    None
}

/// `/talk wake`: listens in short stretches for "hey dcode"; then takes the
/// request (what followed, or the next thing said), answers aloud, and
/// waits again. Esc stops.
async fn talk_wake(
    st: &Arc<Mutex<TuiSessionState>>,
    tx: &tokio::sync::mpsc::UnboundedSender<TuiCmd>,
    model: &str,
    stop: &Arc<std::sync::atomic::AtomicBool>,
) {
    use dcode_ai_cli::voice::stt;
    use std::sync::atomic::Ordering;
    talk_say(
        st,
        "[talk] 👂 say \"hey dcode\" and then what you need — Esc stops listening".into(),
    );
    let quiet = |_: String| {};
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let heard = match stt::listen(
            model,
            std::time::Duration::from_secs(8),
            std::time::Duration::from_millis(900),
            stop.clone(),
            &quiet,
        )
        .await
        {
            Ok(h) => h,
            Err(e) if e == "stopped" => break,
            Err(e) => {
                talk_say(st, format!("[talk] {e}"));
                break;
            }
        };
        if heard.by_esc {
            break;
        }
        let Some(mut request) = after_wake_word(&heard.text) else {
            continue;
        };
        let mut language = heard.language.clone();
        if request.is_empty() {
            let _ = crate::tui::notify::speak("Yes?");
            talk_say(st, "[talk] 🎙 yes? (listening)".into());
            match stt::listen(
                model,
                std::time::Duration::from_secs(60),
                std::time::Duration::from_millis(1500),
                stop.clone(),
                &quiet,
            )
            .await
            {
                Ok(h) if !h.by_esc && !h.text.is_empty() => {
                    request = h.text;
                    language = h.language;
                }
                Ok(h) if h.by_esc => break,
                _ => continue,
            }
        }
        if stt::is_stop(&request) {
            break;
        }
        talk_say(st, format!("[talk] you: {request}"));
        let _ = tx.send(TuiCmd::Submit(spoken_request(
            &request,
            language.as_deref(),
        )));
        if !talk_wait_for_reply(st, stop).await {
            break;
        }
        let reply = st
            .lock()
            .ok()
            .and_then(|g| {
                g.blocks.iter().rev().find_map(|b| match b {
                    crate::tui::state::DisplayBlock::Assistant(t) => Some(t.clone()),
                    _ => None,
                })
            })
            .unwrap_or_default();
        let s2 = stop.clone();
        let cut =
            tokio::task::spawn_blocking(move || crate::tui::notify::speak_until(&reply, &s2)).await;
        if matches!(cut, Ok(Ok(true))) {
            break;
        }
        talk_say(st, "[talk] 👂 waiting for \"hey dcode\"…".into());
    }
    talk_say(st, "[talk] stopped listening for \"hey dcode\"".into());
}

/// `/talk test`: listens for a few seconds and shows the level live, then
/// says what's wrong (no audio, too quiet, noisy) and what it heard.
async fn talk_mic_test(
    st: &Arc<Mutex<TuiSessionState>>,
    model: &str,
    stop: &Arc<std::sync::atomic::AtomicBool>,
) {
    use dcode_ai_cli::voice::{audio, mixer, stt, vad};
    use std::sync::atomic::Ordering;
    talk_say(
        st,
        "[talk] microphone test — say something for a few seconds (Esc stops)".into(),
    );
    let (atx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
    let io = match audio::start(atx, Arc::new(Mutex::new(mixer::Mixer::default()))) {
        Ok(io) => io,
        Err(e) => {
            talk_say(st, format!("[talk] ✗ can't open the microphone: {e}"));
            return;
        }
    };
    let backend = io.backend();
    let mut gate = vad::Gate::default();
    let mut levels: Vec<f32> = Vec::new();
    let mut speech = 0usize;
    let mut pcm: Vec<i16> = Vec::new();
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(6) && !stop.load(Ordering::SeqCst) {
        let frame =
            match tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(_) => continue,
            };
        let level = vad::rms(&frame);
        if gate.open(&frame) {
            speech += 1;
        }
        levels.push(level);
        pcm.extend_from_slice(&frame);
        if levels.len().is_multiple_of(5) {
            let n = (((level.max(1.0).log10() - 2.0) / 2.5) * 20.0).clamp(0.0, 20.0) as usize;
            talk_live(
                st,
                format!(
                    "[talk] 🎙 {}{} {:>5.0}{}",
                    "█".repeat(n),
                    "·".repeat(20 - n),
                    level,
                    if speech > 0 { " · speech heard" } else { "" }
                ),
            );
        }
    }
    drop(io);
    if levels.is_empty() {
        talk_say(
            st,
            format!(
                "[talk] ✗ no sound arrived from the microphone ({backend}). On WSL: Windows 11 with WSLg, and the microphone allowed for desktop apps in Windows privacy settings."
            ),
        );
        return;
    }
    let mut sorted = levels.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let floor = sorted[sorted.len() / 5];
    let peak = sorted[sorted.len() - 1];
    talk_say(
        st,
        format!(
            "[talk] {backend} · {:.1}s · background {floor:.0} · loudest {peak:.0} · speech {:.1}s",
            levels.len() as f32 * 0.02,
            speech as f32 * 0.02
        ),
    );
    if peak < 300.0 {
        talk_say(
            st,
            "[talk] ✗ almost silent — the microphone may be muted or the wrong input is selected"
                .into(),
        );
        return;
    }
    if floor > 900.0 {
        talk_say(
            st,
            "[talk] ⚠ noisy background — pauses may be missed; press Esc when you're done talking"
                .into(),
        );
    }
    if speech == 0 {
        talk_say(
            st,
            "[talk] ⚠ no speech detected — speak louder or closer".into(),
        );
        return;
    }
    stop.store(false, Ordering::SeqCst);
    talk_live(st, "[talk] ✎ transcribing…".into());
    match stt::transcribe_pcm(model, pcm, stop.clone()).await {
        Ok(t) if !t.is_empty() => talk_say(st, format!("[talk] ✓ heard: “{t}” — /talk is ready")),
        Ok(_) => talk_say(st, "[talk] ⚠ heard sound but no words".into()),
        Err(e) => talk_say(st, format!("[talk] ✗ {e}")),
    }
}

impl Repl {
    #[allow(unused_variables, unused_mut)]
    pub(super) async fn cmd_team(
        &mut self,
        input: &str,
        command: &str,
        rest: &str,
        mut parts: std::str::SplitWhitespace<'_>,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<bool> {
        match command {
            "/office" => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.office_open = true;
                } else {
                    out.println("[office] the office needs the full-screen terminal UI");
                }
            }
            "/diagram" => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.diagram_open = true;
                } else {
                    out.println("[diagram] the diagram viewer needs the full-screen terminal UI");
                }
            }
            "/timeline" => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.timeline_open = true;
                } else {
                    out.println("[timeline] the timeline needs the full-screen terminal UI");
                }
            }
            "/files" => {
                let lines: Vec<String> = match &out {
                    ReplOutput::Tui(st) => st
                        .lock()
                        .map(|g| {
                            let map = &g.file_map;
                            let mut lines = vec![
                                format!(
                                    "[files] {} touched · {} edited (dashboard: Ctrl+\\ then Alt+4)",
                                    map.files.len(),
                                    map.edited()
                                ),
                                "[files] · read (2r = twice) · ✎ edited · time = last touched · \"listed\" = the agent listed that folder".into(),
                            ];
                            for (depth, name, touch) in map.tree() {
                                let indent = "  ".repeat(depth + 1);
                                lines.push(match touch {
                                    None => format!("{indent}{name}"),
                                    Some(t) if t.edits > 0 => format!(
                                        "{indent}✎ {name}  {} edit{} · {}",
                                        t.edits,
                                        if t.edits == 1 { "" } else { "s" },
                                        crate::tui::activity::ago(t.last_at)
                                    ),
                                    Some(t) => format!(
                                        "{indent}· {name}  read {}× · {}",
                                        t.reads,
                                        crate::tui::activity::ago(t.last_at)
                                    ),
                                });
                            }
                            lines
                        })
                        .unwrap_or_default(),
                    _ => vec!["[files] needs the terminal UI".into()],
                };
                for l in lines {
                    out.println(&l);
                }
            }
            "/sounds" => {
                let on = match rest.trim() {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                };
                let Some(on) = on else {
                    let now = self.runtime.config().ui.sounds;
                    out.println(&format!(
                        "[sounds] {} — /sounds on|off (a bell when a long turn finishes or dcode-ai needs you)",
                        if now { "on" } else { "off" }
                    ));
                    return Ok(true);
                };
                self.runtime.config_mut().ui.sounds = on;
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.sounds = on;
                }
                let path = dcode_ai_common::config::global_config_file();
                if let Some(path) = path {
                    let _ = dcode_ai_common::config::set_toml_value(
                        &path,
                        "ui",
                        "sounds",
                        toml::Value::Boolean(on),
                    );
                }
                out.println(&format!(
                    "[sounds] {} (saved to your config)",
                    if on { "on" } else { "off" }
                ));
            }
            "/slack" => match rest.trim() {
                "off" | "stop" => {
                    if let Some(h) = self.slack.take() {
                        h.abort();
                        out.println("[slack] stopped answering");
                    } else {
                        out.println("[slack] not running");
                    }
                }
                "on" | "" => {
                    if self.slack.as_ref().is_some_and(|h| !h.is_finished()) {
                        out.println("[slack] already answering — /slack off stops it");
                        return Ok(true);
                    }
                    let Some(cfg) = dcode_ai_cli::slack::SlackConfig::load() else {
                        out.println(
                            "[slack] not set up — run `dcode-ai slack setup` in a terminal first",
                        );
                        return Ok(true);
                    };
                    let report: dcode_ai_cli::slack::Report = match &out {
                        ReplOutput::Tui(st) => {
                            let st = (*st).clone();
                            std::sync::Arc::new(move |s: String| {
                                if let Ok(mut g) = st.lock() {
                                    g.push_block(crate::tui::state::DisplayBlock::System(format!(
                                        "[slack] {s}"
                                    )));
                                    g.touch_transcript();
                                }
                            })
                        }
                        ReplOutput::Stdio => {
                            std::sync::Arc::new(|s: String| eprintln!("[slack] {s}"))
                        }
                    };
                    let ws = self.runtime.workspace_root().to_path_buf();
                    let r2 = report.clone();
                    self.slack = Some(tokio::spawn(async move {
                        if let Err(e) = dcode_ai_cli::slack::serve(cfg, ws, r2.clone()).await {
                            r2(format!("stopped: {e}"));
                        }
                    }));
                    out.println("[slack] answering @dcode-ai mentions about this project (read-only) for the people you allowed · /slack off stops");
                }
                _ => out.println("usage: /slack on|off — set up once with `dcode-ai slack setup`"),
            },
            "/detach" => {
                if std::env::var_os(dcode_ai_cli::host::HOSTED_ENV).is_some() {
                    use std::io::Write;
                    let mut o = std::io::stdout();
                    let _ = o.write_all(dcode_ai_cli::host::DETACH_MARKER);
                    let _ = o.flush();
                } else {
                    out.println("[detach] this session isn't running in the background, so closing the terminal ends it — `[session] keep_running = true` (Linux/macOS) makes new sessions outlive the terminal");
                }
            }
            "/teams" => match rest.trim() {
                "off" | "stop" => {
                    if let Some(h) = self.teams.take() {
                        h.abort();
                        out.println("[teams] stopped answering");
                    } else {
                        out.println("[teams] not running");
                    }
                }
                "on" | "" => {
                    if self.teams.as_ref().is_some_and(|h| !h.is_finished()) {
                        out.println("[teams] already answering — /teams off stops it");
                        return Ok(true);
                    }
                    let Some(cfg) = dcode_ai_cli::teams::TeamsConfig::load() else {
                        out.println(
                            "[teams] not set up — run `dcode-ai teams setup` in a terminal first",
                        );
                        return Ok(true);
                    };
                    let report: dcode_ai_cli::slack::Report = match &out {
                        ReplOutput::Tui(st) => {
                            let st = (*st).clone();
                            std::sync::Arc::new(move |s: String| {
                                if let Ok(mut g) = st.lock() {
                                    g.push_block(crate::tui::state::DisplayBlock::System(format!(
                                        "[teams] {s}"
                                    )));
                                    g.touch_transcript();
                                }
                            })
                        }
                        ReplOutput::Stdio => {
                            std::sync::Arc::new(|s: String| eprintln!("[teams] {s}"))
                        }
                    };
                    let ws = self.runtime.workspace_root().to_path_buf();
                    let r2 = report.clone();
                    self.teams = Some(tokio::spawn(async move {
                        if let Err(e) = dcode_ai_cli::teams::serve(cfg, ws, r2.clone()).await {
                            r2(format!("stopped: {e}"));
                        }
                    }));
                    out.println("[teams] answering @dcode-ai mentions about this project (read-only) for the people you allowed — keep your tunnel running · /teams off stops");
                }
                _ => out.println("usage: /teams on|off — set up once with `dcode-ai teams setup`"),
            },
            "/talk" | "/standup" => {
                let mut model = self
                    .runtime
                    .config()
                    .ui
                    .stt_model
                    .clone()
                    .unwrap_or_else(|| "tiny.en".into());
                let sub = rest.trim();
                if command == "/talk" && (sub == "persona" || sub.starts_with("persona ")) {
                    let want = sub.trim_start_matches("persona").trim();
                    let file = dcode_ai_common::config::dcode_ai_home_dir()
                        .map(|d| d.join("voice-persona"));
                    match (want, file) {
                        ("", _) => {
                            let p = voice_persona();
                            out.println(&format!(
                                "[talk] voice persona: {} — /talk persona calm|brief|cheerful|off",
                                if p.is_empty() {
                                    "none (plain)"
                                } else {
                                    p.as_str()
                                }
                            ));
                        }
                        ("off" | "none", Some(f)) => {
                            let _ = std::fs::remove_file(f);
                            out.println("[talk] voice persona off");
                        }
                        (p, Some(f)) if persona_style(p).is_some() => {
                            let _ = std::fs::create_dir_all(
                                f.parent().unwrap_or(std::path::Path::new(".")),
                            );
                            match std::fs::write(&f, p) {
                                Ok(()) => out.println(&format!(
                                    "[talk] spoken replies will be {p} from now on"
                                )),
                                Err(e) => out.eprintln(&format!("[talk] {e}")),
                            }
                        }
                        _ => out.println("usage: /talk persona calm|brief|cheerful|off"),
                    }
                    return Ok(true);
                }
                // Any language: the multilingual model, answers in the
                // language you spoke.
                if command == "/talk" && sub == "lang" && model.ends_with(".en") {
                    model = model.trim_end_matches(".en").to_string();
                    if model != "tiny" && model != "base" {
                        model = "tiny".into();
                    }
                    if model == "base" {
                        model = "tiny".into();
                    }
                }
                if command == "/standup" && sub != "voice" {
                    if sub.is_empty() {
                        out.println(
                            "usage: /standup voice (talk through your day) · /standup <your notes>",
                        );
                    } else {
                        self.run_preset(STANDUP_PROMPT, sub, out).await?;
                    }
                    return Ok(true);
                }
                // Listening/transcribing status rewrites one line in place
                // (instead of a new line every second).
                // In the TUI, talking runs in the background: dcode-ai keeps
                // working, and Esc stops it (listening, the spoken reply, the
                // hands-free loop).
                if let ReplOutput::Tui(st) = out
                    && let Some(tx) = self.self_cmd_tx.clone()
                {
                    if st.lock().is_ok_and(|g| g.talk_stop.is_some()) {
                        out.println("[talk] already on — Esc stops it");
                        return Ok(true);
                    }
                    let mode = match (command, sub) {
                        ("/standup", _) => TalkMode::Standup,
                        (_, "loop") => TalkMode::Loop,
                        (_, "send") => TalkMode::Send,
                        (_, "test") => TalkMode::Test,
                        (_, "ptt") => TalkMode::Ptt,
                        (_, "wake") => TalkMode::Wake,
                        (_, "lang") => TalkMode::Loop,
                        _ => TalkMode::Prompt,
                    };
                    tokio::spawn(talk_session(Arc::clone(st), tx, model, mode));
                    return Ok(true);
                }
                let progress = |s: String| {
                    let line = format!("[talk] {s}");
                    if let ReplOutput::Tui(st) = &out
                        && let Ok(mut g) = st.lock()
                    {
                        let live = |b: &DisplayBlock| {
                            matches!(b, DisplayBlock::System(t)
                                if t.starts_with("[talk] 🎙 listening")
                                    || t.starts_with("[talk] ✎ transcribing"))
                        };
                        if g.blocks.last().is_some_and(live) {
                            g.blocks.pop();
                        }
                        g.blocks.push(DisplayBlock::System(line));
                        g.touch_transcript();
                        return;
                    }
                    out.println(&line);
                };
                // Esc in the TUI stops listening (and a hands-free loop).
                let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

                let talk_run = async {
                    let (max, quiet) = if command == "/standup" {
                        (
                            std::time::Duration::from_secs(180),
                            std::time::Duration::from_secs(3),
                        )
                    } else {
                        (
                            std::time::Duration::from_secs(90),
                            std::time::Duration::from_millis(1500),
                        )
                    };
                    // Esc belongs to `/talk` only while it listens or
                    // transcribes; during the agent's reply it works as usual.
                    let listen = || async {
                        stop.store(false, std::sync::atomic::Ordering::SeqCst);
                        let _armed = TalkStopGuard::install(out, stop.clone());
                        dcode_ai_cli::voice::stt::listen(&model, max, quiet, stop.clone(), progress)
                            .await
                    };
                    if command == "/standup" {
                        match listen().await.map(|h| h.text) {
                            Ok(t) if !t.is_empty() => {
                                out.println(&format!("[talk] you said: {t}"));
                                self.run_preset(STANDUP_PROMPT, &t, out).await?;
                            }
                            Ok(_) => out.println("[talk] didn't catch that — try again"),
                            Err(e) => out.eprintln(&format!("[talk] {e}")),
                        }
                        return Ok::<(), anyhow::Error>(());
                    }
                    match sub {
                        // Hands-free: listen → send → read the reply → listen.
                        "loop" => {
                            out.println("[talk] hands-free — say \"stop\" (or press Esc while silent) to end");
                            for _ in 0..30 {
                                let (text, esc) = match listen().await {
                                    Ok(h) => (h.text, h.by_esc),
                                    Err(e) => {
                                        out.eprintln(&format!("[talk] {e}"));
                                        break;
                                    }
                                };
                                if text.is_empty() {
                                    if esc {
                                        out.println("[talk] ended");
                                        break;
                                    }
                                    continue;
                                }
                                if dcode_ai_cli::voice::stt::is_stop(&text) {
                                    out.println("[talk] ended");
                                    break;
                                }
                                out.println(&format!("[talk] you: {text}"));
                                self.run_preset("", &text, out).await?;
                                let reply = self
                                    .runtime
                                    .messages()
                                    .iter()
                                    .rev()
                                    .find(|m| m.role == dcode_ai_common::message::Role::Assistant)
                                    .map(|m| m.content.to_summary_text())
                                    .unwrap_or_default();
                                let _ = tokio::task::spawn_blocking(move || {
                                    crate::tui::notify::speak_and_wait(&reply)
                                })
                                .await;
                            }
                        }
                        _ => match listen().await.map(|h| h.text) {
                            Ok(t) if t.is_empty() => {
                                out.println("[talk] didn't catch that — try again")
                            }
                            Ok(t) if sub == "send" => {
                                out.println(&format!("[talk] you: {t}"));
                                self.run_preset("", &t, out).await?;
                            }
                            Ok(t) => {
                                if let ReplOutput::Tui(st) = &out
                                    && let Ok(mut g) = st.lock()
                                {
                                    g.set_input_text(t.clone());
                                    out.println(
                                        "[talk] in the prompt — edit it or press Enter to send",
                                    );
                                } else {
                                    out.println(&format!("[talk] {t}"));
                                }
                            }
                            Err(e) => out.eprintln(&format!("[talk] {e}")),
                        },
                    }
                    Ok::<(), anyhow::Error>(())
                }
                .await;

                talk_run?;
            }
            "/speak" => {
                let last = self
                    .runtime
                    .messages()
                    .iter()
                    .rev()
                    .find(|m| m.role == dcode_ai_common::message::Role::Assistant)
                    .map(|m| m.content.to_summary_text())
                    .unwrap_or_default();
                match crate::tui::notify::speak(&last) {
                    Ok(()) => out.println("[speak] reading the last reply aloud…"),
                    Err(e) => out.eprintln(&format!("[speak] {e}")),
                }
            }
            "/watch" => {
                let arg = rest.trim();
                if arg == "stop" {
                    if self.watch.take().is_some() {
                        out.println("[watch] stopped — the link no longer works");
                    } else {
                        out.println("[watch] no watch link is running");
                    }
                    return Ok(true);
                }
                // `/watch`, `/watch lan`, `/watch lan 8787`, `/watch 8787`.
                let words: Vec<&str> = arg.split_whitespace().collect();
                let lan = words.contains(&"lan");
                let port: u16 = words.iter().find_map(|w| w.parse().ok()).unwrap_or(0);
                if let Some(w) = &self.watch
                    && w.lan == lan
                    && (port == 0 || w.port == port)
                {
                    out.println(&format!("[watch] already live: {}", w.url));
                    return Ok(true);
                }
                // A new one replaces the old.
                self.watch = None;
                let title = self
                    .runtime
                    .session_name()
                    .map(str::to_string)
                    .unwrap_or_else(|| self.runtime.session_id().to_string());
                match crate::watch::start_on(
                    self.runtime.event_log_path(),
                    title,
                    lan,
                    port,
                    self.runtime.event_tx(),
                )
                .await
                {
                    Ok(w) => {
                        let _ = crate::tui::clipboard::copy_to_clipboard(&w.url);
                        out.println(&format!("[watch] live, read-only: {}", w.url));
                        if lan {
                            out.println(
                                "[watch] reachable by anyone on this network who has the link — /watch stop ends it",
                            );
                            print_qr(&out, "[watch] or scan it with a phone:", &w.url);
                            if let Some(help) = crate::watch::lan_help(w.port) {
                                for line in help.lines() {
                                    out.println(&format!("[watch] {line}"));
                                }
                                if port == 0 {
                                    out.println(
                                        "[watch] tip: /watch lan 8787 keeps the same port, so the rule is made once",
                                    );
                                }
                            }
                        } else {
                            out.println(
                                "[watch] this machine only (copied) · /watch lan to share on your network · /watch stop",
                            );
                        }
                        self.watch = Some(w);
                    }
                    Err(e) => out.eprintln(&format!("[watch] could not start: {e}")),
                }
            }
            "/invite" => {
                let words: Vec<&str> = rest.split_whitespace().collect();
                if words.first() == Some(&"stop") {
                    // The call goes on for people joined on this machine.
                    if let ReplOutput::Tui(st) = &out
                        && let Ok(mut g) = st.lock()
                    {
                        // The tab stays while there's chat to read.
                        g.team_live = false;
                    }
                    if self.invite.take().is_some() {
                        out.println("[invite] stopped — the line no longer works and guests were disconnected");
                    } else {
                        out.println("[invite] no invite is open");
                    }
                    return Ok(true);
                }
                let ReplOutput::Tui(st) = &out else {
                    out.eprintln(
                        "[invite] needs the terminal UI (it asks you before letting anyone in)",
                    );
                    return Ok(true);
                };
                let Some(socket) = self.runtime.socket_path() else {
                    out.eprintln(
                        "[invite] this session has no IPC endpoint, so nobody can join it",
                    );
                    return Ok(true);
                };
                // `/invite phone`: the web page, with approvals, for you on
                // your phone.
                let phone = words.contains(&"phone");
                let web = phone || words.contains(&"web");
                let let_approve = phone || words.contains(&"--let-approve");
                let port: u16 = words.iter().find_map(|w| w.parse().ok()).unwrap_or(0);
                // A new invite replaces the old one (and its key).
                self.invite = None;
                let title = self
                    .runtime
                    .session_name()
                    .map(str::to_string)
                    .unwrap_or_else(|| self.runtime.session_id().to_string());
                let knocks = spawn_knock_prompts((*st).clone());
                let opts = dcode_ai_cli::invite::HostOptions {
                    port,
                    web,
                    let_approve,
                    socket,
                    log_path: self.runtime.event_log_path(),
                    title,
                    knocks,
                    presence: self.runtime.event_tx(),
                    host_name: dcode_ai_cli::people::my_name(self.runtime.workspace_root()),
                    project: self
                        .runtime
                        .workspace_root()
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    session_id: self.runtime.session_id().to_string(),
                    model: self.runtime.model().to_string(),
                    auto_admit: false,
                    hub: Some(self.call_hub.clone()),
                };
                // `/invite relay <host:port>` (or $DCODE_AI_RELAY): through a
                // relay, for people on other networks.
                let relay = words.iter().position(|w| *w == "relay").map(|i| {
                    words
                        .get(i + 1)
                        .map(|s| s.to_string())
                        .or_else(|| std::env::var("DCODE_AI_RELAY").ok())
                });
                let hosted = match &relay {
                    Some(Some(addr)) => dcode_ai_cli::invite::host_relay(opts, addr).await,
                    Some(None) => {
                        out.eprintln("usage: /invite relay <host:port> — the machine running `dcode-ai relay serve`");
                        return Ok(true);
                    }
                    // `/invite lan`: this network (and Tailscale) only.
                    None if words.contains(&"lan") || !dcode_ai_cli::p2p::enabled() => {
                        dcode_ai_cli::invite::host(opts).await
                    }
                    None => dcode_ai_cli::invite::host_with_p2p(opts).await,
                };
                let via_relay = relay.is_some();
                match hosted {
                    Ok(inv) => {
                        // People can join now: the team tab (chat, to-dos)
                        // shows from here on.
                        if let Ok(mut g) = st.lock() {
                            g.team_live = true;
                        }
                        let line = inv.command();
                        let _ = crate::tui::clipboard::copy_to_clipboard(&line);
                        // The browser link (and its QR code) first, so the
                        // line to send ends up at the bottom.
                        if let Some(url) = &inv.web_url {
                            print_qr(&out, "[invite] or scan it with a phone:", url);
                            out.println(&format!(
                                "[invite] in a browser (no install; plain HTTP, so people on this network could read it): {url}"
                            ));
                            if phone {
                                out.println("[invite] phone: scan the code (same Wi-Fi, or Tailscale), tap \"Ask to join\" and let yourself in to drive — approvals and questions then show there with Allow / Deny, with a buzz when one waits (keep the page open)");
                            }
                        }
                        out.println(&format!(
                            "[invite] open on port {} — send this line to whoever should join (copied):",
                            inv.port()
                        ));
                        out.println(&format!("[invite]   {line}"));
                        out.println(if let_approve {
                            "[invite] you're asked before anyone gets in · guests can send messages, approve and stop · /invite stop ends it"
                        } else {
                            "[invite] you're asked before anyone gets in · guests can send messages (approvals stay with you) · /invite stop ends it"
                        });
                        if via_relay {
                            out.println("[invite] through the relay — works from any network; the relay only passes encrypted bytes and never sees the key");
                            self.invite = Some(inv);
                            return Ok(true);
                        }
                        if let Some(help) = crate::watch::lan_help(inv.port()) {
                            for line in help.lines() {
                                out.println(&format!("[invite] {line}"));
                            }
                        }
                        if inv.p2p {
                            out.println("[invite] works from any network too: a direct peer-to-peer link (encrypted end to end; /invite lan for this network only)");
                        }
                        match &inv.tailscale {
                            Some(ip) => out.println(&format!(
                                "[invite] works over Tailscale too ({ip})"
                            )),
                            None if inv.p2p => {}
                            None => out.println(
                                "[invite] from another network: `dcode-ai setup tailscale` on both computers, or /invite relay <host:port> (a `dcode-ai relay serve` you run)",
                            ),
                        }
                        self.invite = Some(inv);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => out.eprintln(&format!(
                        "[invite] port {} is in use — try /invite <another port>",
                        if port == 0 {
                            dcode_ai_cli::invite::DEFAULT_PORT
                        } else {
                            port
                        }
                    )),
                    Err(e) => out.eprintln(&format!("[invite] could not start: {e}")),
                }
            }
            "/call" => {
                let ReplOutput::Tui(st) = &out else {
                    out.eprintln("[call] calls need the terminal UI");
                    return Ok(true);
                };
                let Some(inv) = self.invite.as_ref().or(self.local_invite.as_ref()) else {
                    out.println("[call] calls need the session's invite — /invite starts one");
                    return Ok(true);
                };
                let already = st.lock().map(|g| g.office.voice.is_some()).unwrap_or(false);
                // In a call: the call menu (pick, don't type).
                if already && rest.trim().is_empty() {
                    if let Ok(mut g) = st.lock()
                        && let Some(menu) = crate::tui::menus::call_menu(&g)
                    {
                        g.choice_prompts.push_front(menu);
                        g.touch_transcript();
                    }
                    return Ok(true);
                }
                if already || !rest.trim().is_empty() {
                    let lines = st
                        .lock()
                        .ok()
                        .and_then(|mut g| crate::tui::call_cmd::run(rest.trim(), &mut g));
                    match lines {
                        Some(lines) => {
                            for l in lines {
                                out.println(&l);
                            }
                        }
                        None => out.println("[call] you're not in a call — /call starts one"),
                    }
                    return Ok(true);
                }
                match dcode_ai_cli::voice::Call::start(dcode_ai_cli::invite::HOST_PERSON) {
                    Ok((call, mic)) => {
                        inv.call_join(call.clone(), mic);
                        if let Ok(mut g) = st.lock() {
                            g.office.voice = Some(call);
                        }
                        out.println("[call] 📞 you started the call — guests type /call to join · /mute · /hangup · headphones avoid echo");
                    }
                    Err(e) => out.eprintln(&format!("[call] can't start: {e}")),
                }
            }
            "/mute" => {
                let ReplOutput::Tui(st) = &out else {
                    return Ok(true);
                };
                let state = st.lock().ok().and_then(|g| g.office.voice.clone());
                match state {
                    Some(v) => {
                        let on = !v.muted();
                        v.set_muted(on);
                        out.println(if on {
                            "[call] 🔇 muted — /mute again to talk"
                        } else {
                            "[call] 🎙 unmuted"
                        });
                    }
                    None => out.println("[call] you're not in a call — /call"),
                }
            }
            "/hangup" => {
                let ReplOutput::Tui(st) = &out else {
                    return Ok(true);
                };
                let had = st
                    .lock()
                    .ok()
                    .and_then(|mut g| g.office.voice.take())
                    .is_some();
                if let Some(inv) = &self.invite {
                    inv.call_leave();
                }
                out.println(if had {
                    "[call] you left the call"
                } else {
                    "[call] you're not in a call"
                });
            }
            "/chat" => {
                let text = rest.trim();
                if text.is_empty() {
                    // The chat window: the dashboard's team tab, ready to type.
                    match &out {
                        ReplOutput::Tui(st) => {
                            if let Ok(mut g) = st.lock() {
                                g.open_team_chat();
                            }
                        }
                        _ => out.println("[chat] /chat <text> talks to the people here (the agent doesn't see it)"),
                    }
                    return Ok(true);
                }
                if let ReplOutput::Tui(st) = &out {
                    let ws = self.runtime.workspace_root().to_path_buf();
                    let note = st.lock().ok().and_then(|mut g| {
                        crate::tui::team::chat_picture_command(text, &mut g, &ws)
                    });
                    if let Some(note) = note {
                        if !note.is_empty() {
                            out.println(&note);
                        }
                        return Ok(true);
                    }
                }
                let name = dcode_ai_cli::people::my_name(self.runtime.workspace_root());
                if let Some(tx) = self.runtime.event_tx() {
                    let _ = tx
                        .send(dcode_ai_common::event::AgentEvent::PersonChat {
                            person: "owner".into(),
                            name,
                            text: text.chars().take(2000).collect(),
                            image: None,
                        })
                        .await;
                }
            }
            "/todo" => {
                let arg = rest.trim();
                let name = dcode_ai_cli::people::my_name(self.runtime.workspace_root());
                let ReplOutput::Tui(st) = &out else {
                    out.eprintln("[todo] the shared to-do list needs the terminal UI");
                    return Ok(true);
                };
                if arg.is_empty() {
                    let lines = st.lock().map(|g| g.team.lines()).unwrap_or_default();
                    for l in lines {
                        out.println(&format!("[todo] {l}"));
                    }
                    return Ok(true);
                }
                if arg == "done" || arg == "tick" {
                    let menu = st
                        .lock()
                        .ok()
                        .and_then(|g| crate::tui::menus::todo_menu(&g));
                    match menu {
                        Some(menu) => {
                            if let Ok(mut g) = st.lock() {
                                g.choice_prompts.push_front(menu);
                                g.touch_transcript();
                            }
                        }
                        None => out.println("[todo] nothing open to tick off"),
                    }
                    return Ok(true);
                }
                let event = if let Some(which) = arg
                    .strip_prefix("done ")
                    .or_else(|| arg.strip_prefix("tick "))
                {
                    let id = st
                        .lock()
                        .ok()
                        .and_then(|g| g.team.find(which).map(|t| t.id.clone()));
                    match id {
                        Some(id) => {
                            dcode_ai_common::event::AgentEvent::TeamTodoDone { id, by: name }
                        }
                        None => {
                            out.eprintln(&format!("[todo] no item {which} — /todo lists them"));
                            return Ok(true);
                        }
                    }
                } else {
                    dcode_ai_common::event::AgentEvent::TeamTodoAdded {
                        id: dcode_ai_runtime::supervisor::new_todo_id(),
                        text: arg.chars().take(500).collect(),
                        by: name,
                    }
                };
                if let Some(tx) = self.runtime.event_tx() {
                    let _ = tx.send(event).await;
                }
            }
            "/people" => {
                use crate::tui::office::PersonRole;
                let people: Vec<(String, PersonRole)> = match &out {
                    ReplOutput::Tui(st) => st
                        .lock()
                        .map(|g| {
                            g.office
                                .people_ordered()
                                .into_iter()
                                .filter(|p| p.leaving.is_none())
                                .map(|p| (p.name.clone(), p.role))
                                .collect()
                        })
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                let guests = self.invite.as_ref().map(|i| i.guests()).unwrap_or_default();
                out.println(&format!("[people] {} here", people.len().max(1)));
                for (name, role) in people {
                    let what = match role {
                        PersonRole::Owner => "you · host".to_string(),
                        PersonRole::Watching => "watching (/watch page)".to_string(),
                        PersonRole::Joined => match guests.iter().find(|g| g.name == name) {
                            Some(g) => {
                                format!("joined over /invite from {} · /kick {name}", g.from)
                            }
                            None => "joined (dcode-ai join)".to_string(),
                        },
                    };
                    out.println(&format!("[people]   {name} — {what}"));
                }
            }
            "/kick" => {
                let name = rest.trim();
                if name.is_empty() {
                    // Pick who from a list.
                    let names: Vec<String> = self
                        .invite
                        .as_ref()
                        .map(|i| i.guests().into_iter().map(|g| g.name).collect())
                        .unwrap_or_default();
                    match (crate::tui::menus::kick_menu(names), &out) {
                        (Some(menu), ReplOutput::Tui(st)) => {
                            if let Ok(mut g) = st.lock() {
                                g.choice_prompts.push_front(menu);
                                g.touch_transcript();
                            }
                        }
                        _ => out.println("[kick] no invited guests are here"),
                    }
                    return Ok(true);
                }
                let n = self.invite.as_ref().map(|i| i.kick(name)).unwrap_or(0);
                if n == 0 {
                    out.eprintln(&format!(
                        "[kick] no invited guest named {name} (/people lists who is here; local joins leave with /quit, /watch stop ends the page)"
                    ));
                } else {
                    out.println(&format!(
                        "[kick] removed {name} — their invite no longer works for them"
                    ));
                }
            }
            "/handoff" => {
                let _ = self.runtime.save().await;
                let ws = self.runtime.workspace_root().to_path_buf();
                let todo = match &out {
                    ReplOutput::Tui(st) => st.lock().ok().and_then(|g| g.current_plan.clone()),
                    _ => None,
                };
                let handoff = crate::session_share_cli::Handoff {
                    from: handoff_author(&ws),
                    at: chrono::Utc::now(),
                    note: rest.trim().to_string(),
                    todo,
                    changed_files: changed_files(&ws),
                };
                let id = self.runtime.session_id().to_string();
                let sessions = ws.join(&self.runtime.config().session.history_dir);
                let file = ws.join(format!("{id}.dcode-session"));
                match crate::session_share_cli::export_with(
                    &sessions,
                    &id,
                    Some(file.clone()),
                    Some(handoff),
                )
                .await
                {
                    Ok(path) => {
                        out.println(&format!(
                            "[handoff] wrote {} (secrets masked) — your teammate runs `dcode-ai session import {}` and says \"continue\"",
                            path.display(),
                            path.file_name().and_then(|n| n.to_str()).unwrap_or_default()
                        ));
                        if rest.trim().is_empty() {
                            out.println(
                                "[handoff] tip: /handoff <note> adds what's done and what's next",
                            );
                        }
                    }
                    Err(e) => out.eprintln(&format!("[handoff] {e}")),
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// Asks you in the TUI about each knock on `/invite` ("Priya wants to
/// join — let them in?"), one at a time, when no other prompt is open.
/// Unanswered after a minute: no.
fn spawn_knock_prompts(
    st: Arc<Mutex<TuiSessionState>>,
) -> tokio::sync::mpsc::Sender<dcode_ai_cli::invite::Knock> {
    use dcode_ai_cli::invite::{KNOCK_QUESTION, expire_knock, register_knock};
    let (tx, mut rx) = tokio::sync::mpsc::channel::<dcode_ai_cli::invite::Knock>(8);
    tokio::spawn(async move {
        let mut n = 0u64;
        while let Some(knock) = rx.recv().await {
            n += 1;
            let qid = format!("{KNOCK_QUESTION}{n}");
            let mut shown = false;
            for _ in 0..300 {
                if let Ok(mut g) = st.lock()
                    && g.active_question.is_none()
                    && g.active_approval.is_none()
                {
                    let auto = matches!(
                        g.permission_mode.as_str(),
                        "Auto" | "BypassPermissions" | "DontAsk"
                    );
                    let mut prompt = format!(
                        "{} ({}) wants to join this session — let them in?",
                        knock.name, knock.from
                    );
                    if auto {
                        prompt.push_str(
                            "\n\nYou're in auto mode: letting them drive switches it to default, so the agent asks you before acting on their requests.",
                        );
                    }
                    g.active_question = Some(InteractiveQuestionPayload {
                        question_id: qid.clone(),
                        call_id: qid.clone(),
                        prompt,
                        options: vec![
                            QuestionOption {
                                id: "watch".into(),
                                label: format!("Let {} in to watch and chat", knock.name),
                                description: Some(
                                    "they see the session live and chat with people — nothing they send reaches the agent".into(),
                                ),
                            },
                            QuestionOption {
                                id: "drive".into(),
                                label: format!("Let {} in to drive", knock.name),
                                description: Some(
                                    "they can also ask the agent to do things (approvals stay with you)".into(),
                                ),
                            },
                            QuestionOption {
                                id: "deny".into(),
                                label: "No".into(),
                                description: None,
                            },
                        ],
                        allow_custom: false,
                        suggested_answer: "deny".into(),
                        header: Some(format!("Invite · {}", knock.name)),
                        ..Default::default()
                    });
                    g.open_question_modal();
                    shown = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            if !shown {
                let _ = knock.reply.send(dcode_ai_cli::invite::Admit::No);
                continue;
            }
            register_knock(&qid, knock.reply);
            let st = st.clone();
            let name = knock.name.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                if expire_knock(&qid)
                    && let Ok(mut g) = st.lock()
                {
                    if g.active_question.as_ref().map(|q| q.question_id.as_str())
                        == Some(qid.as_str())
                    {
                        g.active_question = None;
                        g.close_question_modal();
                    }
                    g.push_block(crate::tui::state::DisplayBlock::System(format!(
                        "[invite] {name} wasn't let in (no answer)"
                    )));
                }
            });
        }
    });
    tx
}

/// A link's QR code as one block (so its rows stay together and aligned).
fn print_qr(out: &ReplOutput<'_>, label: &str, url: &str) {
    let qr = dcode_ai_cli::people::qr_lines(url);
    if qr.is_empty() {
        return;
    }
    match out {
        ReplOutput::Tui(st) => {
            if let Ok(mut g) = st.lock() {
                g.push_block(crate::tui::state::DisplayBlock::System(format!(
                    "{label}\n{}",
                    qr.join("\n")
                )));
            }
        }
        _ => {
            out.println(label);
            for l in qr {
                out.println(&l);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::TuiSessionState;
    use std::path::PathBuf;

    fn make_state() -> Arc<Mutex<TuiSessionState>> {
        Arc::new(Mutex::new(TuiSessionState::new(
            "session".into(),
            "model".into(),
            "@agent".into(),
            "Default".into(),
            PathBuf::from("."),
            false,
        )))
    }

    fn system_lines(state: &Arc<Mutex<TuiSessionState>>) -> String {
        state
            .lock()
            .map(|g| {
                g.blocks
                    .iter()
                    .filter_map(|b| match b {
                        crate::tui::state::DisplayBlock::System(t) => Some(t.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    /// `/talk` in the background with the fake microphone (needs the
    /// speech model: `cargo test -p dcode-ai-cli -- --ignored talk_session`).
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn talk_session_test_prompt_and_esc() {
        // SAFETY: test-only, before any audio starts.
        unsafe { std::env::set_var("DCODE_AI_FAKE_AUDIO", "tone") };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TuiCmd>();

        // The microphone check reports the level and what it heard.
        let state = make_state();
        talk_session(state.clone(), tx.clone(), "tiny.en".into(), TalkMode::Test).await;
        let out = system_lines(&state);
        eprintln!("{out}");
        assert!(out.contains("fake ·"), "{out}");
        assert!(out.contains("speech"), "{out}");
        assert!(state.lock().is_ok_and(|g| g.talk_stop.is_none()));

        // Prompt mode: the tone ends at a pause, the text goes to the prompt.
        let state = make_state();
        talk_session(
            state.clone(),
            tx.clone(),
            "tiny.en".into(),
            TalkMode::Prompt,
        )
        .await;
        let out = system_lines(&state);
        assert!(!out.contains("stopped"), "{out}");
        assert!(state.lock().is_ok_and(|g| g.talk_stop.is_none()));

        // Hands-free: Esc (the flag) ends the loop within moments, nothing sent.
        let state = make_state();
        let session = tokio::spawn(talk_session(
            state.clone(),
            tx.clone(),
            "tiny.en".into(),
            TalkMode::Loop,
        ));
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        let flag = state
            .lock()
            .ok()
            .and_then(|g| g.talk_stop.clone())
            .expect("armed");
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        tokio::time::timeout(std::time::Duration::from_secs(20), session)
            .await
            .expect("Esc ends the loop")
            .unwrap();
        assert!(state.lock().is_ok_and(|g| g.talk_stop.is_none()));
        let out = system_lines(&state);
        assert!(
            out.contains("in the prompt") || out.contains("stopped"),
            "{out}"
        );
        assert!(rx.try_recv().is_err(), "Esc sends nothing");
    }

    #[test]
    fn talk_stop_guard_clears_after_drop() {
        let state = make_state();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

        {
            let _guard = talk_stop_guard_for_tui(state.clone(), stop.clone());
            assert!(
                state
                    .lock()
                    .ok()
                    .and_then(|g| g.talk_stop.as_ref().cloned())
                    .is_some()
            );
        }

        assert!(state.lock().is_ok_and(|g| g.talk_stop.is_none()));
    }

    #[test]
    fn talk_stop_guard_replaces_stale_flag_and_restores_none() {
        let state = make_state();
        let stale = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if let Ok(mut g) = state.lock() {
            g.talk_stop = Some(stale);
        }

        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let _guard = talk_stop_guard_for_tui(state.clone(), stop.clone());
            assert!(state.lock().is_ok_and(|g| {
                g.talk_stop
                    .as_ref()
                    .is_some_and(|cur| Arc::ptr_eq(cur, &stop))
            }));
        }

        assert!(state.lock().is_ok_and(|g| g.talk_stop.is_none()));
    }
}

#[cfg(test)]
mod wake_tests {
    use super::*;

    #[test]
    fn the_wake_word_is_found_however_whisper_spells_it() {
        assert_eq!(
            after_wake_word("Hey, DCode, run the tests.").as_deref(),
            Some("run the tests")
        );
        assert_eq!(after_wake_word("hey decode").as_deref(), Some(""));
        assert_eq!(
            after_wake_word("Hey D-code what changed today?").as_deref(),
            Some("what changed today?")
        );
        assert_eq!(after_wake_word("let's decode this file"), None);
        assert_eq!(after_wake_word("hello there"), None);
    }

    #[test]
    fn spoken_requests_ask_for_the_language() {
        let r = spoken_request("wie spät ist es", Some("de"));
        assert!(r.contains("answer in German"), "{r}");
        let r = spoken_request("hi", Some("en"));
        assert!(!r.contains("answer in"), "{r}");
    }
}
