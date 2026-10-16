//! 下载站（OpenList）相关的 Tauri 命令
//!
//! 所有命令都通过 [`crawler`](super::crawler) 访问站点，不再直接拼接接口地址，
//! 站点地址也改为运行时解析（见 [`crawler::base_url`]），这样即使 `.env`
//! 里留空也能回退到内置的默认站点。

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tauri_plugin_http::reqwest;

use crate::{
  error::XMCLError as Error,
  launcher_config::models::LauncherConfig,
  openlist::crawler::{
    CrawlEntry, CrawlListResponse, Crawler, DEFAULT_MAX_DEPTH, DEFAULT_MAX_RESULTS,
  },
  tasks::{commands::schedule_progressive_task_group, download::DownloadParam, PTaskParam},
};

/// 目录浏览请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowseRequest {
  /// 目录路径，`/` 为站点根目录
  pub path: String,
  /// 页码（从 1 开始）
  pub page: Option<i32>,
  /// 每页数量
  #[serde(rename = "perPage")]
  pub per_page: Option<i32>,
}

/// 抓取下载站目录（单层）
#[tauri::command]
pub async fn openlist_browse(
  app: AppHandle,
  request: BrowseRequest,
) -> Result<CrawlListResponse, Error> {
  let client = app.state::<reqwest::Client>();
  let crawler = Crawler::new(client.inner());

  log::info!(
    "[openlist] 抓取目录 {} (站点: {})",
    request.path,
    crawler.base()
  );

  let response = crawler
    .fetch_dir(
      &request.path,
      request.page.unwrap_or(1),
      request.per_page.unwrap_or(100),
    )
    .await?;

  log::info!(
    "[openlist] 目录 {} 返回 code={}, 条目数={}",
    request.path,
    response.code,
    response.data.as_ref().map(|d| d.content.len()).unwrap_or(0)
  );

  Ok(response)
}

/// 递归遍历下载站目录树，返回其中的所有文件
///
/// 用于统计站点内容或为全站搜索预热缓存。
#[tauri::command]
pub async fn openlist_crawl_tree(
  app: AppHandle,
  path: Option<String>,
  max_depth: Option<usize>,
) -> Result<Vec<CrawlEntry>, Error> {
  let client = app.state::<reqwest::Client>();
  let crawler = Crawler::new(client.inner());

  let root = path.unwrap_or_else(|| "/".to_string());
  let depth = max_depth.unwrap_or(DEFAULT_MAX_DEPTH);

  log::info!("[openlist] 开始遍历目录树 {} (最大深度 {})", root, depth);

  let files = crawler.fetch_tree(&root, depth).await?;

  log::info!(
    "[openlist] 目录树 {} 遍历完成，共 {} 个文件",
    root,
    files.len()
  );

  Ok(files)
}

/// 全站搜索：递归遍历目录树后按文件名匹配（不区分大小写）
#[tauri::command]
pub async fn openlist_crawl_search(
  app: AppHandle,
  keyword: String,
  root: Option<String>,
  max_depth: Option<usize>,
  max_results: Option<usize>,
) -> Result<Vec<CrawlEntry>, Error> {
  let client = app.state::<reqwest::Client>();
  let crawler = Crawler::new(client.inner());

  let root = root.unwrap_or_else(|| "/".to_string());
  let results = crawler
    .search(
      &root,
      &keyword,
      max_depth.unwrap_or(DEFAULT_MAX_DEPTH),
      max_results.unwrap_or(DEFAULT_MAX_RESULTS),
    )
    .await?;

  log::info!(
    "[openlist] 全站搜索 \"{}\" 命中 {} 个文件",
    keyword,
    results.len()
  );

  Ok(results)
}

/// 下载下载站中的文件
///
/// 完成后返回 JSON 字符串：`{ "path": <本地文件路径>, "taskGroupPrefix": <任务组前缀或 null> }`。
/// `taskGroupPrefix` 为 `null` 表示文件已存在、无需下载。
#[tauri::command]
pub async fn openlist_download_modpack(
  file_name: String,
  file_path: String,
  sign: String,
  size: u64,
  app: AppHandle,
  launcher_config: tauri::State<'_, std::sync::Mutex<LauncherConfig>>,
) -> Result<String, Error> {
  let client = app.state::<reqwest::Client>();
  let crawler = Crawler::new(client.inner());

  log::info!("[openlist] 开始下载:");
  log::info!("  - 文件名: {}", file_name);
  log::info!("  - 远程路径: {}", file_path);
  log::info!("  - 调用方给定大小: {} 字节", size);

  // 解析远端文件信息（同时拿到最新的 sign 与真实直链）
  let resolved = crawler.resolve_file(&file_path).await?;
  let remote_size = if resolved.size > 0 {
    resolved.size
  } else {
    size
  };

  // 选择下载地址：
  // 1) `/d/<path>?sign=<sign>` —— sign 由站点签发且长期有效，服务端会 302 到新的存储直链，最稳
  // 2) 调用方传入的 sign 同理
  // 3) `raw_url` —— 存储临时直链，有效期通常只有几分钟，仅作最后回退
  let url = if !resolved.sign.is_empty() {
    resolved.download_url.clone()
  } else if !sign.trim().is_empty() {
    crawler.download_url(&file_path, &sign)
  } else if !resolved.raw_url.is_empty() {
    resolved.raw_url.clone()
  } else {
    return Err(Error(format!("无法为 {} 解析下载链接", file_path)));
  };

  // 下载缓存目录（使用用户配置的下载目录）
  let download_cache_dir = {
    let config = launcher_config.lock().unwrap();
    config.download.cache.directory.clone()
  };

  let temp_dir = download_cache_dir.join("openlist_downloads");
  tokio::fs::create_dir_all(&temp_dir).await?;

  let dest_file = temp_dir.join(&file_name);
  log::info!("  - 本地路径: {}", dest_file.display());
  log::info!("  - 下载地址: {}", url);

  // 若本地已存在且大小一致，直接复用
  if dest_file.exists() {
    if let Ok(metadata) = tokio::fs::metadata(&dest_file).await {
      if remote_size > 0 && metadata.len() == remote_size {
        log::info!("[openlist] 本地文件已存在且大小一致，跳过下载");
        let result = serde_json::json!({
          "path": dest_file.to_string_lossy().to_string(),
          "taskGroupPrefix": serde_json::Value::Null,
        });
        return serde_json::to_string(&result)
          .map_err(|e| Error(format!("序列化 JSON 失败: {}", e)));
      }
    }
  }

  // 交给统一的下载任务系统（支持进度上报、暂停、续传）
  let download_param = DownloadParam {
    src: url
      .parse()
      .map_err(|e| Error(format!("无效的下载 URL: {}", e)))?,
    dest: dest_file.clone(),
    filename: Some(file_name.clone()),
    sha1: None, // 下载站未提供 SHA1，跳过校验
  };

  let task_group_prefix = format!("openlist:{}", file_name);

  let task_group_desc = schedule_progressive_task_group(
    app.clone(),
    task_group_prefix.clone(),
    vec![PTaskParam::Download(download_param)],
    true,
  )
  .await
  .map_err(|e| Error(format!("创建下载任务失败: {:?}", e)))?;

  log::info!(
    "[openlist] 下载任务已创建: task_group={}",
    task_group_desc.task_group
  );

  let result = serde_json::json!({
    "path": dest_file.to_string_lossy().to_string(),
    "taskGroupPrefix": task_group_prefix,
  });

  serde_json::to_string(&result).map_err(|e| Error(format!("序列化 JSON 失败: {}", e)))
}
