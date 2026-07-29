# dcode-ai Architecture Flow Diagram

## High-Level Architecture

```mermaid
graph TB
    subgraph CLI["crates/cli — Terminal UX"]
        REPL[REPL / TUI]
        WEB[Web Server :8642]
        PROMPT[Prompt Handler]
        STREAM[Stream Renderer]
        SLASH[Slash Commands]
        APPROVAL_UI[Approval Prompts]
        IPC_CLIENT[IPC Client]
    end

    subgraph Runtime["crates/runtime — Session Lifecycle"]
        SUPERVISOR[Supervisor]
        SERVICE[Service Session]
        IPC_SERVER[IPC Server<br/>Unix Socket]
        SESSION_STORE[Session Store<br/>JSON + JSONL]
        WORKTREE[Git Worktree Manager]
        PTY[PTY / Process]
        BASH[Bash Tool]
        CTX_MGR[Context Manager]
        TOKEN[Token Counter]
        MEMORY[Memory Store]
    end

    subgraph Core["crates/core — Agent Logic"]
        AGENT[AgentLoop]
        PROVIDER[Provider Trait]
        TOOLS[Tool Registry]
        APPROVAL[Approval Policy]
        COST[Cost Tracker]
        HOOKS[Hook Runner]
        UNDO[Undo Manager]
        SKILLS[Skill Catalog]
    end

    subgraph Providers["LLM Providers"]
        OPENAI[OpenAI]
        ANTHROPIC[Anthropic]
        MINIMAX[MiniMax]
        OPENROUTER[OpenRouter]
        ANTIGRAVITY[Antigravity]
        CLAUDE_CLI[Claude CLI]
    end

    subgraph Common["crates/common — Shared Types"]
        CONFIG[Config]
        EVENTS[AgentEvent Bus]
        MESSAGES[Message Types]
        TOOLS_DEF[Tool Definitions]
        SESSION_META[Session Meta]
        AUTH[Auth / Credentials]
    end

    subgraph AutoResearch["crates/autoresearch — Research"]
        EXP[Experiment Runner]
        LOOP[Research Loop]
        METRIC[Metric Parser]
    end

    %% CLI to Runtime
    REPL --> IPC_CLIENT
    WEB --> IPC_CLIENT
    IPC_CLIENT -->|"Unix Socket<br/>Newline-Delimited JSON"| IPC_SERVER
    IPC_SERVER --> SUPERVISOR

    %% Runtime internal
    SUPERVISOR --> SERVICE
    SERVICE --> AGENT
    SERVICE --> SESSION_STORE
    SERVICE --> WORKTREE
    SUPERVISOR --> CTX_MGR
    CTX_MGR --> TOKEN

    %% Core internal
    AGENT --> PROVIDER
    AGENT --> TOOLS
    AGENT --> APPROVAL
    AGENT --> COST
    AGENT --> HOOKS
    AGENT --> UNDO

    %% Provider connections
    PROVIDER --> OPENAI
    PROVIDER --> ANTHROPIC
    PROVIDER --> MINIMAX
    PROVIDER --> OPENROUTER
    PROVIDER --> ANTIGRAVITY
    PROVIDER --> CLAUDE_CLI

    %% Tool connections
    TOOLS --> BASH
    TOOLS --> PTY
    TOOLS --> MEMORY

    %% Approval UI
    APPROVAL -->|"Approval Request"| APPROVAL_UI
    APPROVAL_UI -->|"Verdict"| APPROVAL

    %% Common is used by all
    CLI -.-> Common
    Runtime -.-> Common
    Core -.-> Common
    AutoResearch -.-> Common

    style CLI fill:#e1f5fe
    style Runtime fill:#f3e5f5
    style Core fill:#e8f5e9
    style Common fill:#fff3e0
    style Providers fill:#fce4ec
    style AutoResearch fill:#f1f8e9
```

## Agent Loop — Single Turn Flow

```mermaid
flowchart TD
    A[User Input + Attachments] --> B[Build Message<br/>Text + Image Parts]
    B --> C[Push to Message History]
    C --> D[Emit MessageReceived Event]
    D --> E[Prepare Messages<br/>provider.preprocess]

    E --> F{Stream from Provider}
    F -->|TextDelta| G[Accumulate Assistant Text]
    F -->|InternalDelta| H[Emit ThinkingDelta]
    F -->|ToolUse| I[Collect Tool Calls]
    F -->|Usage| J[Update Cost Tracker]
    F -->|Error| K[Emit Error + Abort Turn]
    F -->|Done| L[End Stream]

    G --> M{Has Tool Calls?}
    L --> M
    I --> M

    M -->|No + Empty Text| N{Retry? < 3}
    N -->|Yes| F
    N -->|No| O[Fail: Empty Completion]

    M -->|No + Has Text| P[Push Assistant Message]
    P --> Q[Emit MessageReceived]
    Q --> R[Return Final Text]

    M -->|Yes| S[Phase 1: Permission Check<br/>Sequential]
    S --> T{Permission Tier}
    T -->|AutoApprove| U[Queue for Execution]
    T -->|Ask| V[Emit ApprovalRequested]
    V --> W[Wait for Verdict via IPC]
    W --> X{Approved?}
    X -->|Yes| U
    X -->|No| Y[Denied ToolResult]
    T -->|Denied| Z[Denied ToolResult]

    U --> AA[Phase 2: Concurrent Execution<br/>FuturesUnordered]
    AA --> AB[Tool Registry.execute]
    AB --> AC[Tool Result]

    Y --> AD[Push Tool Results]
    Z --> AD
    AC --> AD

    AD --> AE[Emit ToolCallCompleted]
    AE --> AF[Push Tool Result Messages]
    AF --> AG{Turn Limit?}
    AG -->|No| E
    AG -->|Yes| AH[Error: Turn Budget Exceeded]

    style A fill:#bbdefb
    style R fill:#c8e6c9
    style O fill:#ffcdd2
    style AH fill:#ffcdd2
    style K fill:#ffcdd2
    style AA fill:#fff9c4
```

## Session Lifecycle

```mermaid
stateDiagram-v2
    [*] --> Created : Supervisor::create() or ::resume()

    Created --> Running : run_turn_with_images()
    Running --> Running : Turn completes, await next prompt
    Running --> Paused : Cancel / User Exit (turn aborted)
    Running --> Failed : Error (provider, tool, budget)

    Paused --> Running : New prompt arrives
    Paused --> Resumed : Supervisor::resume()

    Resumed --> Running : run_turn_with_images()

    Failed --> [*]
    Paused --> Finished : Shutdown / User Exit
    Running --> Finished : Session End
    Resumed --> Finished : Session End

    Finished --> [*] : persist state + close event log
```

## IPC Communication Flow

```mermaid
sequenceDiagram
    participant CLI as CLI (cli crate)
    participant Socket as Unix Socket
    participant Runtime as Runtime (supervisor)
    participant Agent as AgentLoop
    participant Provider as LLM Provider

    CLI->>Socket: Connect to $XDG_RUNTIME_DIR/dcode-ai/<id>.sock
    CLI->>Socket: Send prompt (newline-delimited JSON)

    Socket->>Runtime: Command: RunPrompt
    Runtime->>Agent: run_turn(prompt, attachments)
    Agent->>Provider: chat(messages, tools, model)

    loop Streaming
        Provider-->>Agent: StreamChunk (TextDelta / ToolUse / Usage)
        Agent-->>Runtime: AgentEvent (via channel)
        Runtime-->>Socket: EventEnvelope (JSON line)
        Socket-->>CLI: Render event (TUI / stream)
    end

    alt Approval Required
        Agent-->>Runtime: AgentEvent::ApprovalRequested
        Runtime-->>Socket: Approval request
        Socket-->>CLI: Show approval prompt
        CLI-->>Socket: Verdict (approve/deny)
        Socket-->>Runtime: Approval response
        Runtime-->>Agent: ApprovalVerdict
    end

    Agent-->>Runtime: Turn complete
    Runtime-->>Socket: AgentEvent::BusyStateChanged(Idle)
    Socket-->>CLI: Show idle state
```

## Tool Execution Pipeline

```mermaid
flowchart LR
    A[ToolCall from LLM] --> B[Permission Check]
    B -->|Tier: AutoApprove| C[Execute Immediately]
    B -->|Tier: Ask| D[IPC Approval Flow]
    B -->|Tier: Denied| E[Return Denied Result]

    D -->|Approved| C
    D -->|Denied| E

    C --> F{Tool Type}
    F -->|read_file| G[Read from Disk]
    F -->|write_file| H[Undo Checkpoint → Write]
    F -->|edit_file| I[Undo Checkpoint → Edit]
    F -->|execute_bash| J[Sandbox → Spawn Process]
    F -->|interactive_exec| K[PTY Allocation]
    F -->|search_code| L[Ripgrep Search]
    F -->|query_symbols| M[Code Intelligence]
    F -->|list_directory| N[fs::read_dir]
    F -->|git_status| O[Git Command]
    F -->|web_search| P[HTTP Request]
    F -->|fetch_url| Q[HTTP Fetch + Normalize]

    G --> R[ToolResult]
    H --> R
    I --> R
    J --> R
    K --> R
    L --> R
    M --> R
    N --> R
    O --> R
    P --> R
    Q --> R

    R --> S[Cap Output if > 100KB]
    S --> T[Return to AgentLoop]

    style D fill:#fff9c4
    style H fill:#c8e6c9
    style I fill:#c8e6c9
    style J fill:#ffcdd2
```

## Data Flow Summary

```mermaid
graph LR
    subgraph Input
        U[User] -->|Prompt| CLI
        U -->|Image Paste| CLI
        U -->|Slash Command| CLI
    end

    subgraph Processing
        CLI -->|IPC| RT[Runtime]
        RT --> AG[Agent Loop]
        AG <-->|Chat API| PR[Provider]
        AG --> TL[Tool Execution]
        AG --> AP[Approval Flow]
    end

    subgraph Persistence
        RT --> SS[Session State<br/>.json]
        RT --> EL[Event Log<br/>.events.jsonl]
        RT --> WT[Git Worktree]
        RT --> MS[Memory Store]
    end

    subgraph Output
        AG -->|Events| RT
        RT -->|IPC| CLI
        CLI -->|Render| U
        RT -->|Webhooks| WH[External]
        RT -->|Web UI| WEB[Browser]
    end

    style Input fill:#e3f2fd
    style Processing fill:#e8f5e9
    style Persistence fill:#fff8e1
    style Output fill:#fce4ec
```

## Crate Dependency Graph

```mermaid
graph BT
    CLI[crates/cli<br/>Terminal UX] --> RT[crates/runtime<br/>Session Lifecycle]
    CLI --> CORE[crates/core<br/>Agent Logic]
    CLI --> COMMON[crates/common<br/>Shared Types]

    RT --> CORE
    RT --> COMMON

    CORE --> COMMON

    AR[crates/autoresearch<br/>Research Loop] --> CORE
    AR --> COMMON

    style CLI fill:#bbdefb
    style RT fill:#ce93d8
    style CORE fill:#81c784
    style COMMON fill:#ffb74d
    style AR fill:#aed581
```
