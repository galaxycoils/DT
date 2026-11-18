use clap::Parser;
use gm_core::error::Result;
use gm_daemon::{
    Cli, Commands, run_audit, run_bots, run_channels, run_chat, run_daemon, run_diagnostics,
    run_do, run_doctor_cli, run_dogfood, run_dream, run_feed, run_forget, run_goals, run_ideas,
    run_import_memory, run_kill, run_models, run_onboard, run_permissions, run_queue, run_review,
    run_routines, run_serve, run_service, run_skills, run_spaces, run_stats, run_status, run_team,
    run_update, run_upkeep,
};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Chat { system }) => run_chat(system, cli.config).await,
        Some(Commands::Permissions {
            open_missing,
            open,
            json,
        }) => run_permissions(open_missing, open, json).await,

        Some(Commands::Onboard { check_only }) => run_onboard(check_only).await,
        Some(Commands::Do { instruction }) => run_do(&instruction, cli.config).await,
        Some(Commands::Daemon) => run_daemon(cli.config, cli.task).await,
        Some(Commands::Status) => run_status().await,
        Some(Commands::Kill) => run_kill().await,
        Some(Commands::Service { action }) => run_service(action).await,
        Some(Commands::Dogfood { action, json }) => run_dogfood(action, json).await,
        Some(Commands::Stats { json }) => run_stats(json).await,
        Some(Commands::Review { date }) => run_review(date).await,
        Some(Commands::Audit { period, json }) => run_audit(&period, json).await,
        Some(Commands::Upkeep { stale_days, json }) => run_upkeep(stale_days, json).await,
        Some(Commands::Feed { limit, json }) => run_feed(limit, json).await,
        Some(Commands::Ideas { json }) => run_ideas(json).await,
        Some(Commands::Update {
            check,
            force,
            repo,
            json,
            dir,
        }) => run_update(check, force, repo, json, dir).await,
        Some(Commands::Models { action, json }) => run_models(action, json).await,
        Some(Commands::Bots { action, json }) => run_bots(action, json, cli.config).await,
        Some(Commands::Queue { action, json }) => run_queue(action, json, cli.config).await,
        Some(Commands::Channels { action, json }) => run_channels(action, json, cli.config).await,
        Some(Commands::Team { action, json }) => run_team(action, json, cli.config).await,
        Some(Commands::Routines { action, json }) => run_routines(action, json, cli.config).await,
        Some(Commands::Doctor { action, json }) => run_doctor_cli(action, json, cli.config).await,
        Some(Commands::Diagnostics { action, json }) => {
            run_diagnostics(action, json, cli.config).await
        }
        Some(Commands::Serve { bind }) => run_serve(&bind, cli.config).await,
        Some(Commands::Skills { action, json }) => run_skills(action, json, cli.config).await,
        Some(Commands::Goals { action, json }) => run_goals(action, json, cli.config).await,
        Some(Commands::Forget { action, json }) => run_forget(action, json, cli.config).await,
        Some(Commands::ImportMemory {
            file,
            input,
            apply,
            report_sha256,
            bot,
            json,
        }) => run_import_memory(file, input, apply, report_sha256, bot, json, cli.config).await,
        Some(Commands::Dream { date, json }) => run_dream(date, json, cli.config).await,
        Some(Commands::Spaces { action, json }) => run_spaces(action, json, cli.config).await,
        None => {
            // Default behavior when invoked as gm-daemon: run daemon or one-shot task
            run_daemon(cli.config, cli.task).await
        }
    }
}
