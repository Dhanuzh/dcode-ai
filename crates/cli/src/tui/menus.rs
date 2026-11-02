//! Pick-from-a-list menus instead of typed settings: the call (mute,
//! push-to-talk, echo, volume per person, hang up), `/kick`, ticking off a
//! shared to-do. The same for the host and for guests.

use crate::tui::choice::{ChoiceAction, ChoicePrompt};
use crate::tui::state::TuiSessionState;

#[derive(Debug, Clone, PartialEq)]
pub enum Menu {
    /// Call settings; each option's meaning.
    Call(Vec<CallItem>),
    /// Whose volume to change: (person id, name).
    CallVolume(Vec<(String, String)>),
    /// How loud: for this person.
    CallVolumeLevel { person: String, name: String },
    /// Remove which guest (names).
    Kick(Vec<String>),
    /// Tick off which to-do (ids).
    TodoDone(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CallItem {
    Mute,
    PushToTalk,
    Echo,
    Volume,
    HangUp,
    Close,
}

/// What the caller still has to do after a choice.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuEffect {
    None,
    HangUp,
    Kick(String),
    TodoDone(String),
    /// Show this line.
    Say(String),
}

const LEVELS: [u32; 6] = [25, 50, 75, 100, 150, 200];

fn prompt(title: &str, body: Vec<String>, options: Vec<String>, menu: Menu) -> ChoicePrompt {
    let close = options.len().saturating_sub(1);
    ChoicePrompt {
        title: title.into(),
        body,
        options,
        index: 0,
        esc_index: close,
        hint: Some("↑/↓ Enter · Esc close".into()),
        action: ChoiceAction::Menu(menu),
    }
}

fn name_of(g: &TuiSessionState, id: &str) -> String {
    g.office
        .people
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| id.to_string())
}

/// The call menu (None when not in a call).
pub fn call_menu(g: &TuiSessionState) -> Option<ChoicePrompt> {
    let call = g.office.voice.as_ref()?;
    let mut ids: Vec<&String> = g.office.in_call.iter().collect();
    ids.sort();
    let who: Vec<String> = ids
        .iter()
        .map(|id| {
            let me = id.as_str() == call.me();
            let vol = (call.volume(id) * 100.0).round() as u32;
            format!(
                "  {}{}{}",
                name_of(g, id),
                if me { " (you)" } else { "" },
                if !me && vol != 100 {
                    format!(" · {vol}%")
                } else {
                    String::new()
                }
            )
        })
        .collect();
    let mut body = vec![format!(
        "{} in the call · {}",
        g.office.in_call.len().max(1),
        crate::voice::clock(call.elapsed())
    )];
    body.extend(who);
    let items = vec![
        CallItem::Mute,
        CallItem::PushToTalk,
        CallItem::Echo,
        CallItem::Volume,
        CallItem::HangUp,
        CallItem::Close,
    ];
    let options = items
        .iter()
        .map(|i| match i {
            CallItem::Mute if call.muted() => "Unmute".to_string(),
            CallItem::Mute => "Mute".into(),
            CallItem::PushToTalk if call.ptt() => {
                "Push-to-talk: on (hold Ctrl+Space) → turn off".into()
            }
            CallItem::PushToTalk => "Push-to-talk: off → turn on".into(),
            CallItem::Echo if call.echo_gate() => {
                "Echo reduction: on → turn off (headphones)".into()
            }
            CallItem::Echo => "Echo reduction: off → turn on".into(),
            CallItem::Volume => "Volume for someone…".into(),
            CallItem::HangUp => "Hang up".into(),
            CallItem::Close => "Close".into(),
        })
        .collect();
    Some(prompt("Call", body, options, Menu::Call(items)))
}

pub fn kick_menu(names: Vec<String>) -> Option<ChoicePrompt> {
    if names.is_empty() {
        return None;
    }
    let mut options: Vec<String> = names.iter().map(|n| format!("Remove {n}")).collect();
    options.push("Close".into());
    Some(prompt(
        "Remove a guest",
        vec!["They're told, and their invite stops working for them.".into()],
        options,
        Menu::Kick(names),
    ))
}

pub fn todo_menu(g: &TuiSessionState) -> Option<ChoicePrompt> {
    let open: Vec<(String, String)> = g
        .team
        .todos
        .iter()
        .enumerate()
        .filter(|(_, t)| t.done_by.is_none())
        .map(|(i, t)| (t.id.clone(), format!("#{} {}", i + 1, t.text)))
        .collect();
    if open.is_empty() {
        return None;
    }
    let mut options: Vec<String> = open.iter().map(|(_, l)| l.clone()).collect();
    options.push("Close".into());
    Some(prompt(
        "Tick off a to-do",
        Vec::new(),
        options,
        Menu::TodoDone(open.into_iter().map(|(id, _)| id).collect()),
    ))
}

/// Carries out a choice; anything the caller must do is returned.
pub fn chosen(menu: Menu, index: usize, g: &mut TuiSessionState) -> MenuEffect {
    match menu {
        Menu::Call(items) => {
            let Some(call) = g.office.voice.clone() else {
                return MenuEffect::None;
            };
            match items.get(index) {
                Some(CallItem::Mute) => {
                    call.set_muted(!call.muted());
                    MenuEffect::Say(
                        if call.muted() {
                            "[call] 🔇 muted"
                        } else {
                            "[call] 🎙 unmuted"
                        }
                        .into(),
                    )
                }
                Some(CallItem::PushToTalk) => {
                    call.set_ptt(!call.ptt());
                    MenuEffect::Say(
                        if call.ptt() {
                            "[call] push-to-talk on — hold Ctrl+Space to talk"
                        } else {
                            "[call] push-to-talk off"
                        }
                        .into(),
                    )
                }
                Some(CallItem::Echo) => {
                    call.set_echo_gate(!call.echo_gate());
                    MenuEffect::Say(
                        if call.echo_gate() {
                            "[call] echo reduction on"
                        } else {
                            "[call] echo reduction off — use headphones"
                        }
                        .into(),
                    )
                }
                Some(CallItem::Volume) => {
                    let mut people: Vec<(String, String)> = g
                        .office
                        .in_call
                        .iter()
                        .filter(|id| id.as_str() != call.me())
                        .map(|id| (id.clone(), name_of(g, id)))
                        .collect();
                    people.sort_by(|a, b| a.1.cmp(&b.1));
                    if people.is_empty() {
                        return MenuEffect::Say("[call] nobody else is in the call yet".into());
                    }
                    let mut options: Vec<String> = people
                        .iter()
                        .map(|(id, n)| format!("{n} · {}%", (call.volume(id) * 100.0).round()))
                        .collect();
                    options.push("Close".into());
                    g.choice_prompts.push_front(prompt(
                        "Whose volume?",
                        vec!["Only what you hear changes.".into()],
                        options,
                        Menu::CallVolume(people),
                    ));
                    MenuEffect::None
                }
                Some(CallItem::HangUp) => MenuEffect::HangUp,
                _ => MenuEffect::None,
            }
        }
        Menu::CallVolume(people) => {
            if let Some((person, name)) = people.get(index).cloned() {
                let now = g
                    .office
                    .voice
                    .as_ref()
                    .map(|c| (c.volume(&person) * 100.0).round() as u32)
                    .unwrap_or(100);
                let mut options: Vec<String> = LEVELS
                    .iter()
                    .map(|l| {
                        if *l == now {
                            format!("{l}% (now)")
                        } else {
                            format!("{l}%")
                        }
                    })
                    .collect();
                options.push("Close".into());
                let mut p = prompt(
                    &format!("Volume for {name}"),
                    Vec::new(),
                    options,
                    Menu::CallVolumeLevel { person, name },
                );
                p.index = LEVELS.iter().position(|l| *l == now).unwrap_or(3);
                g.choice_prompts.push_front(p);
            }
            MenuEffect::None
        }
        Menu::CallVolumeLevel { person, name } => match (LEVELS.get(index), &g.office.voice) {
            (Some(level), Some(call)) => {
                call.set_volume(&person, *level as f32 / 100.0);
                MenuEffect::Say(format!("[call] {name} at {level}% for you"))
            }
            _ => MenuEffect::None,
        },
        Menu::Kick(names) => names
            .get(index)
            .cloned()
            .map(MenuEffect::Kick)
            .unwrap_or(MenuEffect::None),
        Menu::TodoDone(ids) => ids
            .get(index)
            .cloned()
            .map(MenuEffect::TodoDone)
            .unwrap_or(MenuEffect::None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kick_and_todo_menus_pick_from_a_list() {
        let menu = kick_menu(vec!["Priya".into(), "Sam".into()]).unwrap();
        assert_eq!(menu.options, ["Remove Priya", "Remove Sam", "Close"]);
        assert_eq!(menu.esc_index, 2, "Esc closes");
        let ChoiceAction::Menu(m) = menu.action else {
            panic!()
        };
        let mut g = TuiSessionState::new(
            "s".into(),
            "m".into(),
            "a".into(),
            "p".into(),
            std::path::PathBuf::from("."),
            false,
        );
        assert_eq!(chosen(m.clone(), 1, &mut g), MenuEffect::Kick("Sam".into()));
        assert_eq!(chosen(m, 2, &mut g), MenuEffect::None);
        assert!(todo_menu(&g).is_none(), "nothing open");
        g.apply_event(&dcode_ai_common::event::AgentEvent::TeamTodoAdded {
            id: "t1".into(),
            text: "fix login".into(),
            by: "Priya".into(),
        });
        let menu = todo_menu(&g).unwrap();
        assert_eq!(menu.options[0], "#1 fix login");
        let ChoiceAction::Menu(m) = menu.action else {
            panic!()
        };
        assert_eq!(chosen(m, 0, &mut g), MenuEffect::TodoDone("t1".into()));
        assert!(call_menu(&g).is_none(), "no call, no call menu");
    }
}
