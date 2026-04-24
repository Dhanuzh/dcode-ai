# Context Management & Auto-Summarize Feature

## Overview

This feature implements intelligent context management to prevent token overflow and hallucinations caused by excessive context window usage. When the conversation grows large, the system automatically summarizes older messages to maintain context continuity.

## Key Components

### 1. Context Manager (`crates/runtime/src/context_manager.rs`)

**Purpose:** Tracks context size, generates statistics, and handles message compaction.

**Features:**
- Token estimation using character-based approximation
- Context statistics tracking (usage %, message count)
- Sliding window for recent messages
- System message preservation
- Summary generation prompts

**Configuration Options:**
```toml
[memory.context]
# Target context window size (approximate tokens)
context_window_target = 32000

# Maximum messages to retain after compaction  
max_retained_messages = 50

# Percentage of context window that triggers auto-summarize (0-100)
auto_summarize_threshold = 75

# Enable automatic context summarization
enable_auto_summarize = true
```

### 2. Auto-Summarize Integration (`crates/runtime/src/supervisor.rs`)

**Purpose:** Integrates context management into the session lifecycle.

**Flow:**
1. Before each `run_turn`: Check if context needs attention
2. After each `run_turn`: Check if summarization should trigger
3. If threshold exceeded: Call AI to generate summary
4. Apply summary: Replace old messages with concise summary
5. Emit events: `ContextWarning`, `ContextCompaction`

**Events Emitted:**
- `ContextWarning`: When context reaches 80% of target
- `ContextCompaction`: During summarization phases (starting/completed)

### 3. Configuration Schema (`crates/common/src/config.rs`)

**New `ContextConfig`:**
```rust
pub struct ContextConfig {
    pub context_window_target: usize,     // Default: 32000
    pub max_retained_messages: usize,     // Default: 50
    pub auto_summarize_threshold: u8,     // Default: 75
    pub enable_auto_summarize: bool,      // Default: true
}
```

**Nested under `memory`:**
```toml
[memory]
file_path = ".dcode-ai/memory.json"
max_notes = 128
auto_compact_on_finish = false

[memory.context]
context_window_target = 32000
max_retained_messages = 50
auto_summarize_threshold = 75
enable_auto_summarize = true
```

## How It Works

### Token Estimation
```rust
// Rough approximation: tokens ≈ characters / 4
// Tool messages: more token-dense (3.5 divisor)
// System messages: standard (4.0 divisor)
// + 10 base overhead + ~50 per tool call
```

### Compaction Strategy
1. **Preserve System Messages**: Always keep at start
2. **Sliding Window**: Keep last N messages (configurable)
3. **Summarize Middle**: Old messages get summarized by AI
4. **Insert Summary**: Summary inserted as system message with special header

### Summary Format
```
## Conversation Summary (Earlier Context)

[AI-generated concise summary covering:]
- Key topics and goals discussed
- Important decisions or findings
- Critical context (file paths, variable names, errors)
```

## Usage Examples

### Default Behavior
Simply start a session - context management is enabled by default with sensible defaults.

### Custom Thresholds
For very long conversations, increase thresholds:
```toml
[memory.context]
context_window_target = 64000  # For 128k context models
auto_summarize_threshold = 80
max_retained_messages = 100
```

### Disable for Short Sessions
```toml
[memory.context]
enable_auto_summarize = false
```

## Benefits

1. **Prevents Token Overflow**: Automatic compaction before hitting limits
2. **Reduces Hallucinations**: Smaller, focused context = more accurate responses
3. **Context Continuity**: Important context preserved via summarization
4. **Cost Efficiency**: Uses fewer tokens per request
5. **Transparency**: Events emitted for UI feedback

## Testing

Run tests:
```bash
cargo test -p dcode-ai-runtime context_manager
```

Test coverage:
- Token estimation
- Context statistics
- Sliding window behavior
- Summary application

## Future Improvements

1. **Hierarchical Summaries**: Multiple levels of summarization
2. **Importance Scoring**: Preserve critical messages
3. **Tool Call Preservation**: Keep summaries of tool executions
4. **Async Summarization**: Background summarization without blocking
5. **Model-Specific Tuning**: Adjust based on provider context limits
