//! Standalone DT Doctor recovery executable runnable even when app or daemon is broken.

use clap::{Parser, Subcommand};
use gm_core::config::GmConfig;
use gm_core::error::Result;
use gm_daemon::doctor::{DoctorMode, run_doctor};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "dt-doctor",
    about = "DT Standalone Recovery & Diagnostics Executable"
)]
struct DoctorCli {
    #[arg(short, long)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    action: Option<DoctorAction>,
}

#[derive(Subcommand, Debug)]
enum DoctorAction {
    /// Inspect health, readiness, and permissions without applying repairs
    Check {
        #[arg(long)]
        json: bool,
    },
    /// Run serialized safe repairs and recheck readiness
    Repair {
        #[arg(long)]
        json: bool,
    },
    /// Resume a journaled Doctor run after interruption, consent prompt, or reboot
    Resume {
        run_id: String,
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = DoctorCli::parse();
    let config = GmConfig::load(cli.config.as_deref())?;
    let (mode, run_id, json) = match cli.action {
        Some(DoctorAction::Check { json }) => (DoctorMode::Check, None, json),
        Some(DoctorAction::Repair { json }) => (DoctorMode::Repair, None, json),
        Some(DoctorAction::Resume { run_id, json }) => (DoctorMode::Resume, Some(run_id), json),
        None => (DoctorMode::Check, None, false),
    };
    let report = run_doctor(&config, mode, run_id.as_deref()).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "DT Doctor [{}] ({}) -> {:?}",
            report.run_id, report.target, report.installation_readiness
        );
        for c in &report.checks {
            println!("  - [{}] {:?}: {}", c.id, c.outcome, c.summary);
        }
        if !report.pending_actions.is_empty() {
            println!("Pending actions:");
            for a in &report.pending_actions {
                println!("  * {a}");
            }
        }
    }
    Ok(())
}
