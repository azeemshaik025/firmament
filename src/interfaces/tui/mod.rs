//! Ratatui operator cockpit shell.

pub mod state;

use std::{
    io::{self, IsTerminal},
    sync::Arc,
    time::Duration,
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Row, Table, Tabs, Wrap},
};
use tracing::info;

use crate::{
    error::{AppError, AppResult},
    runtime::{RuntimeHandle, RuntimeOrchestrator, RuntimeState},
    tui_state::{
        OperatorCommand, OperatorTab, ScriptedDemoAction, StatusRow, TuiInput, TuiState,
        TuiViewModel,
    },
};

const TICK_RATE: Duration = Duration::from_millis(250);

/// Return whether stdout looks interactive enough to launch the TUI.
#[must_use]
pub fn stdout_is_terminal() -> bool {
    io::stdout().is_terminal()
}

/// Run the operator cockpit against the current runtime projection.
///
/// # Errors
///
/// Returns an error if terminal setup, input polling, rendering, or cleanup fails.
pub async fn run(runtime: RuntimeHandle) -> AppResult<()> {
    let mut terminal = setup_terminal()?;
    let loop_result = run_loop(&mut terminal, runtime, None).await;
    let restore_result = restore_terminal(&mut terminal);

    match (loop_result, restore_result) {
        (Err(error), _) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// Run the operator cockpit with commands wired to a runtime orchestrator.
///
/// # Errors
///
/// Returns an error if terminal setup, rendering, command dispatch, or cleanup
/// fails.
pub async fn run_with_orchestrator(orchestrator: Arc<RuntimeOrchestrator>) -> AppResult<()> {
    let runtime = orchestrator.runtime();
    let mut terminal = setup_terminal()?;
    let loop_result = run_loop(&mut terminal, runtime, Some(orchestrator)).await;
    let restore_result = restore_terminal(&mut terminal);

    match (loop_result, restore_result) {
        (Err(error), _) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// Render one cockpit frame.
pub fn render(frame: &mut Frame<'_>, area: Rect, state: &TuiState, runtime: &RuntimeState) {
    let view = TuiViewModel::from_runtime(runtime);
    let [tabs_area, body_area, events_area, help_area] =
        vertical_chunks(area, [3, area.height.saturating_sub(14), 8, 3]);

    render_tabs(frame, tabs_area, state.selected_tab());
    render_active_tab(frame, body_area, state, &view);
    render_status_table(frame, events_area, "Recent Events", &view.event_rows, 0);
    render_help(frame, help_area);
}

fn setup_terminal() -> AppResult<Terminal<CrosstermBackend<io::Stdout>>> {
    map_io(enable_raw_mode(), "enable raw terminal mode")?;
    let mut stdout = io::stdout();
    map_io(
        execute!(stdout, EnterAlternateScreen),
        "enter alternate screen",
    )?;

    let backend = CrosstermBackend::new(stdout);
    map_io(Terminal::new(backend), "initialize ratatui terminal")
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> AppResult<()> {
    let leave_result = map_io(
        execute!(terminal.backend_mut(), LeaveAlternateScreen),
        "leave alternate screen",
    );
    let raw_result = map_io(disable_raw_mode(), "disable raw terminal mode");
    terminal.show_cursor().map_err(|error| {
        AppError::internal(format!("restore terminal cursor visibility: {error}"))
    })?;

    leave_result.and(raw_result)
}

async fn run_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    runtime: RuntimeHandle,
    orchestrator: Option<Arc<RuntimeOrchestrator>>,
) -> AppResult<()> {
    let mut state = TuiState::default();

    while !state.should_quit() {
        let snapshot = runtime.snapshot().await;
        map_io(
            terminal.draw(|frame| render(frame, frame.area(), &state, &snapshot)),
            "draw TUI frame",
        )?;

        if map_io(event::poll(TICK_RATE), "poll terminal input")? {
            let Event::Key(key) = map_io(event::read(), "read terminal input")? else {
                continue;
            };
            if let Some(command) = handle_key(key, &mut state, &snapshot) {
                dispatch_operator_command(orchestrator.as_deref(), command).await?;
            }
        }
    }

    Ok(())
}

fn handle_key(
    key: KeyEvent,
    state: &mut TuiState,
    runtime: &RuntimeState,
) -> Option<OperatorCommand> {
    let view = TuiViewModel::from_runtime(runtime);
    let row_count = view.rows_for_tab(state.selected_tab()).len();
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => {
            state.select_next_row(row_count);
            return None;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            state.select_previous_row(row_count);
            return None;
        }
        _ => {}
    }

    let input = input_from_key(key)?;

    if let Some(command) = state.apply_input(input) {
        match command {
            OperatorCommand::Quit => info!("operator requested graceful shutdown"),
            OperatorCommand::Demo(action) => {
                info!(command = action.label(), "operator demo command requested");
            }
        }
        return Some(command);
    }

    None
}

/// Dispatch one TUI command into the runtime orchestrator.
///
/// # Errors
///
/// Returns a validation error when a mutating command is issued without an
/// orchestrator, or adapter errors when the orchestrator command fails.
pub async fn dispatch_operator_command(
    orchestrator: Option<&RuntimeOrchestrator>,
    command: OperatorCommand,
) -> AppResult<()> {
    match command {
        OperatorCommand::Quit => {
            if let Some(orchestrator) = orchestrator {
                orchestrator
                    .shutdown("operator requested graceful shutdown")
                    .await?;
            }
            Ok(())
        }
        OperatorCommand::Demo(action) => {
            let orchestrator = orchestrator.ok_or_else(|| {
                AppError::validation("operator demo command requires a runtime orchestrator")
            })?;
            dispatch_demo_action(orchestrator, action).await
        }
    }
}

/// Dispatch one scripted demo action into the runtime orchestrator.
///
/// # Errors
///
/// Returns adapter, risk, or validation errors from the runtime orchestrator.
pub async fn dispatch_demo_action(
    orchestrator: &RuntimeOrchestrator,
    action: ScriptedDemoAction,
) -> AppResult<()> {
    match action {
        ScriptedDemoAction::GenerateTinyRfq => {
            orchestrator.generate_tiny_demo_rfq().await?;
        }
        ScriptedDemoAction::GenerateOversizedRfq => {
            orchestrator.generate_oversized_demo_rfq().await?;
        }
        ScriptedDemoAction::AcceptQuote => {
            orchestrator.accept_latest_quote().await?;
        }
        ScriptedDemoAction::TriggerRebalanceCheck => {
            orchestrator
                .trigger_operator_automation_check("rebalance")
                .await?;
        }
        ScriptedDemoAction::TriggerGatewayRefillCheck => {
            orchestrator
                .trigger_operator_automation_check("gateway")
                .await?;
        }
        ScriptedDemoAction::TriggerExposureHedgeCheck => {
            orchestrator
                .trigger_operator_automation_check("hedge")
                .await?;
        }
    }
    Ok(())
}

fn input_from_key(key: KeyEvent) -> Option<TuiInput> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(TuiInput::Quit);
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Some(TuiInput::Quit),
        KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => Some(TuiInput::NextTab),
        KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => Some(TuiInput::PreviousTab),
        KeyCode::Char('n') => Some(TuiInput::Demo(ScriptedDemoAction::GenerateTinyRfq)),
        KeyCode::Char('o') => Some(TuiInput::Demo(ScriptedDemoAction::GenerateOversizedRfq)),
        KeyCode::Char('a') => Some(TuiInput::Demo(ScriptedDemoAction::AcceptQuote)),
        KeyCode::Char('r') => Some(TuiInput::Demo(ScriptedDemoAction::TriggerRebalanceCheck)),
        KeyCode::Char('g') => Some(TuiInput::Demo(
            ScriptedDemoAction::TriggerGatewayRefillCheck,
        )),
        KeyCode::Char('x') => Some(TuiInput::Demo(
            ScriptedDemoAction::TriggerExposureHedgeCheck,
        )),
        _ => None,
    }
}

fn render_tabs(frame: &mut Frame<'_>, area: Rect, selected_tab: OperatorTab) {
    let titles = OperatorTab::ALL
        .iter()
        .map(|tab| Line::from(tab.title()))
        .collect::<Vec<_>>();
    let tabs = Tabs::new(titles)
        .select(selected_tab.index())
        .block(Block::default().borders(Borders::ALL).title("RFQ Maker"))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().fg(Color::Gray));

    frame.render_widget(tabs, area);
}

fn render_active_tab(frame: &mut Frame<'_>, area: Rect, state: &TuiState, view: &TuiViewModel) {
    let selected_tab = state.selected_tab();
    let rows = view.rows_for_tab(selected_tab);
    let selected_row = state.selected_row_for(selected_tab, rows.len());
    render_status_table(frame, area, selected_tab.title(), rows, selected_row);
}

fn render_status_table(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    rows: &[StatusRow],
    selected_row: usize,
) {
    let display_rows = if rows.is_empty() {
        vec![StatusRow::new("Status", "no runtime data", "empty")]
    } else {
        rows.to_vec()
    };

    let table_rows = display_rows.iter().enumerate().map(|(index, row)| {
        let style = if index == selected_row {
            status_style(&row.status).add_modifier(Modifier::REVERSED)
        } else {
            status_style(&row.status)
        };

        Row::new(vec![
            row.label.clone(),
            row.value.clone(),
            row.status.clone(),
        ])
        .style(style)
    });

    let table = Table::new(
        table_rows,
        [
            Constraint::Length(24),
            Constraint::Min(20),
            Constraint::Length(18),
        ],
    )
    .block(Block::default().borders(Borders::ALL).title(title))
    .column_spacing(1);

    frame.render_widget(table, area);
}

fn render_help(frame: &mut Frame<'_>, area: Rect) {
    let help = Paragraph::new(
        "Tabs: Left/Right/h/l | Rows: Up/Down/j/k | Demo: n tiny RFQ, o oversized, a accept, r rebalance, g gateway, x hedge | q quit",
    )
    .block(Block::default().borders(Borders::ALL).title("Controls"))
    .wrap(Wrap { trim: true })
    .style(Style::default().fg(Color::Gray));

    frame.render_widget(help, area);
}

fn vertical_chunks(area: Rect, heights: [u16; 4]) -> [Rect; 4] {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(heights[0]),
            Constraint::Min(heights[1]),
            Constraint::Length(heights[2]),
            Constraint::Length(heights[3]),
        ])
        .split(area);

    [chunks[0], chunks[1], chunks[2], chunks[3]]
}

fn status_style(status: &str) -> Style {
    let color = match status {
        "accepted" | "balanced" | "checked" | "clear" | "completed" | "enabled" | "executed"
        | "live" | "ready" | "refill_completed" | "utc" => Color::Green,
        "active" | "limit" | "pending" | "price" | "quoteable" | "quoted" | "requested"
        | "settling" | "snapshot" | "started" | "tx" | "usdc" => Color::Cyan,
        "disabled" | "empty" | "expired" | "failed" | "negative" | "rejected" | "threshold" => {
            Color::Red
        }
        "drift" | "refill_requested" | "required" => Color::Yellow,
        _ => Color::Gray,
    };

    Style::default().fg(color)
}

fn map_io<T>(result: io::Result<T>, context: &'static str) -> AppResult<T> {
    result.map_err(|error| AppError::internal(format!("{context}: {error}")))
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};
    use rust_decimal::Decimal;
    use time::OffsetDateTime;

    use super::*;
    use crate::{
        runtime::{
            GatewayProjection, InventoryProjection, PnlProjection, RebalanceProjection,
            RfqProjection, RiskProjection,
        },
        types::{AmountRaw, RuntimeRunId},
    };

    #[test]
    fn tui_render_does_not_panic_with_empty_runtime_state() {
        let backend = TestBackend::new(100, 32);
        let mut terminal = Terminal::new(backend).expect("test backend should initialize");
        let state = TuiState::default();
        let runtime = empty_runtime();

        terminal
            .draw(|frame| render(frame, frame.area(), &state, &runtime))
            .expect("test backend draw should succeed");
    }

    #[test]
    fn tui_key_mapping_supports_tab_navigation_and_quit() {
        assert_eq!(
            input_from_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
            Some(TuiInput::NextTab)
        );
        assert_eq!(
            input_from_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            Some(TuiInput::PreviousTab)
        );
        assert_eq!(
            input_from_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(TuiInput::Quit)
        );
        assert_eq!(
            input_from_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(TuiInput::Quit)
        );
    }

    #[test]
    fn tui_key_mapping_supports_scripted_demo_commands() {
        assert_eq!(
            input_from_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)),
            Some(TuiInput::Demo(ScriptedDemoAction::GenerateTinyRfq))
        );
        assert_eq!(
            input_from_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE)),
            Some(TuiInput::Demo(ScriptedDemoAction::GenerateOversizedRfq))
        );
        assert_eq!(
            input_from_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(TuiInput::Demo(ScriptedDemoAction::AcceptQuote))
        );
        assert_eq!(
            input_from_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
            Some(TuiInput::Demo(ScriptedDemoAction::TriggerRebalanceCheck))
        );
        assert_eq!(
            input_from_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)),
            Some(TuiInput::Demo(
                ScriptedDemoAction::TriggerGatewayRefillCheck
            ))
        );
        assert_eq!(
            input_from_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Some(TuiInput::Demo(
                ScriptedDemoAction::TriggerExposureHedgeCheck
            ))
        );
    }

    fn empty_runtime() -> RuntimeState {
        RuntimeState {
            run_id: RuntimeRunId::generate(),
            started_at: OffsetDateTime::now_utc(),
            inventory: InventoryProjection {
                balances: Vec::new(),
                quoteable_thresholds: Vec::new(),
                max_drift_bps: 0,
                status: "empty".to_owned(),
            },
            risk: RiskProjection {
                last_decision: None,
                active_rejections: Vec::new(),
                require_taker_allowlist: false,
                max_price_staleness_seconds: 20,
            },
            pnl: PnlProjection {
                realized_spread_usdc_estimate: Decimal::ZERO,
                fees_usdc_estimate: Decimal::ZERO,
                hedge_cost_usdc_estimate: Decimal::ZERO,
                rebalance_cost_usdc_estimate: Decimal::ZERO,
                net_usdc_estimate: Decimal::ZERO,
            },
            rfq: RfqProjection::default(),
            rebalance: RebalanceProjection::default(),
            gateway: GatewayProjection {
                enabled: false,
                usdc_refill_threshold_raw: AmountRaw::new(0),
                usdc_refill_target_raw: AmountRaw::new(0),
                status: "not_checked".to_owned(),
            },
            recent_events: Vec::new(),
        }
    }
}
