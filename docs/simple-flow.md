# dcode-ai — Simple Flow Diagram

## How it Works (Big Picture)

```
┌─────────────┐         ┌──────────────┐         ┌─────────────┐
│             │  prompt  │              │  chat   │             │
│   USER      │ ──────►  │  CLI (TUI)   │ ──────► │  AGENT LOOP │
│             │ ◄──────  │              │ ◄──────  │             │
└─────────────┘  reply   └──────┬───────┘  stream  └──────┬──────┘
                                │                         │
                          Unix Socket               calls tools
                                │                    & providers
                                ▼                         │
                         ┌──────────────┐                 │
                         │              │  ◄───────────────┘
                         │  RUNTIME     │
                         │  (Supervisor)│
                         │              │
                         └──────┬───────┘
                                │
                    ┌───────────┼───────────┐
                    ▼           ▼           ▼
              ┌──────────┐ ┌────────┐ ┌──────────┐
              │ Session  │ │  Git   │ │  Memory  │
              │  Store   │ │Worktree│ │  Store   │
              └──────────┘ └────────┘ └──────────┘
```

## One Turn (What Happens When You Type Something)

```
  You type a message
       │
       ▼
  ┌─────────────┐
  │ Send to LLM │  ───► OpenAI / Anthropic / MiniMax / etc.
  └──────┬──────┘
         │
         ▼
  ┌─────────────┐     ┌──────────────┐
  │ LLM replies │────►│  Has tools?  │
  └─────────────┘     └──────┬───────┘
                             │
                    ┌────────┴────────┐
                    ▼                 ▼
                   YES                NO
                    │                 │
                    ▼                 ▼
          ┌──────────────┐    ┌───────────┐
          │ Run the tool │    │ Show text │
          │ (read file,  │    │ to user   │
          │  edit, bash) │    └───────────┘
          └──────┬───────┘
                 │
                 ▼
          ┌──────────────┐
          │ Send result  │
          │ back to LLM  │──── (loop until no more tools)
          └──────────────┘
```

## The 5 Crates (What Each Part Does)

```
┌─────────────────────────────────────────────────────┐
│                    dcode-ai                         │
│                                                     │
│  ┌─────────┐  You see this (terminal, prompts)      │
│  │   CLI   │─────────────────────────────┐          │
│  └─────────┘                             │          │
│                                    Unix Socket      │
│  ┌─────────┐  Manages sessions,           │          │
│  │ RUNTIME │◄────────────────────────────┘          │
│  └────┬────┘                                       │
│       │                                             │
│  ┌────▼────┐  The brain: agent loop, tools,         │
│  │   CORE  │  providers, approvals                   │
│  └────┬────┘                                       │
│       │                                             │
│  ┌────▼────┐  Shared types used by everyone          │
│  │ COMMON  │                                        │
│  └─────────┘                                        │
│                                                     │
│  ┌─────────┐  Autonomous research experiments       │
│  │AUTORESCH│  (optional)                             │
│  └─────────┘                                        │
└─────────────────────────────────────────────────────┘
```

## Providers (Which AI Models It Can Use)

```
            dcode-ai
               │
       ┌───────┼───────┬───────────┬──────────┐
       ▼       ▼       ▼           ▼          ▼
   ┌───────┐┌──────┐┌────────┐┌────────┐┌────────┐
   │OpenAI ││Anthro││ MiniMax││OpenRout││Antigrav│
   │(GPT)  ││(Claude)││(M2.5) ││(router)││ (AG)   │
   └───────┘└──────┘└────────┘└────────┘└────────┘
```

## Tools (What the AI Can Do)

```
  AI says "use a tool"
         │
         ├──► read_file      → Read a file
         ├──► write_file     → Create/overwrite a file
         ├──► edit_file      → Change part of a file
         ├──► execute_bash   → Run a shell command
         ├──► search_code    → Search with ripgrep
         ├──► list_directory → List folder contents
         ├──► git_status     → Show git status
         ├──► git_diff       → Show git diff
         ├──► web_search     → Search the internet
         ├──► fetch_url      → Read a webpage
         ├──► query_symbols  → Code intelligence
         ├──► save_memory    → Remember something
         ├──► spawn_subagent → Start a child agent
         └──► run_validation → Run tests/builds
```

## Permission System

```
  Tool request from AI
         │
         ▼
  ┌──────────────┐
  │ Check policy  │
  └──────┬───────┘
         │
    ┌────┴────┬──────────┐
    ▼         ▼          ▼
  ALLOW     ASK        DENY
  (auto)    (user)     (blocked)
    │         │          │
    ▼         ▼          ▼
  Run it   Show prompt  Return error
           to user      "denied"
```
