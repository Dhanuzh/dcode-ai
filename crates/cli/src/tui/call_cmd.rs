//! `/call` settings while in a call, the same for the host and guests:
//! `/call` (who's in it), `/call ptt`, `/call volume <name> <0-200>`,
//! `/call echo on|off`.

use crate::tui::state::TuiSessionState;

/// Runs a `/call …` setting; the lines to show. `None` when not in a call.
pub fn run(args: &str, g: &mut TuiSessionState) -> Option<Vec<String>> {
    let call = g.office.voice.clone()?;
    let words: Vec<&str> = args.split_whitespace().collect();
    let name_of = |g: &TuiSessionState, id: &str| {
        g.office
            .people
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| id.to_string())
    };
    Some(match words.as_slice() {
        [] => {
            let mut out = vec![format!(
                "[call] {} in the call · {} · {}{}",
                g.office.in_call.len().max(1),
                crate::voice::clock(call.elapsed()),
                if call.muted() { "muted" } else { "mic on" },
                if call.ptt() {
                    " · push-to-talk (hold ctrl+space)"
                } else {
                    ""
                },
            )];
            let mut ids: Vec<&String> = g.office.in_call.iter().collect();
            ids.sort();
            for id in ids {
                let me = id == call.me();
                let vol = (call.volume(id) * 100.0).round() as u32;
                out.push(format!(
                    "[call]   {}{}{}",
                    name_of(g, id),
                    if me { " (you)" } else { "" },
                    if !me && vol != 100 {
                        format!(" · volume {vol}")
                    } else {
                        String::new()
                    }
                ));
            }
            out.push("[call] /mute · /call ptt · /call volume <name> <0-200> · /call echo on|off · /hangup".into());
            out
        }
        ["ptt"] | ["push-to-talk"] => {
            let on = !call.ptt();
            call.set_ptt(on);
            vec![if on {
                "[call] push-to-talk on — hold Ctrl+Space to talk (/call ptt again to turn it off)"
                    .into()
            } else {
                "[call] push-to-talk off — your mic is open (unless muted)".into()
            }]
        }
        ["echo", v @ ("on" | "off")] => {
            call.set_echo_gate(*v == "on");
            vec![if *v == "on" {
                "[call] echo reduction on (your mic doesn't send what your speakers just played)"
                    .into()
            } else {
                "[call] echo reduction off — use headphones".into()
            }]
        }
        ["volume", rest @ ..] if rest.len() >= 2 => {
            let level = rest[rest.len() - 1].trim_end_matches('%');
            let who = rest[..rest.len() - 1].join(" ").to_lowercase();
            let Ok(level) = level.parse::<u32>() else {
                return Some(vec!["usage: /call volume <name> <0-200>".into()]);
            };
            let id = g
                .office
                .people
                .iter()
                .find(|p| p.name.to_lowercase() == who && g.office.in_call.contains(&p.id))
                .map(|p| (p.id.clone(), p.name.clone()));
            match id {
                Some((id, name)) => {
                    call.set_volume(&id, level.min(200) as f32 / 100.0);
                    vec![format!("[call] {name} at {}% for you", level.min(200))]
                }
                None => vec![format!(
                    "[call] nobody called {who} is in the call — /call lists them"
                )],
            }
        }
        _ => vec![
            "[call] /call · /call ptt · /call volume <name> <0-200> · /call echo on|off".into(),
        ],
    })
}
