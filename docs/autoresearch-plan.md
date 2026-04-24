# dcode-ai-autoresearch: Autonomous Research Loop

## Overview

Integrate **autoresearch** capabilities into `dcode-ai` — allowing AI agents to autonomously run experiments, measure results, and iteratively improve within a fixed time budget. This brings Karpathy's nanoGPT autoresearch concept to the Rust-native dcode-ai ecosystem.

## Key Concepts (from karpathy/autoresearch)

| Concept | Description |
|---------|-------------|
| **Fixed time budget** | Each experiment runs for a fixed duration (e.g., 5 min), making results comparable |
| **Single editable file** | Agent modifies one file per experiment; keeps diffs manageable |
| **Single metric** | `val_bpb` (validation bits per byte) — lower is better, vocab-independent |
| **Keep/Discard loop** | If metric improves → keep commit; if worse → revert |
| **program.md** | Instructions file that defines the research program |
| **Results logging** | TSV file tracking commit, metric, memory, status, description |

## Implementation Plan

### Phase 1: Core Architecture

1. **New crate: `dcode-ai-autoresearch`**
   - Location: `crates/autoresearch/`
   - Workspace member
   - Dependencies: `dcode-ai-common`, `dcode-ai-core`, `tokio`

2. **Core types (`lib.rs`)**
   ```rust
   // Experiment result
   pub struct ExperimentResult {
       pub commit: String,
       pub metric_value: f64,        // val_bpb or custom metric
       pub memory_gb: f64,
       pub training_seconds: f64,
       pub total_seconds: f64,
       pub status: ExperimentStatus, // Keep, Discard, Crash
       pub description: String,
       pub timestamp: DateTime<Utc>,
   }

   // Research program (like program.md)
   pub struct ResearchProgram {
       pub name: String,
       pub description: String,
       pub editable_files: Vec<PathBuf>,  // Files agent can modify
       pub fixed_files: Vec<PathBuf>,      // Read-only files
       pub metric_command: Command,        // How to extract metric
       pub metric_goal: MetricGoal,        // Minimize or Maximize
       pub time_budget_seconds: u64,
       pub extra_constraints: Vec<String>,
   }

   // Auto-research session
   pub struct AutoResearchSession {
       pub program: ResearchProgram,
       pub results_log: PathBuf,
       pub baseline_commit: Option<String>,
       pub best_metric: f64,
       pub experiment_count: u64,
   }
   ```

3. **Experiment runner**
   - Fixed-time execution with SIGKILL after timeout
   - Capture stdout/stderr to log file
   - Parse metric from output via regex
   - Track memory usage (peak RSS)

### Phase 2: Git Integration

4. **Branch management**
   - Create `autoresearch/<tag>` branches
   - Commit experiments with descriptive messages
   - Reset/revert on failed experiments

5. **Worktree support** (leverage existing dcode-ai worktree infrastructure)
   - Run parallel experiments in separate worktrees
   - Each experiment gets isolated git state

### Phase 3: Agent Integration

6. **New skill format: `.research.md`**
   - Similar to `.dcode-ai/SKILL.md` but with autoresearch config
   - Defines the research program inline

7. **Built-in autoresearch skill**
   - Discovery from `.research.md` files
   - Provide tool for starting/stopping auto-research sessions

8. **Loop integration with AgentLoop**
   - Hook into dcode-ai's existing agent infrastructure
   - Autonomous mode: agent receives results, decides next experiment
   - Never-ask mode after initial setup

### Phase 4: Features & Polish

9. **Multi-metric support**
   - Primary metric (e.g., val_bpb)
   - Secondary constraints (memory, FLOPs)
   - Soft constraints with penalty functions

10. **Parallel experiment support**
    - Run multiple experiments on different GPUs
    - Aggregate results and pick winners

11. **Visualization**
    - Generate progress plots (similar to autoresearch progress.png)
    - TSV → CSV → simple ASCII chart

12. **Result analysis**
    - Detect diminishing returns
    - Suggest next directions based on history

## File Structure

```
crates/
  autoresearch/
    src/
      lib.rs              # Public API
      experiment.rs       # Experiment execution
      result.rs           # Result types & logging
      program.rs          # Research program parsing
      git_integration.rs  # Branch/commit management
      metric_parser.rs    # Parse metrics from output
      loop.rs             # Main research loop
    Cargo.toml
```

## CLI Integration

```bash
# Start autonomous research
dcode-ai autoresearch start --program my-research.md --tag mar15

# Check status
dcode-ai autoresearch status

# Stop research
dcode-ai autoresearch stop

# View results
dcode-ai autoresearch results

# Spawn as background task
dcode-ai spawn --autoresearch --program my-research.md
```

## Skill Format (`.research.md`)

```markdown
# My Auto-Research Program

## Setup
- Editable files: `train.py`
- Fixed files: `prepare.py`, `README.md`
- Time budget: 300 seconds (5 minutes)

## Metric
- Command: `grep "^val_bpb:" run.log | cut -d: -f2`
- Goal: minimize
- Baseline: 0.997900

## Constraints
- Max memory: 50GB
- Must finish within time budget

## Instructions
You are an autonomous researcher...
```

## Benefits over Python Implementation

| Aspect | Python (karpathy) | Rust (dcode-ai) |
|--------|-------------------|------------|
| Startup time | ~500ms uv init | ~5ms native |
| Memory overhead | ~50MB Python | ~2MB Rust |
| Parallel runs | Multiple processes | Lightweight async |
| Integration | External agent | Native dcode-ai integration |
| Tool access | Limited | Full dcode-ai tool suite |

## Risks & Mitigation

| Risk | Mitigation |
|------|------------|
| Agent infinite loop | Max iterations, timeout, manual interrupt |
| Disk space | Rotate old experiments, configurable retention |
| GPU OOM | Memory monitoring, early kill |
| Git conflicts | Worktree isolation |

## Implementation Order

1. ✅ Design and document
2. Create `crates/autoresearch/` with `Cargo.toml`
3. Implement `ExperimentResult`, `ResearchProgram` types
4. Implement `ExperimentRunner` with timeout and metric parsing
5. Add git integration for commits/branches
6. Add `autoresearch` skill discovery
7. Add CLI commands
8. Test with a real training script
9. Add parallel experiment support
10. Add visualization/analysis tools
