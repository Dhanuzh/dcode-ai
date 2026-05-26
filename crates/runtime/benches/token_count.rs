//! Benchmarks for the two hottest budgeting paths: raw BPE token counting and
//! per-message context estimation. Run with `cargo bench -p dcode-ai-runtime`.

use criterion::{Criterion, criterion_group, criterion_main};
use dcode_ai_common::message::{Message, MessageContent, Role};
use dcode_ai_runtime::context_manager::ContextManager;
use dcode_ai_runtime::token_count::count_tokens;
use std::hint::black_box;

const CODE: &str = r#"
fn fib(n: u64) -> u64 {
    let (mut a, mut b) = (0u64, 1u64);
    for _ in 0..n { let t = a + b; a = b; b = t; }
    a
}
struct Config { name: String, retries: u32, endpoints: Vec<String> }
"#;

fn bench_count_tokens(c: &mut Criterion) {
    // Repeat so we measure throughput on a realistically sized turn, not noise.
    let text = CODE.repeat(20);
    c.bench_function("count_tokens/code_~1.5k_chars", |b| {
        b.iter(|| count_tokens(black_box(&text)))
    });
}

fn bench_estimate_tokens(c: &mut Criterion) {
    let messages: Vec<Message> = (0..40)
        .map(|i| Message {
            role: if i % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            content: MessageContent::Text(format!("Turn {i}: {CODE}")),
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
        })
        .collect();

    c.bench_function("estimate_tokens_for_slice/40_msgs", |b| {
        b.iter(|| ContextManager::estimate_tokens_for_slice(black_box(&messages)))
    });
}

criterion_group!(benches, bench_count_tokens, bench_estimate_tokens);
criterion_main!(benches);
