//! Standalone DT Doctor recovery executable runnable even when app or daemon is broken.

use clap::Parser;
use gm_core::error::Result;
use gm_daemon::{DoctorSubcommand, run_doctor_cli};
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
    action: Option<DoctorSubcommand>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = DoctorCli::parse();
    run_doctor_cli(cli.action, false, cli.config).await
}
