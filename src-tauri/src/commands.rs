use std::sync::Arc;

use tauri::State;

use crate::download::{DownloadManager, TaskView};

pub struct AppState {
    pub manager: Arc<DownloadManager>,
}

#[tauri::command]
pub async fn add_task(
    state: State<'_, AppState>,
    url: String,
    dir: Option<String>,
    connections: Option<usize>,
) -> Result<String, String> {
    state.manager.add(url, dir, connections)
}

#[tauri::command]
pub async fn pause_task(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.manager.pause(&id)
}

#[tauri::command]
pub async fn resume_task(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.manager.resume(&id)
}

#[tauri::command]
pub async fn cancel_task(
    state: State<'_, AppState>,
    id: String,
    delete_file: Option<bool>,
) -> Result<(), String> {
    state.manager.cancel(&id, delete_file.unwrap_or(false))
}

#[tauri::command]
pub async fn remove_task(
    state: State<'_, AppState>,
    id: String,
    delete_file: Option<bool>,
) -> Result<(), String> {
    state.manager.remove(&id, delete_file.unwrap_or(false))
}

#[tauri::command]
pub async fn list_tasks(state: State<'_, AppState>) -> Result<Vec<TaskView>, String> {
    Ok(state.manager.list())
}

#[tauri::command]
pub async fn default_download_dir(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.manager.default_dir())
}
