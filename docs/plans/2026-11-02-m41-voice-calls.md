# M41 — Talk with the people on a session (voice calls)

Status: phase 1 implemented (v1.0.40)

Like a Teams call, inside dcode-ai: the host and the guests who joined with
`/invite` talk to each other while the agent works. Rust only, no browser,
no outside service, and it stays a single `dcode-ai` binary.

## What it looks like

- `/call` starts a call (host) or joins the running one (guest). Everyone on
  the session sees "📞 Dhanush started a call — /call to join".
- While in a call:
  - The footer shows `🎙 call · 3 · 02:14` (`🔇` when muted).
  - In the office, whoever is speaking gets sound waves and a moving mouth;
    muted people get a small 🔇.
  - `/people` shows who is in the call.
- `/mute` toggles your microphone; `/hangup` leaves. The call ends when the
  last person leaves, or with `/invite stop` or the session.
- `/call devices` lists microphones and speakers; `/call device <n>` picks
  one (remembered in the config).
- Headphones are recommended; the first `/call` says so. Without echo
  cancellation, speakers feed back into the microphone (see phase 2).

## Privacy

- The microphone is open only while you're in a call, and the footer always
  shows it.
- Nothing is recorded or logged: audio never goes to the event log, the
  agent, `/watch` or any file. Only "joined the call" / "left the call"
  lines are logged.
- Audio travels only over the invite's encrypted connection (Noise, same
  key), host ↔ guest, on your network or VPN. Nothing goes through an
  outside service.

## How it works

### Audio in and out, without breaking the single binary

Linking an audio library on Linux would stop dcode-ai from starting on
servers without one. So:

- **macOS and Windows**: `cpal` (CoreAudio / WASAPI, always present).
- **Linux and WSL**: run the system's audio tools as child processes:
  `pw-record`/`pw-play` (PipeWire), then `parec`/`pacat` (PulseAudio — this
  includes WSLg on Windows 11), then `arecord`/`aplay` (ALSA). Raw PCM
  goes through pipes. If none exists, `/call` says what to install.
  dcode-ai itself links no audio library.

Format everywhere: 16 kHz, mono, 16-bit, 20 ms frames (320 samples).

### Codec

- Phase 1: **IMA ADPCM** (pure Rust, about 30 lines; 4:1 → 64 kbit/s per
  speaker). Plenty for a LAN or VPN, with no C dependency.
- Phase 2: Opus behind a cargo feature (about 24 kbit/s), for slow links
  and relay mode.

### Sending: the host forwards, each person mixes

- Each person sends their microphone frames to the host. The host forwards
  every frame to everyone else, tagged with the speaker. Each person mixes
  what they hear.
- No mixing on the host: the cost stays low, and each person can turn one
  speaker down (`/call volume Priya 50`).
- A simple voice-activity gate (energy plus hangover) stops sending
  silence. It also drives the office's "speaking" waves.
- Phase 1 transport: new message types on the existing invite connection
  (`{"t":"audio", seq, frame}`, and `call_join` / `call_leave` / `mute`).
  It's TCP, so the same port and firewall rule apply, and on a LAN the
  delay is about 60–100 ms.
- A jitter buffer per speaker (60 ms target, grows to 200 ms when frames
  arrive unevenly; late frames are dropped, gaps filled with silence).

### Who can be in a call

- The host and guests joined over `/invite` in a terminal.
- Not browser guests (`/invite web`): browsers allow the microphone only
  over HTTPS or on localhost, and that page is plain HTTP. They'll see who
  is talking.
- Not local `dcode-ai join` terminals in phase 1; phase 2 can add them over
  the session's IPC socket.

## New pieces

- `crates/cli/src/voice/`:
  - `audio.rs`: capture and playback (cpal, or the child-process backends)
  - `adpcm.rs`
  - `jitter.rs`
  - `mixer.rs`
  - `vad.rs`
  - `call.rs`: the call state, who is in, mute, levels
- Invite protocol: the audio and call messages; the host forwards them.
- Events (logged, no audio): `CallStarted`, `CallJoined {person}`,
  `CallLeft {person}`, `CallEnded`. Speaking and mute change often, so they
  stay local to each TUI and are not logged.
- TUI:
  - `/call`, `/mute`, `/hangup`
  - footer call status
  - office speaking animation
  - a "📞 call" line in `/people`
  - the dashboard team tab lists who is in the call

## Tests

- ADPCM round trip (error bound on a sine and on speech-like noise).
- Jitter buffer: frames out of order, late, or missing → steady 20 ms
  output with silence for gaps.
- Mixer: two speakers sum without clipping; per-speaker volume.
- Voice-activity gate: silence isn't sent; speech opens it with a short
  hangover.
- Host forwarding, end to end in-process with fake devices (a sine
  generator in, a recorder out): A's tone reaches B and C, never back to A;
  mute stops it; leaving the call stops forwarding.
- PTY:
  - `/call` on the host and the guest shows the footer and the people in
    the call;
  - `/mute` shows 🔇;
  - `/hangup` leaves.
  - Uses a fake audio backend selected by an env var, so no sound card is
    needed in CI.

## Phases

1. **Calls on a LAN or VPN**: everything above. TCP on the invite
   connection, ADPCM, Linux through system tools, macOS and Windows
   through cpal.
2. **Better sound**:
   - Opus; UDP with the same keys (lower delay on bad Wi-Fi);
   - push-to-talk (hold a key in terminals that report key release, such as
     kitty, WezTerm, Ghostty and Windows Terminal; a toggle elsewhere);
   - simple echo suppression (duck the mic while others speak);
   - local `dcode-ai join` terminals in calls.
3. **Optional, later**: "call notes": local speech-to-text of the call
   (opt-in, everyone told), summarised into the shared to-do list.

## Risks

- **Echo** without headphones until phase 2's ducking; true echo
  cancellation would need a C++ library.
- **WSL**: sound goes through WSLg's PulseAudio (Windows 11). Older WSL
  without WSLg: use the Windows binary for calls.
- **The mic permission prompt on macOS** shows the terminal app's name (it
  asks for the terminal, not dcode-ai).
