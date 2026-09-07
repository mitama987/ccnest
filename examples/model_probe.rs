//! One-off diagnostic: run the REAL SessionTailer against live session files
//! to see whether the focused-pane model would resolve. Temporary; delete after.
//!
//! Run with: `cargo run --example model_probe`

use std::path::Path;

use ccnest::claude::session::{pretty_model, session_path, SessionTailer};

fn main() {
    let cwd = Path::new(r"C:\Users\mitam\Desktop\work\90_other\ClaudeCompany");
    let ids = [
        "a6f8ef02-9aad-4be8-a649-214e9cebd566",
        "805bd658-bf1a-40df-9b6d-63dca6761e2d",
    ];
    for id in ids {
        let p = session_path(cwd, id);
        println!("id={id}");
        println!("  path={p:?}");
        println!(
            "  exists={}",
            p.as_ref().map(|p| p.exists()).unwrap_or(false)
        );
        let mut t = SessionTailer::new(p);
        // Two refreshes: first seeds via backward scan, second is the delta path.
        t.refresh();
        let after1 = (t.state(), t.info().model.clone());
        t.refresh();
        println!(
            "  after refresh#1: state={:?} model={:?}",
            after1.0, after1.1
        );
        println!(
            "  after refresh#2: state={:?} model={:?} usage={:?}",
            t.state(),
            t.info().model,
            t.info().usage
        );
        if let Some(m) = &t.info().model {
            println!("  label => {}", pretty_model(m));
        } else {
            println!("  label => Claude …  (UNRESOLVED)");
        }
    }
}
