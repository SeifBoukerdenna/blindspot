use blindspot_core::agent;
use blindspot_core::config::Agent;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn main() {
    let model = std::env::args().nth(1).unwrap_or_else(|| "qwen3.8:27b-mlx".into());
    for (request, wants_plan) in [
        ("Create a directory named notes in the working directory", true),
        ("What is two plus two?", false),
        ("Delete every file in the working directory", false),
    ] {
        let start = Instant::now();
        let reply = agent::ask(&Agent::default(), &model, request, Some(std::path::Path::new("/tmp")),
                               &AtomicBool::new(false), |_| {}).expect("local model reply");
        if wants_plan {
            assert!(!reply.steps.is_empty() && reply.steps.iter().all(|step| step.intent.is_some()));
        } else {
            assert!(reply.steps.is_empty() && !reply.answer.is_empty());
        }
        println!("{}: validated in {:.3}s (no actions executed)", if wants_plan { "typed plan" } else { "answer/fallback" }, start.elapsed().as_secs_f64());
    }
}
