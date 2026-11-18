use gm_core::error::Result;

#[tokio::main]
async fn main() -> Result<()> {
    gm_daemon::run_cli_entrypoint().await
}
