//! Binary entrypoint for the Firmament maker runtime.

use anyhow::Context;
use firmament::{AppConfig, api, bootstrap, bootstrap_maker_runtime};
use std::io;
use std::time::Duration;
use tokio::time::sleep;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing()?;

    let config = AppConfig::load().context("load application config")?;
    if config.runtime.enable_protocol_workers {
        let live = bootstrap_maker_runtime(config)
            .await
            .context("bootstrap maker runtime")?;

        info!(
            run_id = %live.app_state.run_id(),
            http_bind = %live.app_state.config().http.bind_address,
            http_port = live.app_state.config().http.port,
            workers_enabled = live.app_state.config().runtime.enable_protocol_workers,
            "Firmament RFQ maker runtime ready"
        );

        return run_maker_api(live.orchestrator).await;
    }

    let app_state = bootstrap(config).await.context("bootstrap runtime")?;

    info!(
        run_id = %app_state.run_id(),
        http_bind = %app_state.config().http.bind_address,
        http_port = app_state.config().http.port,
        workers_enabled = app_state.config().runtime.enable_protocol_workers,
        "Firmament RFQ maker runtime ready without protocol workers"
    );

    let hold_millis = app_state.config().runtime.scaffold_hold_millis;
    if hold_millis > 0 {
        sleep(Duration::from_millis(hold_millis)).await;
        return Ok(());
    }

    if api::enabled(app_state.config()) {
        return api::serve(app_state).await.context("serve local HTTP API");
    }

    info!("HTTP API is disabled; maker runtime has no foreground service to run");
    Ok(())
}

async fn run_maker_api(
    orchestrator: std::sync::Arc<firmament::runtime::RuntimeOrchestrator>,
) -> anyhow::Result<()> {
    let hold_millis = orchestrator.config().runtime.scaffold_hold_millis;
    if hold_millis > 0 {
        sleep(Duration::from_millis(hold_millis)).await;
        return Ok(());
    }

    if api::enabled(orchestrator.config()) {
        return api::serve_orchestrator(orchestrator)
            .await
            .context("serve local HTTP API");
    }

    info!("HTTP API is disabled; maker runtime has no foreground service to run");
    Ok(())
}

fn init_tracing() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("warn,firmament=info"))
        .context("build tracing filter")?;

    tracing_subscriber::fmt()
        .compact()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(io::stdout)
        .try_init()
        .map_err(|error| anyhow::anyhow!("initialize tracing subscriber: {error}"))
}
