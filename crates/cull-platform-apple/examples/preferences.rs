//! Learn from your decisions: propose new ranking weights, approve them, look back, undo.
//!
//!   cargo run --release -p cull-platform-apple --example preferences -- train [--db PATH] [--yes]
//!   cargo run --release -p cull-platform-apple --example preferences -- history [--db PATH]
//!   cargo run --release -p cull-platform-apple --example preferences -- revert <id> [--db PATH]
//!   cargo run --release -p cull-platform-apple --example preferences -- off [--db PATH]
//!
//! `train` only reads your decisions and photo analysis. It prints how well the proposed weights predict your
//! newest decisions and what would move, then asks. Your answer (approved or declined) is recorded in the
//! `preference_models` / `preference_events` tables with the weights and the moves, so any model can be restored
//! with `revert` and the history can be studied later. Photos, decisions and analysis are never touched.
//! The ★ in a burst is picked by the approved model from the next time the app builds a plan; `off` goes back
//! to the hand-tuned ranking.

use std::io::Write;
use std::path::PathBuf;

use cull_core::preference::history::{StoredModel, Verdict};
use cull_core::preference::train::{Config, Metrics, cross_validate, cv_report, propose};
use cull_core::store::Store;

fn default_db() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Library/Application Support/app.darkroom.desktop/darkroom.db")
}

fn ask(question: &str) -> bool {
    print!("{question} [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok();
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

fn auc_of(v: &serde_json::Value, which: &str) -> String {
    v.get("metrics").and_then(|m| m.get(which)).and_then(|e| e.get("auc")).and_then(|a| a.as_f64()).map_or("n/a".into(), |a| format!("{a:.3}"))
}

fn print_history(models: &[StoredModel]) {
    if models.is_empty() {
        println!("No models yet. Run `train`.");
        return;
    }
    println!("{:<4} {:<20} {:<9} {:>9} {:>9} {:>8}  biggest moves", "id", "when", "status", "decisions", "AUC held", "AUC old");
    for m in models {
        let metrics: Option<Metrics> = serde_json::from_value(m.metrics.clone()).ok();
        let changes: Vec<cull_core::preference::WeightChange> = serde_json::from_value(m.changes.clone()).unwrap_or_default();
        let moves: Vec<String> = changes.iter().take(3).map(|c| format!("{} {:+.2}", c.name, c.delta)).collect();
        let wrapped = serde_json::json!({ "metrics": m.metrics });
        println!(
            "{:<4} {:<20} {:<9} {:>9} {:>9} {:>8}  {}{}",
            m.id,
            m.created_at,
            m.status,
            metrics.map_or(0, |x| x.decisions),
            auc_of(&wrapped, "candidate"),
            auc_of(&wrapped, "current"),
            moves.join(", "),
            m.note.as_deref().map(|n| format!("  [{n}]")).unwrap_or_default()
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("train");
    let db = args.iter().position(|a| a == "--db").and_then(|i| args.get(i + 1)).map(PathBuf::from).unwrap_or_else(default_db);
    let yes = args.iter().any(|a| a == "--yes");
    if !db.exists() {
        eprintln!("No database at {} (use --db PATH)", db.display());
        std::process::exit(1);
    }
    let store = Store::open(&db).expect("open the database");

    match command {
        "history" => print_history(&store.preference_models().unwrap()),
        "off" => {
            store.deactivate_preference().unwrap();
            println!("No learned model is active now; the app ranks with the hand-tuned score. History is kept.");
        }
        "revert" => {
            let id: i64 = args.get(1).and_then(|s| s.parse().ok()).expect("revert <id> (see `history`)");
            store.revert_preference(id).unwrap();
            println!("Model {id} is active again.");
        }
        "train" => {
            let analyzers = cull_platform_apple::vision::analyzers();
            let samples = store.preference_samples(&analyzers).unwrap();
            let active = store.active_preference().unwrap();
            let proposal = match propose(&samples, active.as_ref().map(|m| &m.weights), &Config::default()) {
                Ok(p) => p,
                Err(why) => {
                    println!("No proposal: {why}.");
                    return;
                }
            };
            println!("{}", proposal.report(active.is_some()));
            match cross_validate(&samples, &Config::default(), 5) {
                Ok(rows) => println!("{}", cv_report(&rows, 5)),
                Err(why) => println!("No cross-validation: {why}.\n"),
            }
            match &active {
                Some(m) => println!("Replaces model {} (approved {}).", m.id, m.created_at),
                None => println!("Replaces: the hand-tuned ranking (no learned model is active)."),
            }
            let verdict = if yes || ask("Approve this model so the ★ uses it?") { Verdict::Approve } else { Verdict::Decline };
            let id = store.record_preference(&proposal, verdict, None).unwrap();
            match verdict {
                Verdict::Approve => println!("Approved and saved as model {id}. Undo any time: `revert <id>` or `off`."),
                Verdict::Decline => println!("Declined; kept in the history as model {id}. Nothing changed."),
            }
        }
        other => {
            eprintln!("Unknown command `{other}`. Use: train | history | revert <id> | off");
            std::process::exit(2);
        }
    }
}
