//! Fills a history file with made-up sessions, to check the History page
//! with many of them.
//!
//! ```text
//! cargo run -p tidedesk-core --example sample_history -- SCRATCH\TideDesk\sessions.history 80
//! ```
//!
//! The sessions are spread over the last 20 days, so everyone sees them.
//! Give it a scratch settings folder, never the real one: the sessions are
//! sealed for this Windows account like real ones and cannot be told apart.

use std::path::Path;

use anyhow::{Context, Result, bail};
use tidedesk_core::history::{self, Record};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path, count] = args.as_slice() else {
        bail!("usage: sample_history HISTORY-FILE COUNT");
    };
    let count: u64 = count.parse().context("COUNT is a number")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let viewers = ["Ana's laptop", "Office PC", "Mihai", "Support desk", "Home"];
    let ways = ["access code", "saved password", "trusted viewer"];
    let ends = [
        "the viewer disconnected",
        "you disconnected them",
        "the connection was lost",
    ];
    for i in 0..count {
        let started = now - 20 * 86_400 + i * (20 * 86_400 / count);
        let n = i as usize;
        history::append_to(
            Path::new(path),
            Record {
                started,
                ended: started + 60 * (5 + i % 50),
                viewer: format!("{} {}", viewers[n % viewers.len()], i + 1),
                fingerprint: format!("{:04X} 2FA6 7B0B BD0F", i),
                address: format!("192.168.1.{}:51000", 10 + i % 200),
                admitted_by: ways[n % ways.len()].into(),
                ended_because: ends[n % ends.len()].into(),
            },
        )?;
    }
    println!("Added {count} sessions to {path}.");
    Ok(())
}
