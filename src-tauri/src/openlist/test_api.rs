use tauri::AppHandle;
use tauri::Manager;
use tauri_plugin_http::reqwest;

use crate::openlist::crawler::Crawler;

/// 测试与下载站的连通性，返回站点公告等基础信息
#[tauri::command]
pub async fn test_openlist_connection(app: AppHandle) -> Result<String, String> {
  let client = app.state::<reqwest::Client>();
  let crawler = Crawler::new(client.inner());

  crawler.probe().await.map_err(|e| e.0)
}
