//! Binary entrypoint for the RFQ maker runtime.

use anyhow::Context;
use std::sync::Arc;
use std::time::Duration;
use tbd_rfq_maker_runtime::{AppConfig, api, bootstrap, bootstrap_live_runtime, tui};
use tokio::time::sleep;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing()?;

    let config = AppConfig::load().context("load application config")?;
    if config.runtime.enable_protocol_workers {
        let live = bootstrap_live_runtime(config)
            .await
            .context("bootstrap live runtime")?;

        info!(
            run_id = %live.app_state.run_id(),
            http_bind = %live.app_state.config().http.bind_address,
            http_port = live.app_state.config().http.port,
            workers_enabled = live.app_state.config().runtime.enable_protocol_workers,
            "TBD RFQ maker runtime ready"
        );

        return run_live(live.orchestrator).await;
    }

    let app_state = bootstrap(config).await.context("bootstrap runtime")?;

    info!(
        run_id = %app_state.run_id(),
        http_bind = %app_state.config().http.bind_address,
        http_port = app_state.config().http.port,
        workers_enabled = app_state.config().runtime.enable_protocol_workers,
        "TBD RFQ maker runtime ready without protocol workers"
    );

    let hold_millis = app_state.config().runtime.scaffold_hold_millis;
    if hold_millis > 0 {
        sleep(Duration::from_millis(hold_millis)).await;
        return Ok(());
    }

    let api_enabled = api::enabled(app_state.config());
    let tui_enabled = tui::stdout_is_terminal();

    match (api_enabled, tui_enabled) {
        (true, true) => run_api_with_tui(app_state).await?,
        (true, false) => return api::serve(app_state).await.context("serve local HTTP API"),
        (false, true) => tui::run(app_state.runtime())
            .await
            .context("run operator cockpit")?,
        (false, false) => info!("stdout is not a terminal; skipping operator cockpit"),
    }

    Ok(())
}

async fn run_live(
    orchestrator: Arc<tbd_rfq_maker_runtime::runtime::RuntimeOrchestrator>,
) -> anyhow::Result<()> {
    let hold_millis = orchestrator.config().runtime.scaffold_hold_millis;
    if hold_millis > 0 {
        sleep(Duration::from_millis(hold_millis)).await;
        return Ok(());
    }

    let api_enabled = api::enabled(orchestrator.config());
    let tui_enabled = tui::stdout_is_terminal();

    match (api_enabled, tui_enabled) {
        (true, true) => run_live_api_with_tui(orchestrator).await?,
        (true, false) => {
            return api::serve_orchestrator(orchestrator)
                .await
                .context("serve local HTTP API");
        }
        (false, true) => tui::run_with_orchestrator(orchestrator)
            .await
            .context("run operator cockpit")?,
        (false, false) => info!("stdout is not a terminal; skipping operator cockpit"),
    }

    Ok(())
}

async fn run_api_with_tui(state: tbd_rfq_maker_runtime::AppState) -> anyhow::Result<()> {
    let http_state = state.clone();
    let api_task = tokio::spawn(async move { api::serve(http_state).await });

    let tui_result = tui::run(state.runtime())
        .await
        .context("run operator cockpit");

    api_task.abort();
    let api_result = api_task.await;

    tui_result?;
    match api_result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(error).context("serve local HTTP API"),
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(error).context("join local HTTP API task"),
    }
}

async fn run_live_api_with_tui(
    orchestrator: Arc<tbd_rfq_maker_runtime::runtime::RuntimeOrchestrator>,
) -> anyhow::Result<()> {
    let http_orchestrator = Arc::clone(&orchestrator);
    let api_task = tokio::spawn(async move { api::serve_orchestrator(http_orchestrator).await });

    let tui_result = tui::run_with_orchestrator(orchestrator)
        .await
        .context("run operator cockpit");

    api_task.abort();
    let api_result = api_task.await;

    tui_result?;
    match api_result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(error).context("serve local HTTP API"),
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(error).context("join local HTTP API task"),
    }
}

fn init_tracing() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .compact()
        .with_target(false)
        .try_init()
        .map_err(|error| anyhow::anyhow!("initialize tracing subscriber: {error}"))
}
