# Rust + Ratatui Performance Optimization Research

> **Goal**: Make `dcode-ai` extremely lightweight and fast while maintaining Rust's safety guarantees.

---

## Executive Summary

Rust + Ratatui offers superior performance compared to Go alternatives (no GC, fine-grained control), but requires deliberate optimization patterns to achieve minimal CPU usage. Key insights from Zellij's optimization journey and Ratatui's own issues:

| Problem | Solution | Impact |
|---------|----------|--------|
| 60 FPS continuous rendering | Event-driven dirty flag rendering | 50% → 1% CPU |
| MPSC channel overflow | Bounded channels with backpressure | 2x speedup |
| Buffer diffing overhead | Only render changed regions | Significant for static content |
| Unicode width lookups | Cache symbol width | 17% rendering improvement |
| Large future stack copies | Heap-allocation optimization | Reduced memory pressure |

---

## 1. Ratatui Rendering Architecture

### How Ratatui Works

```
┌─────────────────────────────────────────────────────────────┐
│  Your App                  Ratatui              Terminal   │
│  ───────                  ───────              ────────     │
│                                                             │
│  terminal.draw(|f| {           Buffer A                    │
│    f.render_widget(...)  ────► ┌─────────────────┐          │
│  });                           │ Cell │ Cell │ ...│ Buffer │
│                                │ Cell │ Cell │ ...│   B    │
│                                └─────────────────┘          │
│                                      │                      │
│                                      ▼                      │
│                               diff(A, B) → Δ               │
│                                      │                      │
│                                      ▼                      │
│                               Write Δ to terminal           │
└─────────────────────────────────────────────────────────────┘
```

### The Performance Problem

**Issue**: Ratatui calls `diff()` on every frame even when content is identical. The diff algorithm:
1. Iterates every cell in the buffer
2. Calls `.width()` twice per cell (Unicode width calculation)
3. Compares current vs previous buffer state

**Benchmark** (from Ratatui issue #1338):
- Debug build: 50% single-core CPU at 60 FPS
- Release build: 7% single-core CPU at 60 FPS
- Static content should be ~0% CPU

### Key Ratatui Optimization Patterns

#### Pattern 1: Dirty Flag Rendering (CRITICAL)

```rust
// ❌ BAD: Continuous rendering regardless of changes
loop {
    terminal.draw(|f| {
        f.render_widget(&app);
    });
    sleep(Duration::from_millis(16)); // 60 FPS
}

// ✅ GOOD: Only render when state changes
loop {
    app.update();

    if app.is_dirty() {
        terminal.draw(|f| {
            f.render_widget(&app);
        });
        app.clear_dirty();
    }

    sleep(Duration::from_millis(16));
}
```

#### Pattern 2: Pre-build Widgets Outside Draw

```rust
// ❌ BAD: Rebuild widget state every frame
fn render(&mut self, f: &mut Frame) {
    let list = List::new(items.iter().map(|i| ListItem::new(i.content)));
    f.render_widget(list, area);
}

// ✅ GOOD: Build once, reference in draw
struct App {
    list_state: ListState,
    cached_items: Vec<ListItem<'static>>,
}

impl App {
    fn update(&mut self) {
        // Only rebuild when data changes
        if self.data_changed {
            self.cached_items = self.items.iter()
                .map(|i| ListItem::new(i.content.clone()))
                .collect();
            self.data_changed = false;
        }
    }

    fn render(&self, f: &mut Frame) {
        let list = List::new(self.cached_items.iter());
        f.render_widget_ref(list, area); // WidgetRef for pre-built
    }
}
```

#### Pattern 3: Incremental Diff Optimization

Ratatui stores symbols as individual cells. For ASCII-only content, this is overhead. The Ratatui team suggests:

- Storing text as **runs** instead of single cells
- Caching width calculations between frames
- Only updating viewport regions that changed

---

## 2. Zellij's Multi-Threaded Architecture

Zellij achieves terminal-multiplexer performance parity with tmux through architectural patterns:

### Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                         PTY Thread                              │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────────┐  │
│  │ Poll PTY     │───►│ Send data    │───►│ Bounded Channel │  │
│  │ (non-blocking)│    │ to screen   │    │ (50 msg buffer) │  │
│  └──────────────┘    └──────────────┘    └────────┬─────────┘  │
│                                                      │           │
│                      Screen Thread                   │           │
│  ┌──────────────┐    ┌──────────────┐    ┌─────────▼─────────┐ │
│  │ Receive from  │◄───│ Parse ANSI  │◄───│ Backpressure     │ │
│  │ channel       │    │ /VT codes   │    │ blocks when full │ │
│  └──────┬───────┘    └──────────────┘    └───────────────────┘ │
│         │                                                        │
│         ▼                                                        │
│  ┌──────────────┐    ┌──────────────┐                           │
│  │ Grid state   │───►│ Render only  │───► Terminal             │
│  │ (viewport)   │    │ changed lines│                           │
│  └──────────────┘    └──────────────┘                           │
└─────────────────────────────────────────────────────────────────┘
```

### Key Pattern: Bounded Channel Backpressure

```rust
// Zellij's bounded channel (50 messages)
let (tx, rx) = channel::<Message>(50);

// PTY thread blocks when channel full (backpressure)
loop {
    match deadline_read(&mut reader, deadline, &mut buf).await {
        ReadResult::Timeout => {
            tx.send(Message::Render).await.unwrap(); // Blocks if full
            deadline = None;
        }
        ReadResult::Ok(n) => {
            tx.send(Message::Data(&buf[..n])).await.unwrap();
            deadline.get_or_insert(Instant::now() + render_pause);
        }
        ReadResult::Ok(0) | ReadResult::Err(_) => break,
    }
}
```

**Result**: Cat-ing a 2M line file in Zellij went from 19.2s to 5.3s (3.6x faster).

---

## 3. Rust Async & Tokio Optimization

### Zero-Cost Abstraction Reality

Rust's async/await compiles to efficient state machines, but Tokio abstractions add measurable overhead:

| Abstraction | Overhead Source |
|--------------|-----------------|
| `task::spawn` | Stack allocation, scheduling |
| `mpsc::channel` | Internal synchronization |
| `JoinSet` | Task tracking metadata |
| `select!` | Branch prediction, polling |

**Measured**: Runtime overhead averages 12-18% of CPU under heavy load (not business logic).

### Optimization Techniques

#### 1. Avoid Spawning Large Futures

Large futures (>1KB) trigger stack copies on spawn:

```rust
// ❌ BAD: Large future captures entire AppState
task::spawn(async move {
    let data = app_state.expensive_clone();
    process(data).await
});

// ✅ GOOD: Arc<Mutex<>> for shared state, small future
let shared = Arc::clone(&app_state);
task::spawn(async move {
    let guard = shared.lock().await;
    process(&guard.data).await
});
```

Tokio has optimized this in recent versions (#4487), but be mindful of future size.

#### 2. Bounded Channels for Backpressure

```rust
// ❌ BAD: Unbounded - can accumulate infinite messages
let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

// ✅ GOOD: Bounded - sender blocks when full
let (tx, rx) = tokio::sync::mpsc::channel(100);
```

This matches Zellij's pattern: bounded channels prevent memory bloat and create natural backpressure.

#### 3. Select with Bias for Latency-Critical Paths

```rust
// ✅ GOOD: Prioritize certain branches
loop {
    tokio::select! {
        biased; // Process in order listed

        result = rx.recv() => {
            if let Some(msg) = result {
                handle(msg);
            }
        }
        _ = sleep(Duration::from_millis(16)) => {
            // Rate-limited fallback
        }
    }
}
```

#### 4. Async Traits with `async-trait`

The `async-trait` crate is **not** zero-cost — it heap-allocates a `Box<dyn Future>` per call. For hot paths, prefer native async functions or poll-based approaches. For trait-object dispatch (like our tool executors), the allocation is acceptable since the network call dominates.

```rust
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, input: ToolInput) -> Result<ToolOutput>;
}
```

---

## 4. Binary Size Optimization

For a lightweight CLI, binary size matters for distribution and cold-start time.

### Release Profile Optimization

```toml
# Cargo.toml
[profile.release]
opt-level = "z"        # Optimize for size over speed
lto = true             # Link-time optimization
codegen-units = 1      # Single codegen unit for max optimization
strip = true           # Remove debug symbols
panic = "abort"        # Smaller panic handling

# For even more size savings:
[profile.release.package.dcode-ai-cli]
opt-level = "z"
```

### Expected Size Reductions

| Optimization | Binary Size Reduction |
|--------------|----------------------|
| `opt-level = "z"` | 25-30% |
| `lto = true` | 5-10% |
| `strip = true` | 3-8% |
| `panic = "abort"` | 2-5% |
| **Combined** | **40-50%** |

### Compile-Time Trade-off

These optimizations significantly increase compile time. Use in CI/release builds, not during development:

```bash
# Development
cargo build

# Release
cargo build --release
```

---

## 5. Memory Allocation Optimization

### Preallocate Vectors

```rust
// ❌ BAD: Vec grows by doubling
let mut rows: Vec<Row> = Vec::new();
for _ in 0..width {
    rows.push(Row::new());
}

// ✅ GOOD: Preallocate
let mut rows: Vec<Row> = Vec::with_capacity(width);
for _ in 0..width {
    rows.push(Row::with_capacity(height));
}
```

### Cache Expensive Computations

```rust
// ❌ BAD: Compute width on every access
fn line_width(&self) -> usize {
    self.columns.iter().map(|c| c.character.width()).sum()
}

// ✅ GOOD: Cache width in struct
#[derive(Clone, Copy)]
struct TerminalCharacter {
    character: char,
    styles: CharacterStyles,
    width: usize, // Cached at construction
}
```

---

## 6. dcode-ai-Specific Recommendations

Based on current architecture in `crates/cli/src/tui/app.rs`:

### Immediate Optimizations

1. **Add dirty flag to App state**
   - Track `is_dirty()` boolean
   - Only call `terminal.draw()` when dirty
   - Set dirty on any state change (message received, approval requested, etc.)

2. **Pre-build static widgets**
   - Block widgets, borders, labels built once
   - Reuse across frames

3. **Cache string measurements**
   - Unicode width for repeated strings
   - Use `unicode-width` crate's `Cached`

### Architecture Improvements

4. **Separate rendering from event loop**
   - Current: `app.update()` called inside draw loop
   - Ideal: Event-driven rendering via channel

5. **Consider bounded channels for IPC**
   - Currently using unbounded `mpsc::unboundedSender`
   - Zellij shows bounded channels prevent resource exhaustion

### Code Example: Dirty Flag Implementation

```rust
// In App state
pub struct App {
    is_dirty: bool,
    // ... other state
}

impl App {
    pub fn mark_dirty(&mut self) {
        self.is_dirty = true;
    }

    pub fn clear_dirty(&mut self) {
        self.is_dirty = false;
    }

    pub fn is_dirty(&self) -> bool {
        self.is_dirty
    }
}

// In main loop
loop {
    // Process events
    while let Some(event) = rx.try_recv() {
        app.handle_event(event);
    }

    // Only render when dirty
    if app.is_dirty() {
        terminal.draw(|f| {
            app.render(f);
        });
        app.clear_dirty();
    }

    // Sleep to avoid spinning
    sleep(Duration::from_millis(16)).await;
}
```

---

## 7. Measurement Strategy

Before optimizing, establish baselines:

```bash
# CPU profiling
CARGO_PROFILE_RELEASE_DEBUG=true cargo flamegraph --root

# Memory profiling
cargo build --release && /usr/bin/time -v target/release/dcode-ai

# Binary size
ls -lh target/release/dcode-ai
wc -c target/release/dcode-ai
```

### Target Metrics for dcode-ai

| Metric | Current | Target |
|--------|---------|--------|
| Idle CPU | ~7% (from Ratatui issue) | <1% |
| Active typing CPU | ~10% (from issue) | <5% |
| Binary size | ~? MB | <5 MB |
| Cold start | ~? ms | <100ms |

---

## 8. References

- [Ratatui Issue #1338](https://github.com/ratatui/ratatui/issues/1338) - High CPU usage analysis
- [Ratatui PR #1339](https://github.com/ratatui/ratatui/pull/1339) - Symbol width caching (17% improvement)
- [Zellij Performance Blog](https://poor.dev/blog/performance/) - MPSC backpressure, preallocation
- [Tokio PR #4487](https://github.com/tokio-rs/tokio/pull/4487) - Spawn optimization for large futures
- [Rust Binary Size Optimization Guide](https://elitedev.in/rust/optimizing-rust-binary-size-essential-techniques-/)
- [Zero-Cost Abstractions in Async Rust](https://dev.to/pranta/zero-cost-abstractions-in-rust-asynchronous-programming-without-breaking-a-sweat-221b)

---

## 9. Action Items

### High Priority
- [ ] Add dirty flag rendering to `crates/cli/src/tui/app.rs`
- [ ] Profile current CPU usage with flamegraph
- [ ] Add bounded channels to IPC (from unbounded)

### Medium Priority
- [ ] Pre-build static UI components (blocks, borders)
- [ ] Add release profile size optimization to Cargo.toml
- [ ] Cache Unicode width for repeated strings

### Low Priority (Future)
- [ ] Consider WidgetRef pattern for pre-built widgets
- [ ] Multi-thread rendering pipeline (like Zellij)
- [ ] Benchmark and track metrics over time
