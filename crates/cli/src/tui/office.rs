//! The Agent Office (`/office`, dashboard `Alt+5`): every agent at work as
//! a small animated pixel-art character at a desk — the main agent and each
//! sub-agent — acting out what it is doing, with a speech bubble.
//!
//! Pixels are drawn with half blocks (`▀`: the top pixel is the foreground,
//! the bottom one the background), so one terminal cell holds two square
//! pixels. All art is drawn in code here (no image files).

use std::time::{Duration, Instant};

use dcode_ai_common::event::{AgentEvent, BusyState};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

/// What an agent is doing, as the office shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Idle,
    Thinking,
    Typing,
    Reading,
    Shell,
    Browsing,
    WaitingApproval,
    Asking,
    Tidying,
    Delegating,
    Done,
    Failed,
}

impl Activity {
    /// Light from the laptop and the bubble's accent.
    fn glow(self) -> Color {
        match self {
            Activity::Typing => rgb(0x7a, 0xa2, 0xf7),
            Activity::Reading => rgb(0xe0, 0xc0, 0x7a),
            Activity::Shell => rgb(0x7f, 0xd8, 0x6f),
            Activity::Browsing => rgb(0x5f, 0xc8, 0xe8),
            Activity::WaitingApproval | Activity::Asking => rgb(0xf0, 0xb2, 0x3c),
            Activity::Thinking | Activity::Tidying => rgb(0xc0, 0x9a, 0xf0),
            Activity::Delegating => rgb(0xd9, 0x77, 0x57),
            Activity::Done => rgb(0x7f, 0xd8, 0x6f),
            Activity::Failed => rgb(0xe0, 0x6c, 0x75),
            Activity::Idle => rgb(0x55, 0x5a, 0x66),
        }
    }
}

/// One agent in the office.
#[derive(Debug, Clone)]
pub struct OfficeAgent {
    pub id: String,
    pub name: String,
    pub activity: Activity,
    /// The bubble's text: `Edit main.rs`, `$ cargo test`, `? edit_file`.
    pub bubble: String,
    pub since: Instant,
    /// When a sub-agent finished (it leaves a few seconds later).
    pub finished: Option<Instant>,
    /// Look (hair, skin, shirt), stable per agent.
    pub look: usize,
}

/// How long a finished sub-agent stays at its desk.
const LINGER: Duration = Duration::from_secs(8);

/// Everyone in the office: the main agent first.
#[derive(Debug)]
pub struct Office {
    pub agents: Vec<OfficeAgent>,
    /// People with the session: you, teammates on `join`, `/watch` viewers.
    pub people: Vec<OfficePerson>,
}

/// How a person is with the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonRole {
    /// Running this session.
    Owner,
    /// On `dcode-ai join`: can send messages.
    Joined,
    /// On the `/watch` page: read-only.
    Watching,
}

/// A person in the office.
#[derive(Debug, Clone)]
pub struct OfficePerson {
    pub id: String,
    pub name: String,
    pub role: PersonRole,
    /// What they last said (shown for a while).
    pub said: String,
    pub said_at: Option<Instant>,
    pub last_seen: Instant,
    /// Leaving: waves, then goes.
    pub leaving: Option<Instant>,
    pub look: usize,
}

impl OfficePerson {
    /// The bubble right now.
    pub fn bubble(&self) -> String {
        if self.leaving.is_some() {
            return "bye 👋".into();
        }
        if self.said_at.is_some_and(|t| t.elapsed() < SAID_FOR) {
            return format!("💬 {}", self.said);
        }
        match self.role {
            PersonRole::Watching => "👀 watching".into(),
            _ => String::new(),
        }
    }

    pub fn talking(&self) -> bool {
        self.said_at.is_some_and(|t| t.elapsed() < SAID_FOR)
    }
}

/// How long a message stays in its bubble.
const SAID_FOR: Duration = Duration::from_secs(8);
/// A joined person not heard from for this long has gone.
const AWAY_AFTER: Duration = Duration::from_secs(25);
/// How long the wave lasts before someone is gone.
const WAVE_FOR: Duration = Duration::from_secs(3);

/// A look for a name: the same person always looks the same.
fn look_for(name: &str) -> usize {
    let h = name
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    1 + (h as usize % (LOOKS.len() - 1))
}

impl Default for Office {
    fn default() -> Self {
        Self {
            agents: vec![OfficeAgent {
                id: "main".into(),
                name: "dcode-ai".into(),
                activity: Activity::Idle,
                bubble: String::new(),
                since: Instant::now(),
                finished: None,
                look: 0,
            }],
            people: Vec::new(),
        }
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn short(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > max {
        format!(
            "{}…",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    } else {
        s
    }
}

/// What a tool call looks like at the desk.
pub fn tool_activity(tool: &str, input: &serde_json::Value) -> (Activity, String) {
    let path = ["path", "file_path", "notebook_path"]
        .iter()
        .find_map(|k| input[*k].as_str())
        .map(file_name)
        .unwrap_or("");
    match tool {
        "edit_file" | "write_file" | "apply_patch" | "replace_match" | "notebook_edit" => {
            (Activity::Typing, short(&format!("Edit {path}"), 22))
        }
        "read_file" | "list_directory" | "inspect_binary" | "query_symbols" => {
            (Activity::Reading, short(&format!("Read {path}"), 22))
        }
        "search_code" | "code_intel" | "repo_map" | "tool_search" => {
            let q = input["pattern"]
                .as_str()
                .or_else(|| input["query"].as_str())
                .unwrap_or("");
            (Activity::Reading, short(&format!("Search {q}"), 22))
        }
        "execute_bash" | "run_background" | "run_validation" | "interactive_exec" => {
            let cmd = input["command"].as_str().unwrap_or("");
            (Activity::Shell, short(&format!("$ {cmd}"), 22))
        }
        "browser" => {
            let action = input["action"].as_str().unwrap_or("page");
            (Activity::Browsing, short(&format!("Browser: {action}"), 22))
        }
        "web_search" | "fetch_url" => (Activity::Browsing, "Web".into()),
        "spawn_subagent" => (Activity::Delegating, "Delegating".into()),
        "ask_question" => (Activity::Asking, "? question".into()),
        "update_plan" => (Activity::Typing, "Planning".into()),
        "delete_path" | "move_path" | "rename_path" | "copy_path" | "create_directory" => {
            (Activity::Typing, short(&format!("Files {path}"), 22))
        }
        other => (Activity::Typing, short(other, 22)),
    }
}

impl Office {
    fn set(&mut self, i: usize, activity: Activity, bubble: impl Into<String>) {
        if let Some(a) = self.agents.get_mut(i) {
            if a.activity != activity {
                a.since = Instant::now();
            }
            a.activity = activity;
            a.bubble = bubble.into();
        }
    }

    fn child(&mut self, id: &str) -> Option<usize> {
        self.agents.iter().position(|a| a.id == id)
    }

    /// Finished sub-agents leave after a while.
    pub fn tidy(&mut self) {
        self.agents
            .retain(|a| a.finished.is_none_or(|t| t.elapsed() < LINGER));
        for p in &mut self.people {
            if p.role != PersonRole::Owner
                && p.leaving.is_none()
                && p.last_seen.elapsed() > AWAY_AFTER
            {
                p.leaving = Some(Instant::now());
            }
        }
        self.people
            .retain(|p| p.leaving.is_none_or(|t| t.elapsed() < WAVE_FOR));
    }

    /// You, at the lead desk (named after git's `user.name`).
    pub fn set_owner(&mut self, name: &str) {
        self.people.retain(|p| p.role != PersonRole::Owner);
        self.people.insert(
            0,
            OfficePerson {
                id: "owner".into(),
                name: name.to_string(),
                role: PersonRole::Owner,
                said: String::new(),
                said_at: None,
                last_seen: Instant::now(),
                leaving: None,
                look: look_for(name),
            },
        );
    }

    /// People in drawing order: you, then teammates, then watchers.
    pub fn people_ordered(&self) -> Vec<&OfficePerson> {
        let mut v: Vec<&OfficePerson> = self.people.iter().collect();
        v.sort_by_key(|p| match p.role {
            PersonRole::Owner => 0,
            PersonRole::Joined => 1,
            PersonRole::Watching => 2,
        });
        v
    }

    fn person(&mut self, id: &str) -> Option<&mut OfficePerson> {
        self.people.iter_mut().find(|p| p.id == id)
    }

    pub fn apply(&mut self, ev: &AgentEvent) {
        match ev {
            AgentEvent::BusyStateChanged { state } => match state {
                BusyState::Idle => {
                    let done = !matches!(self.agents[0].activity, Activity::Idle);
                    self.set(0, Activity::Idle, if done { "done ✓" } else { "" });
                }
                BusyState::Thinking => self.set(0, Activity::Thinking, "thinking"),
                BusyState::Streaming => self.set(0, Activity::Typing, "writing"),
                BusyState::ApprovalPending => self.set(0, Activity::WaitingApproval, "? waiting"),
                BusyState::Error => self.set(0, Activity::Failed, "error"),
                _ => {}
            },
            AgentEvent::ToolCallStarted { tool, input, .. } => {
                let (activity, bubble) = tool_activity(tool, input);
                self.set(0, activity, bubble);
            }
            AgentEvent::ApprovalRequested { tool, .. } => self.set(
                0,
                Activity::WaitingApproval,
                short(&format!("? {tool}"), 22),
            ),
            AgentEvent::ApprovalResolved { .. } => self.set(0, Activity::Thinking, "thinking"),
            AgentEvent::QuestionRequested { .. } => self.set(0, Activity::Asking, "? question"),
            AgentEvent::ContextCompaction { phase, .. } if phase == "starting" => {
                self.set(0, Activity::Tidying, "tidying up")
            }
            AgentEvent::ChildSessionSpawned {
                child_session_id,
                task,
                ..
            } => {
                if self.child(child_session_id).is_none() {
                    let look = 1 + self.agents.len() % (LOOKS.len() - 1);
                    self.agents.push(OfficeAgent {
                        id: child_session_id.clone(),
                        name: short(task, 18),
                        activity: Activity::Thinking,
                        bubble: "starting".into(),
                        since: Instant::now(),
                        finished: None,
                        look,
                    });
                }
            }
            AgentEvent::ChildSessionActivity {
                child_session_id,
                phase,
                detail,
            } => {
                if let Some(i) = self.child(child_session_id) {
                    let (activity, bubble) = if phase == "provider_request" {
                        (Activity::Thinking, "thinking".to_string())
                    } else {
                        let (a, b) = tool_activity(phase, &serde_json::Value::Null);
                        let b = if detail.trim().is_empty() {
                            b
                        } else {
                            short(detail, 22)
                        };
                        (a, b)
                    };
                    self.set(i, activity, bubble);
                }
            }
            AgentEvent::PersonJoined { person, name, role } => {
                let role = match role.as_str() {
                    "watch" => PersonRole::Watching,
                    "owner" => PersonRole::Owner,
                    _ => PersonRole::Joined,
                };
                if let Some(p) = self.person(person) {
                    p.last_seen = Instant::now();
                    p.leaving = None;
                    p.name = name.clone();
                } else {
                    self.people.push(OfficePerson {
                        id: person.clone(),
                        name: name.clone(),
                        role,
                        said: String::new(),
                        said_at: None,
                        last_seen: Instant::now(),
                        leaving: None,
                        look: look_for(name),
                    });
                }
            }
            AgentEvent::PersonSeen { person } => {
                if let Some(p) = self.person(person) {
                    p.last_seen = Instant::now();
                }
            }
            AgentEvent::PersonLeft { person, .. } => {
                if let Some(p) = self.person(person)
                    && p.leaving.is_none()
                {
                    p.leaving = Some(Instant::now());
                }
            }
            AgentEvent::PersonSaid { person, text, .. } => {
                if let Some(p) = self.person(person) {
                    p.said = short(text, 20);
                    p.said_at = Some(Instant::now());
                    p.last_seen = Instant::now();
                }
            }
            // What you type shows over your head.
            AgentEvent::MessageReceived { role, content }
                if role == "user" && !content.starts_with("(from ") =>
            {
                if let Some(p) = self.person("owner") {
                    p.said = short(content, 20);
                    p.said_at = Some(Instant::now());
                }
            }
            AgentEvent::ChildSessionCompleted {
                child_session_id,
                status,
                ..
            } => {
                if let Some(i) = self.child(child_session_id) {
                    let ok = status == "completed";
                    self.set(
                        i,
                        if ok { Activity::Done } else { Activity::Failed },
                        if ok { "done ✓" } else { "stopped" },
                    );
                    self.agents[i].finished = Some(Instant::now());
                }
            }
            _ => {}
        }
    }
}

// ── Drawing ──────────────────────────────────────────────────────────────

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// Hair, skin, shirt.
const LOOKS: [(Color, Color, Color); 6] = [
    (
        rgb(0x3a, 0x2a, 0x22),
        rgb(0xf2, 0xc9, 0xa0),
        rgb(0xd9, 0x77, 0x57),
    ),
    (
        rgb(0xd9, 0xa4, 0x41),
        rgb(0xe8, 0xb8, 0x8a),
        rgb(0x3b, 0x5b, 0xa5),
    ),
    (
        rgb(0x1e, 0x1e, 0x24),
        rgb(0x8d, 0x55, 0x24),
        rgb(0x2f, 0x8f, 0x6b),
    ),
    (
        rgb(0xb3, 0x3a, 0x3a),
        rgb(0xf2, 0xc9, 0xa0),
        rgb(0x6b, 0x4f, 0xa8),
    ),
    (
        rgb(0xe6, 0xe6, 0xe6),
        rgb(0xc6, 0x8e, 0x62),
        rgb(0xc4, 0x5b, 0x8f),
    ),
    (
        rgb(0x6b, 0x3e, 0x26),
        rgb(0xf2, 0xd2, 0xb0),
        rgb(0x3f, 0x7f, 0xbf),
    ),
];

const WALL: Color = rgb(0x2a, 0x30, 0x3c);
const WALL_TRIM: Color = rgb(0x5a, 0x3e, 0x2c);
const WINDOW: Color = rgb(0x4a, 0x6a, 0x8a);
const WINDOW_LIGHT: Color = rgb(0x7f, 0x9c, 0xbc);
const FLOOR_A: Color = rgb(0x4a, 0x34, 0x26);
const FLOOR_B: Color = rgb(0x55, 0x3b, 0x2b);
const FLOOR_SEAM: Color = rgb(0x3a, 0x29, 0x1e);
const DESK_TOP: Color = rgb(0xc4, 0x94, 0x66);
const DESK_FRONT: Color = rgb(0x8c, 0x62, 0x39);
const DESK_LEG: Color = rgb(0x5c, 0x40, 0x26);
const CHAIR: Color = rgb(0x3a, 0x3f, 0x4b);
const LAPTOP: Color = rgb(0xa8, 0xae, 0xb6);
const LAPTOP_DARK: Color = rgb(0x6c, 0x72, 0x7a);
const EYE: Color = rgb(0x22, 0x22, 0x28);
const PAPER: Color = rgb(0xf4, 0xf1, 0xe8);
const MUG: Color = rgb(0xe8, 0xe8, 0xf0);

/// Pixel grid: `w` × `h` square pixels (each cell = 1 × 2 pixels).
struct Canvas {
    w: usize,
    h: usize,
    px: Vec<Color>,
}

impl Canvas {
    fn new(w: usize, h: usize, fill: Color) -> Self {
        Self {
            w,
            h,
            px: vec![fill; w * h],
        }
    }

    fn set(&mut self, x: i32, y: i32, c: Color) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            self.px[y as usize * self.w + x as usize] = c;
        }
    }

    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.set(xx, yy, c);
            }
        }
    }
}

/// Width and height of one desk, in pixels.
pub const STATION_W: usize = 24;
const STATION_PX_H: usize = 24;
/// Rows of one desk: bubble, pixels, name.
pub const STATION_ROWS: u16 = 1 + (STATION_PX_H as u16 / 2) + 1;
/// Wall height in rows.
const WALL_ROWS: usize = 3;

fn draw_room(c: &mut Canvas, tick: u64) {
    let wall_px = WALL_ROWS * 2;
    c.rect(0, 0, c.w as i32, wall_px as i32, WALL);
    // Windows along the wall, softly lit.
    let mut x = 4;
    while x + 10 < c.w as i32 {
        c.rect(x, 1, 10, 4, WINDOW);
        c.rect(x + 1, 2, 4, 2, WINDOW_LIGHT);
        c.rect(x + 6, 2, 3, 2, WINDOW_LIGHT);
        x += 22;
    }
    c.rect(0, wall_px as i32 - 1, c.w as i32, 1, WALL_TRIM);
    // Plank floor.
    for y in wall_px..c.h {
        let band = (y - wall_px) / 4;
        let color = if band.is_multiple_of(2) {
            FLOOR_A
        } else {
            FLOOR_B
        };
        for x in 0..c.w {
            let seam = (x + band * 7).is_multiple_of(18);
            c.set(x as i32, y as i32, if seam { FLOOR_SEAM } else { color });
        }
    }
    // A plant on the left, a water cooler on the right.
    let floor_top = wall_px as i32;
    if c.w > 40 {
        let leaf = rgb(0x4f, 0x9a, 0x5a);
        let leaf_dark = rgb(0x35, 0x74, 0x41);
        c.rect(2, floor_top + 4, 4, 3, rgb(0x9a, 0x5b, 0x3a));
        for (dx, dy, col) in [
            (3, 0, leaf),
            (2, 1, leaf_dark),
            (4, 1, leaf),
            (1, 2, leaf),
            (3, 2, leaf_dark),
            (5, 2, leaf_dark),
            (2, 3, leaf),
            (4, 3, leaf),
        ] {
            c.set(dx, floor_top + dy, col);
        }
        let x = c.w as i32 - 6;
        c.rect(x, floor_top, 4, 3, rgb(0x7f, 0xc4, 0xe8));
        c.rect(x, floor_top + 3, 4, 4, rgb(0xd8, 0xdc, 0xe0));
        c.set(x + 1, floor_top + 4, rgb(0x5a, 0x8f, 0xd0));
    }
    // A slow light sweep across the windows.
    let sweep = (tick / 4) as i32 % (c.w as i32 + 20) - 10;
    for dy in 1..5 {
        c.set(sweep, dy, WINDOW_LIGHT);
    }
}

/// One desk with its agent at `(ox, oy)` (pixels), at animation `tick`.
fn draw_station(c: &mut Canvas, ox: i32, oy: i32, agent: &OfficeAgent, tick: u64) {
    let (hair, skin, shirt) = LOOKS[agent.look % LOOKS.len()];
    let act = agent.activity;
    let f = tick as i32;
    let p = |c: &mut Canvas, x: i32, y: i32, col: Color| c.set(ox + x, oy + y, col);
    let r = |c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, col: Color| {
        c.rect(ox + x, oy + y, w, h, col)
    };

    // Head bob while thinking; slump while idle; stand when done.
    let head_dy = match act {
        Activity::Thinking | Activity::Tidying => (f / 4) % 2,
        Activity::Idle => 1,
        Activity::Done => -1,
        _ => 0,
    };

    // Chair back.
    r(c, 6, 7, 11, 9, CHAIR);
    // Hair and face.
    let hy = 2 + head_dy;
    r(c, 8, hy, 7, 3, hair);
    r(c, 8, hy + 3, 1, 2, hair);
    r(c, 14, hy + 3, 1, 2, hair);
    r(c, 9, hy + 2, 5, 5, skin);
    r(c, 9, hy + 2, 5, 1, hair);
    // Eyes: closed when idle, blinking now and then, looking across while reading.
    let blink = (f % 37) == 0;
    if matches!(act, Activity::Idle) || blink {
        // Closed eyelids: a darker skin pixel where each eye is.
        let lid = rgb(0xa0, 0x70, 0x50);
        p(c, 10, hy + 4, lid);
        p(c, 12, hy + 4, lid);
    } else {
        let look = if act == Activity::Reading {
            (f / 6) % 2
        } else {
            0
        };
        p(c, 10 + look, hy + 4, EYE);
        p(c, 12 + look, hy + 4, EYE);
    }
    // Neck and body.
    r(c, 10, hy + 7, 3, 1, skin);
    r(c, 7, 10, 9, 6, shirt);
    r(c, 10, 10, 3, 1, skin); // collar

    // Arms: typing alternates, raised for approval, both up when done.
    let typing = matches!(act, Activity::Typing | Activity::Shell | Activity::Browsing);
    let fast = if act == Activity::Shell { 2 } else { 1 };
    let left_up = typing && (f / fast) % 2 == 0;
    let right_up = typing && (f / fast) % 2 == 1;
    match act {
        Activity::WaitingApproval | Activity::Asking => {
            // Right hand raised, waving.
            let wave = (f / 3) % 2;
            r(c, 6, 11, 1, 4, shirt);
            r(c, 6, 15, 2, 1, skin);
            r(c, 16, 3 + wave, 1, 8, shirt);
            r(c, 16, 2 + wave, 2, 2, skin);
        }
        Activity::Done => {
            r(c, 5, 4, 1, 7, shirt);
            p(c, 5, 3, skin);
            r(c, 17, 4, 1, 7, shirt);
            p(c, 17, 3, skin);
        }
        Activity::Thinking => {
            // Hand on chin.
            r(c, 6, 11, 1, 4, shirt);
            r(c, 6, 15, 2, 1, skin);
            r(c, 13, 10, 2, 1, shirt);
            p(c, 13, hy + 7, skin);
        }
        _ => {
            r(c, 6, 11, 1, 4, shirt);
            r(c, 16, 11, 1, 4, shirt);
            // Hands rest on the desk edge; typing lifts them in turn.
            r(c, 6, 15 - left_up as i32, 2, 1, skin);
            r(c, 15, 15 - right_up as i32, 2, 1, skin);
        }
    }

    // Desk.
    r(c, 1, 16, 22, 1, DESK_TOP);
    r(c, 1, 17, 22, 2, DESK_FRONT);
    r(c, 2, 19, 2, 5, DESK_LEG);
    r(c, 20, 19, 2, 5, DESK_LEG);

    // Laptop (we see the lid's back); its logo glows with the activity.
    r(c, 13, 11, 7, 5, LAPTOP);
    r(c, 12, 16, 9, 1, LAPTOP_DARK);
    let pulse = matches!(
        act,
        Activity::Typing
            | Activity::Shell
            | Activity::Browsing
            | Activity::Reading
            | Activity::Thinking
            | Activity::Tidying
            | Activity::Delegating
    ) && (f / 2) % 3 != 0;
    let glow = if pulse || matches!(act, Activity::Done | Activity::Failed) {
        act.glow()
    } else {
        LAPTOP_DARK
    };
    r(c, 15, 12, 3, 2, glow);
    if matches!(act, Activity::WaitingApproval | Activity::Asking) && (f / 3) % 2 == 0 {
        r(c, 15, 12, 3, 2, act.glow());
    }

    // Props: a mug; papers while reading; papers flying while tidying.
    r(c, 3, 14, 2, 2, MUG);
    if act == Activity::Reading {
        r(c, 6, 15, 4, 1, PAPER);
    }
    if act == Activity::Tidying {
        for k in 0..3 {
            let x = 3 + ((f + k * 5) % 16);
            let y = 6 + ((f / 2 + k * 3) % 8);
            p(c, x, y, PAPER);
        }
    }
    if act == Activity::Delegating {
        // A note travels to the next desk.
        let x = 18 + (f % 6);
        p(c, x, 14, PAPER);
    }
}

/// A person (you, a teammate, a viewer) at `(ox, oy)`.
fn draw_person(c: &mut Canvas, ox: i32, oy: i32, person: &OfficePerson, tick: u64) {
    let (hair, skin, shirt) = LOOKS[person.look % LOOKS.len()];
    let f = tick as i32;
    // Walking out: slide right as the wave goes on.
    let dx = person
        .leaving
        .map(|t| (t.elapsed().as_millis() / 120) as i32)
        .unwrap_or(0);
    let ox = ox + dx;
    let p = |c: &mut Canvas, x: i32, y: i32, col: Color| c.set(ox + x, oy + y, col);
    let r = |c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, col: Color| {
        c.rect(ox + x, oy + y, w, h, col)
    };
    let standing = person.role == PersonRole::Watching;
    // Watchers sway a little; standing people are taller.
    let sway = if standing && (f / 8) % 2 == 0 { 1 } else { 0 };
    let top = if standing { 1 } else { 2 };
    let x0 = sway;

    if !standing {
        r(c, 6, 7, 11, 9, CHAIR);
    }
    // Head.
    r(c, 8 + x0, top, 7, 3, hair);
    r(c, 8 + x0, top + 3, 1, 2, hair);
    r(c, 14 + x0, top + 3, 1, 2, hair);
    r(c, 9 + x0, top + 2, 5, 5, skin);
    r(c, 9 + x0, top + 2, 5, 1, hair);
    p(c, 10 + x0, top + 4, EYE);
    p(c, 12 + x0, top + 4, EYE);
    // Talking: the mouth opens and closes.
    if person.talking() && (f / 2) % 2 == 0 {
        p(c, 11 + x0, top + 6, rgb(0x8a, 0x3a, 0x3a));
    }
    r(c, 10 + x0, top + 7, 3, 1, skin);
    // Body.
    let body_y = top + 8;
    r(c, 7 + x0, body_y, 9, 6, shirt);
    r(c, 10 + x0, body_y, 3, 1, skin);

    if person.leaving.is_some() {
        // Waving goodbye.
        let wave = (f / 2) % 2;
        r(c, 16 + x0, top + 1 + wave, 1, 7, shirt);
        r(c, 16 + x0, top + wave, 2, 2, skin);
        r(c, 6 + x0, body_y + 1, 1, 4, shirt);
    } else if standing {
        // Holding a mug at the chest.
        r(c, 6 + x0, body_y + 1, 1, 4, shirt);
        r(c, 16 + x0, body_y + 1, 1, 3, shirt);
        r(c, 12 + x0, body_y + 3, 4, 3, MUG);
        p(c, 16 + x0, body_y + 4, MUG);
    } else {
        r(c, 6, body_y + 1, 1, 4, shirt);
        r(c, 16, body_y + 1, 1, 4, shirt);
        // Writing while you talk.
        let pen = person.talking() && (f / 2) % 2 == 0;
        r(c, 6, 15, 2, 1, skin);
        r(c, 15, 15 - pen as i32, 2, 1, skin);
    }

    if standing {
        // Legs.
        let pants = rgb(0x2e, 0x34, 0x40);
        r(c, 8 + x0, body_y + 6, 3, 7, pants);
        r(c, 12 + x0, body_y + 6, 3, 7, pants);
        r(c, 8 + x0, body_y + 13, 3, 1, EYE);
        r(c, 12 + x0, body_y + 13, 3, 1, EYE);
        return;
    }
    // Desk: yours has a notebook, a guest's a coffee.
    r(c, 1, 16, 22, 1, DESK_TOP);
    r(c, 1, 17, 22, 2, DESK_FRONT);
    r(c, 2, 19, 2, 5, DESK_LEG);
    r(c, 20, 19, 2, 5, DESK_LEG);
    match person.role {
        PersonRole::Owner => {
            r(c, 14, 14, 6, 2, PAPER);
            r(c, 15, 14, 4, 1, rgb(0x9c, 0xb4, 0xd8));
            p(c, 19, 13, rgb(0x33, 0x33, 0x33));
        }
        _ => {
            r(c, 16, 13, 3, 3, MUG);
            p(c, 19, 14, MUG);
            // Steam.
            if (f / 3) % 2 == 0 {
                p(c, 17, 11, rgb(0xd0, 0xd0, 0xd0));
            } else {
                p(c, 18, 10, rgb(0xd0, 0xd0, 0xd0));
            }
        }
    }
}

/// Everyone drawn, in order: you, the agents, teammates, viewers.
pub enum Who<'a> {
    Agent(&'a OfficeAgent),
    Person(&'a OfficePerson),
}

impl Office {
    pub fn stations(&self) -> Vec<Who<'_>> {
        let people = self.people_ordered();
        let mut out: Vec<Who> = people
            .iter()
            .filter(|p| p.role == PersonRole::Owner)
            .map(|p| Who::Person(p))
            .collect();
        out.extend(self.agents.iter().map(Who::Agent));
        out.extend(
            people
                .iter()
                .filter(|p| p.role != PersonRole::Owner)
                .map(|p| Who::Person(p)),
        );
        out
    }
}

/// Desks that fit side by side in `width` logical pixels.
pub fn desks_per_row(width: u16) -> usize {
    (width as usize / STATION_W).max(1)
}

fn height_at(width: u16, n: usize, scale: u16) -> u16 {
    let per_row = desks_per_row(width / scale);
    let rows = n.div_ceil(per_row).max(1) as u16;
    (WALL_ROWS as u16 + rows * STATION_ROWS) * scale + 1
}

/// Scale and height for `n` agents in `width` × at most `max_height`
/// cells: twice as big when everyone still fits.
pub fn office_layout(width: u16, max_height: u16, n: usize) -> (u16, u16) {
    let n = n.max(1);
    // Every desk (up to four in a row) must fit at double size.
    let fits_wide = (width / 2) as usize >= STATION_W * n.min(4);
    if fits_wide && height_at(width, n, 2) <= max_height {
        (2, height_at(width, n, 2))
    } else {
        (1, height_at(width, n, 1).min(max_height.max(1)))
    }
}

/// Rows the office needs at `width` for `n` agents (normal size).
pub fn office_height(width: u16, n: usize) -> u16 {
    height_at(width, n, 1)
}

/// The office as a widget.
pub struct OfficeView<'a> {
    pub office: &'a Office,
    pub tick: u64,
    /// Index of the highlighted agent (its name is shown inverted).
    pub selected: Option<usize>,
}

impl Widget for OfficeView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 10 || area.height < 4 {
            return;
        }
        let stations = self.office.stations();
        let n = stations.len().max(1);
        let (scale, _) = office_layout(area.width, area.height, n);
        let s = scale as usize;
        // Draw at logical size, then blow each pixel up `s` times.
        let w = area.width as usize / s;
        let rows = area.height as usize / s;
        let mut canvas = Canvas::new(w.max(1), (rows * 2).max(2), FLOOR_A);
        draw_room(&mut canvas, self.tick);

        let per_row = desks_per_row(w as u16);
        let used = per_row.min(n) * STATION_W;
        let left = w.saturating_sub(used) / 2;
        let mut labels: Vec<(u16, u16, String, Style)> = Vec::new();
        for (i, who) in stations.iter().enumerate() {
            let col = i % per_row;
            let row = i / per_row;
            let cell_x = left + col * STATION_W;
            let cell_y = WALL_ROWS + row * STATION_ROWS as usize;
            if cell_y + STATION_ROWS as usize > rows {
                break;
            }
            // Pixels start one row below the bubble row.
            let (px, py) = (cell_x as i32, ((cell_y + 1) * 2) as i32);
            let (bubble, name, tag) = match who {
                Who::Agent(a) => {
                    draw_station(&mut canvas, px, py, a, self.tick);
                    (
                        a.bubble.clone(),
                        a.name.clone(),
                        (a.activity.glow(), rgb(0x1a, 0x16, 0x14)),
                    )
                }
                Who::Person(p) => {
                    draw_person(&mut canvas, px, py, p, self.tick);
                    let bg = match p.role {
                        PersonRole::Owner => rgb(0xd9, 0x77, 0x57),
                        PersonRole::Joined => rgb(0x3f, 0x7f, 0xbf),
                        PersonRole::Watching => rgb(0x5a, 0x60, 0x6c),
                    };
                    let name = match p.role {
                        PersonRole::Owner => format!("{} (you)", p.name),
                        _ => p.name.clone(),
                    };
                    (p.bubble(), name, (bg, bg))
                }
            };
            let centre = |text: &str| -> u16 {
                let tw = text.chars().count();
                ((cell_x * s + (STATION_W * s).saturating_sub(tw) / 2) as u16)
                    .min(area.width.saturating_sub(tw as u16))
            };
            if !bubble.is_empty() {
                let text = format!(" {bubble} ");
                labels.push((
                    centre(&text),
                    (cell_y * s + s - 1) as u16,
                    text,
                    Style::default()
                        .fg(rgb(0x1a, 0x1a, 0x1f))
                        .bg(rgb(0xf4, 0xf1, 0xe8))
                        .add_modifier(Modifier::BOLD),
                ));
            }
            let name = format!(" {} ", short(&name, STATION_W * s - 4));
            let is_person = matches!(who, Who::Person(_));
            let mut style = if is_person {
                Style::default()
                    .fg(rgb(0xf8, 0xf6, 0xf2))
                    .bg(tag.1)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(rgb(0xec, 0xe6, 0xdf))
                    .bg(rgb(0x1a, 0x16, 0x14))
            };
            if self.selected == Some(i) {
                style = style
                    .fg(rgb(0x1a, 0x16, 0x14))
                    .bg(tag.0)
                    .add_modifier(Modifier::BOLD);
            }
            labels.push((
                centre(&name),
                ((cell_y + STATION_ROWS as usize - 1) * s) as u16,
                name,
                style,
            ));
        }

        // Pixels → half blocks.
        let ch = canvas.h;
        for row in 0..area.height as usize {
            for x in 0..area.width as usize {
                let lx = (x / s).min(canvas.w - 1);
                let top = canvas.px[((row * 2) / s).min(ch - 1) * canvas.w + lx];
                let bottom = canvas.px[((row * 2 + 1) / s).min(ch - 1) * canvas.w + lx];
                if let Some(cell) = buf.cell_mut((area.x + x as u16, area.y + row as u16)) {
                    cell.set_symbol("▀");
                    cell.set_style(Style::default().fg(top).bg(bottom));
                }
            }
        }
        for (x, y, text, style) in labels {
            if y < area.height {
                buf.set_string(area.x + x, area.y + y, text, style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_drive_what_each_agent_does() {
        let mut o = Office::default();
        o.apply(&AgentEvent::BusyStateChanged {
            state: BusyState::Thinking,
        });
        assert_eq!(o.agents[0].activity, Activity::Thinking);
        o.apply(&AgentEvent::ToolCallStarted {
            call_id: "c".into(),
            tool: "edit_file".into(),
            input: json!({"path": "src/auth.rs"}),
        });
        assert_eq!(o.agents[0].activity, Activity::Typing);
        assert_eq!(o.agents[0].bubble, "Edit auth.rs");
        o.apply(&AgentEvent::ApprovalRequested {
            call_id: "c".into(),
            tool: "edit_file".into(),
            description: String::new(),
        });
        assert_eq!(o.agents[0].activity, Activity::WaitingApproval);

        o.apply(&AgentEvent::ChildSessionSpawned {
            parent_session_id: "p".into(),
            child_session_id: "kid".into(),
            task: "find the config loader".into(),
            workspace: "/w".into(),
            branch: None,
        });
        assert_eq!(o.agents.len(), 2);
        o.apply(&AgentEvent::ChildSessionActivity {
            child_session_id: "kid".into(),
            phase: "execute_bash".into(),
            detail: "cargo test".into(),
        });
        assert_eq!(o.agents[1].activity, Activity::Shell);
        o.apply(&AgentEvent::ChildSessionCompleted {
            parent_session_id: "p".into(),
            child_session_id: "kid".into(),
            status: "completed".into(),
        });
        assert_eq!(o.agents[1].activity, Activity::Done);
        assert!(o.agents[1].finished.is_some());
    }

    #[test]
    fn people_join_talk_and_leave() {
        let mut o = Office::default();
        o.set_owner("dhanush");
        o.apply(&AgentEvent::PersonJoined {
            person: "p1".into(),
            name: "Priya".into(),
            role: "join".into(),
        });
        o.apply(&AgentEvent::PersonJoined {
            person: "w1".into(),
            name: "Ravi".into(),
            role: "watch".into(),
        });
        // Owner first, then the agent, then who joined, then who watches.
        let order: Vec<String> = o
            .stations()
            .iter()
            .map(|w| match w {
                Who::Person(p) => p.name.clone(),
                Who::Agent(a) => format!("agent:{}", a.id),
            })
            .collect();
        assert_eq!(order, ["dhanush", "agent:main", "Priya", "Ravi"]);
        o.apply(&AgentEvent::PersonSaid {
            person: "p1".into(),
            name: "Priya".into(),
            text: "try staging".into(),
        });
        let p = o.people.iter().find(|p| p.id == "p1").unwrap();
        assert!(p.bubble().contains("try staging"));
        // The same person joining twice stays one person.
        o.apply(&AgentEvent::PersonJoined {
            person: "p1".into(),
            name: "Priya".into(),
            role: "join".into(),
        });
        assert_eq!(o.people.len(), 3);
        o.apply(&AgentEvent::PersonLeft {
            person: "p1".into(),
            name: "Priya".into(),
        });
        let p = o.people.iter().find(|p| p.id == "p1").unwrap();
        assert!(p.leaving.is_some());
        assert!(p.bubble().contains("bye"));
    }

    #[test]
    fn renders_half_blocks_bubbles_and_names() {
        let mut o = Office::default();
        o.apply(&AgentEvent::ToolCallStarted {
            call_id: "c".into(),
            tool: "execute_bash".into(),
            input: json!({"command": "cargo test"}),
        });
        let area = Rect::new(0, 0, 60, office_height(60, 1));
        let mut buf = Buffer::empty(area);
        OfficeView {
            office: &o,
            tick: 3,
            selected: None,
        }
        .render(area, &mut buf);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("$ cargo test"), "{text}");
        assert!(text.contains("dcode-ai"));
        assert!(text.contains('▀'));
    }
}

/// Preview: renders scenes to `target/office-preview.json` (cell symbols and
/// colours) for `scripts/cells_to_png.py`. Run with
/// `cargo test -p dcode-ai-cli --lib office_preview -- --ignored`.
#[cfg(test)]
mod preview {
    use super::*;
    use serde_json::json;

    fn color(c: Color) -> serde_json::Value {
        match c {
            Color::Rgb(r, g, b) => json!([r, g, b]),
            _ => json!([20, 20, 24]),
        }
    }

    #[test]
    #[ignore]
    fn office_preview() {
        let mut o = Office::default();
        o.apply(&AgentEvent::ToolCallStarted {
            call_id: "c".into(),
            tool: "edit_file".into(),
            input: serde_json::json!({"path": "src/auth.rs"}),
        });
        for (i, (task, phase, detail)) in [
            ("explore config", "read_file", "Read config.rs"),
            ("run the tests", "execute_bash", "$ cargo test"),
            ("check the page", "browser", "Browser: click"),
            ("verify changes", "provider_request", ""),
        ]
        .iter()
        .enumerate()
        {
            let id = format!("kid{i}");
            o.apply(&AgentEvent::ChildSessionSpawned {
                parent_session_id: "p".into(),
                child_session_id: id.clone(),
                task: task.to_string(),
                workspace: "/w".into(),
                branch: None,
            });
            o.apply(&AgentEvent::ChildSessionActivity {
                child_session_id: id,
                phase: phase.to_string(),
                detail: detail.to_string(),
            });
        }
        let mut waiting = o.agents.clone();
        waiting[0].activity = Activity::WaitingApproval;
        waiting[0].bubble = "? edit_file".into();
        let scenes = [(o.agents.clone(), 2u64, 0usize), (waiting, 5, 1)];
        let mut out = Vec::new();
        for (agents, tick, sel) in scenes {
            let mut office = Office {
                agents,
                people: Vec::new(),
            };
            office.set_owner("dhanush");
            office.apply(&AgentEvent::PersonJoined {
                person: "j1".into(),
                name: "Priya".into(),
                role: "join".into(),
            });
            office.apply(&AgentEvent::PersonSaid {
                person: "j1".into(),
                name: "Priya".into(),
                text: "try staging".into(),
            });
            office.apply(&AgentEvent::PersonJoined {
                person: "w1".into(),
                name: "Ravi".into(),
                role: "watch".into(),
            });
            let area = Rect::new(0, 0, 120, office_height(120, office.stations().len()));
            let mut buf = Buffer::empty(area);
            OfficeView {
                office: &office,
                tick,
                selected: Some(sel),
            }
            .render(area, &mut buf);
            let rows: Vec<Vec<serde_json::Value>> = (0..area.height)
                .map(|y| {
                    (0..area.width)
                        .map(|x| {
                            let c = &buf[(x, y)];
                            json!([c.symbol(), color(c.fg), color(c.bg)])
                        })
                        .collect()
                })
                .collect();
            out.push(rows);
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/office-preview.json");
        std::fs::write(path, serde_json::to_string(&out).unwrap()).unwrap();
    }
}
