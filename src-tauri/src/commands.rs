use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::download::{DownloadManager, TaskView};
use crate::store::Store;

pub struct AppState {
    pub manager: Arc<DownloadManager>,
    /// 数据库不可用时为 None，此时下载功能照常，只是不保存任务
    pub store: Option<Arc<Store>>,
    /// 实际生效的窗口材质：vibrancy / mica / acrylic / blur / none
    pub material: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageInfo {
    pub available: bool,
    pub path: Option<String>,
    pub schema_version: Option<i64>,
    pub message: Option<String>,
}

#[tauri::command]
pub async fn storage_info(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<StorageInfo, String> {
    let path = app
        .path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join("conflux.db").to_string_lossy().to_string());

    match &state.store {
        Some(store) => Ok(StorageInfo {
            available: true,
            path,
            schema_version: Some(store.schema_version()?),
            message: None,
        }),
        None => Ok(StorageInfo {
            available: false,
            path,
            schema_version: None,
            message: Some("数据库不可用，本次运行不会保存任务".to_string()),
        }),
    }
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

/// 前端据此决定侧边栏是透明的（有原生材质）还是纯色的（退回）
#[tauri::command]
pub async fn window_material(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.material.clone())
}
