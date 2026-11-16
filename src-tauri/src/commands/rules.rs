use std::str::FromStr;

use tauri::State;

use crate::app_config::AppType;
use crate::commands::run_db_task;
use crate::rules::{app_support_list, DiscoverableRule, Rule, RuleAppSupport};
use crate::services::rules::{ImportOutcome, RuleService};
use crate::store::AppState;

#[tauri::command]
pub async fn get_rules(state: State<'_, AppState>) -> Result<Vec<Rule>, String> {
    run_db_task(&state, |state| {
        RuleService::list(state).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn create_rule(
    name: String,
    content: String,
    state: State<'_, AppState>,
) -> Result<Rule, String> {
    run_db_task(&state, move |state| {
        RuleService::create(state, name, content).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn update_rule(
    id: String,
    name: String,
    content: String,
    state: State<'_, AppState>,
) -> Result<Rule, String> {
    run_db_task(&state, move |state| {
        RuleService::update(state, &id, name, content).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn delete_rule(id: String, state: State<'_, AppState>) -> Result<(), String> {
    run_db_task(&state, move |state| {
        RuleService::delete(state, &id).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn toggle_rule_app(
    id: String,
    app: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<Rule, String> {
    let app_type = AppType::from_str(&app).map_err(|e| e.to_string())?;
    run_db_task(&state, move |state| {
        RuleService::toggle_app(state, &id, &app_type, enabled).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn sync_rules(state: State<'_, AppState>) -> Result<(), String> {
    run_db_task(&state, |state| {
        RuleService::sync_all(state).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn discover_rules(state: State<'_, AppState>) -> Result<Vec<DiscoverableRule>, String> {
    run_db_task(&state, |state| {
        RuleService::discover(state).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn import_rules(
    names: Vec<String>,
    state: State<'_, AppState>,
) -> Result<ImportOutcome, String> {
    run_db_task(&state, move |state| {
        RuleService::import(state, names).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn get_rule_app_support() -> Result<Vec<RuleAppSupport>, String> {
    Ok(app_support_list())
}
