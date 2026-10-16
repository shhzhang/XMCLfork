//! CAUCraft 下载站数据访问模块
//!
//! 目标站点 `http://download.cauc.fun/` 是一个 OpenList 实例（Alist 分支）。
//! 它的前端是纯 SPA，页面 HTML 中不包含任何文件数据，目录内容全部由浏览器
//! 在运行时通过站点自身的 HTTP 接口拉取。因此对本站而言，**可行且官方支持的
//! 数据获取方式就是直接请求它的 HTTP 接口**，无需（也无法）解析 HTML。
//!
//! 本模块把这些接口封装为可复用的抓取能力：
//!
//! * [`Crawler::fetch_dir`]      —— 抓取单层目录
//! * [`Crawler::fetch_tree`]     —— 递归遍历整棵目录树
//! * [`Crawler::search`]         —— 全站递归搜索
//! * [`Crawler::resolve_file`]   —— 解析文件元信息与下载直链
//! * [`Crawler::download_url`]   —— 构造稳定的下载链接（`/d/<path>?sign=<sign>`）
//!
//! 站点地址优先级：编译期 `.env` 的 `XMCL_OPENLIST_BASE_URL`（非空时）> 内置默认值。

use std::collections::VecDeque;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri_plugin_http::reqwest;

use crate::error::XMCLError as Error;

/// 内置默认站点地址（当环境变量未配置时使用）
pub const DEFAULT_BASE_URL: &str = "http://download.cauc.fun";

/// 编译期从 `.env` / `.env.template` 注入的站点地址，可能为空字符串
const COMPILED_BASE_URL: &str = match option_env!("XMCL_OPENLIST_BASE_URL") {
  Some(value) => value,
  None => "",
};

/// 单次目录抓取的最大条目数
pub const MAX_PER_PAGE: i32 = 500;

/// 目录树遍历的默认最大深度
pub const DEFAULT_MAX_DEPTH: usize = 6;

/// 全站搜索默认最多返回的条目数
pub const DEFAULT_MAX_RESULTS: usize = 300;

/// 单个请求的超时时间
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 解析最终生效的站点地址（去掉尾部斜杠）
pub fn base_url() -> String {
  let compiled = COMPILED_BASE_URL.trim();
  let raw = if compiled.is_empty() {
    DEFAULT_BASE_URL
  } else {
    compiled
  };
  raw.trim_end_matches('/').to_string()
}

/// 目录中的一条条目（文件或文件夹）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrawlEntry {
  /// 相对站点根目录的完整路径，例如 `/XMCL/0.2.0/xxx.zip`
  #[serde(default)]
  pub path: String,
  /// 名称
  #[serde(default)]
  pub name: String,
  /// 大小（字节），目录为 0
  #[serde(default)]
  pub size: u64,
  /// 是否为目录
  #[serde(default)]
  pub is_dir: bool,
  /// 最后修改时间
  #[serde(default)]
  pub modified: String,
  /// 创建时间
  #[serde(default)]
  pub created: String,
  /// 下载签名（文件有效，目录为空）
  #[serde(default)]
  pub sign: String,
  /// 缩略图
  #[serde(default)]
  pub thumb: String,
  /// 父目录路径
  #[serde(default)]
  pub parent: String,
  /// OpenList 文件类型：0 未知 / 1 文件夹 / 2 视频 / 3 音频 / 4 文本 / 5 图片
  #[serde(rename = "type", default)]
  pub file_type: u8,
}

/// 目录列表数据体（对外返回，`content` 中的条目已补全 `path` / `parent`）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrawlListData {
  #[serde(default)]
  pub content: Vec<CrawlEntry>,
  #[serde(default)]
  pub total: u64,
  #[serde(default)]
  pub readme: String,
  #[serde(default)]
  pub header: String,
  #[serde(default)]
  pub provider: String,
}

/// 接口原始返回的目录数据
///
/// `content` 保留为未加工的 JSON：接口只返回 `name`，不含 `path` / `parent`，
/// 直接反序列化成 [`CrawlEntry`] 会因为缺字段而失败，因此先取原始值再补全。
#[derive(Debug, Clone, Deserialize)]
struct RawListData {
  #[serde(default)]
  content: Vec<serde_json::Value>,
  #[serde(default)]
  total: u64,
  #[serde(default)]
  readme: String,
  #[serde(default)]
  header: String,
  #[serde(default)]
  provider: String,
}

/// 接口原始响应
#[derive(Debug, Clone, Deserialize)]
struct RawListResponse {
  code: i32,
  #[serde(default)]
  message: String,
  #[serde(default)]
  data: Option<RawListData>,
}

/// 统一的接口响应包装
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrawlListResponse {
  pub code: i32,
  pub message: String,
  pub data: Option<CrawlListData>,
}

/// 解析后的文件信息（含下载直链）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedFile {
  /// 文件路径
  pub path: String,
  /// 文件名
  pub name: String,
  /// 文件大小（字节）
  pub size: u64,
  /// 下载签名
  pub sign: String,
  /// 稳定的下载链接（`/d/<path>?sign=<sign>`），服务端会 302 到真实直链
  pub download_url: String,
  /// 存储提供的临时直链（有效期很短，仅作回退）
  pub raw_url: String,
}

/// 站点抓取客户端
///
/// 复用应用全局的 `reqwest::Client`（已带代理配置），因此不需要额外维护连接池。
pub struct Crawler<'a> {
  client: &'a reqwest::Client,
  base: String,
}

impl<'a> Crawler<'a> {
  /// 使用全局 HTTP 客户端创建抓取器
  pub fn new(client: &'a reqwest::Client) -> Self {
    Self {
      client,
      base: base_url(),
    }
  }

  /// 当前生效的站点地址
  pub fn base(&self) -> &str {
    &self.base
  }

  /// 调用 `/api/fs/list` 抓取单层目录
  pub async fn fetch_dir(
    &self,
    path: &str,
    page: i32,
    per_page: i32,
  ) -> Result<CrawlListResponse, Error> {
    let path = normalize_path(path);
    let per_page = per_page.clamp(1, MAX_PER_PAGE);

    let response = self
      .client
      .post(format!("{}/api/fs/list", self.base))
      .timeout(REQUEST_TIMEOUT)
      .json(&json!({
        "path": path,
        "page": page.max(1),
        "per_page": per_page,
      }))
      .send()
      .await
      .map_err(|e| Error(format!("下载站请求失败: {}", e)))?;

    let status = response.status();
    if !status.is_success() {
      return Err(Error(format!("下载站返回异常状态码: {}", status)));
    }

    let raw: RawListResponse = response
      .json()
      .await
      .map_err(|e| Error(format!("下载站响应解析失败: {}", e)))?;

    // 补全 `path` / `parent` 并推断文件类型：接口只返回 `name`，
    // 而前端需要完整的相对路径才能正确导航与下载。
    let data = raw.data.map(|d| CrawlListData {
      content: d
        .content
        .iter()
        .filter_map(|item| Self::normalize_entry(&path, item))
        .collect(),
      total: d.total,
      readme: d.readme,
      header: d.header,
      provider: d.provider,
    });

    Ok(CrawlListResponse {
      code: raw.code,
      message: raw.message,
      data,
    })
  }

  /// 根据 `name` / `is_dir` 推断 OpenList 的 type 值
  fn infer_type(name: &str, is_dir: bool) -> u8 {
    if is_dir {
      return 1;
    }
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
      "mp4" | "mkv" | "avi" | "mov" | "webm" | "flv" | "wmv" => 2,
      "mp3" | "flac" | "wav" | "aac" | "ogg" | "m4a" => 3,
      "txt" | "md" | "json" | "yml" | "yaml" | "toml" | "ini" | "log" | "cfg" | "conf" => 4,
      "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "svg" | "ico" => 5,
      _ => 0,
    }
  }

  /// 兼容处理：把接口返回的裸条目（可能缺少 path / type）补全为完整 [`CrawlEntry`]
  fn normalize_entry(parent: &str, value: &serde_json::Value) -> Option<CrawlEntry> {
    let name = value.get("name")?.as_str()?.to_string();
    let is_dir = value
      .get("is_dir")
      .and_then(|v| v.as_bool())
      .unwrap_or(false);
    let file_type = value
      .get("type")
      .and_then(|v| v.as_u64())
      .map(|v| v as u8)
      .unwrap_or_else(|| Self::infer_type(&name, is_dir));

    Some(CrawlEntry {
      path: join_path(parent, &name),
      name,
      size: value.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
      is_dir,
      modified: value
        .get("modified")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string(),
      created: value
        .get("created")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string(),
      sign: value
        .get("sign")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string(),
      thumb: value
        .get("thumb")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string(),
      parent: parent.to_string(),
      file_type,
    })
  }

  /// 递归遍历整棵目录树（广度优先，带深度与数量上限）
  ///
  /// 返回所有**文件**条目；目录条目本身不返回，避免调用方再过滤。
  pub async fn fetch_tree(&self, root: &str, max_depth: usize) -> Result<Vec<CrawlEntry>, Error> {
    let root = normalize_path(root);
    let max_depth = max_depth.max(1);

    let mut files: Vec<CrawlEntry> = Vec::new();
    // (路径, 当前深度)
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((root.clone(), 0));

    while let Some((dir, depth)) = queue.pop_front() {
      let response = match self.fetch_dir(&dir, 1, MAX_PER_PAGE).await {
        Ok(r) => r,
        // 单个目录失败不应中断整树遍历（可能是受限目录）
        Err(e) => {
          log::warn!("[crawler] 跳过目录 {} : {}", dir, e.0);
          continue;
        }
      };

      let content = match response.data {
        Some(data) => data.content,
        None => continue,
      };

      for entry in content {
        if entry.is_dir {
          if depth + 1 <= max_depth {
            queue.push_back((entry.path.clone(), depth + 1));
          }
        } else {
          files.push(entry);
        }
      }
    }

    Ok(files)
  }

  /// 全站递归搜索（按文件名匹配，不区分大小写）
  pub async fn search(
    &self,
    root: &str,
    keyword: &str,
    max_depth: usize,
    max_results: usize,
  ) -> Result<Vec<CrawlEntry>, Error> {
    let keyword = keyword.trim().to_lowercase();
    if keyword.is_empty() {
      return Ok(Vec::new());
    }

    let all = self.fetch_tree(root, max_depth).await?;
    let matched: Vec<CrawlEntry> = all
      .into_iter()
      .filter(|e| e.name.to_lowercase().contains(&keyword))
      .take(max_results.max(1))
      .collect();

    Ok(matched)
  }

  /// 调用 `/api/fs/get` 解析文件元信息与下载直链
  pub async fn resolve_file(&self, path: &str) -> Result<ResolvedFile, Error> {
    let path = normalize_path(path);

    let response = self
      .client
      .post(format!("{}/api/fs/get", self.base))
      .timeout(REQUEST_TIMEOUT)
      .json(&json!({ "path": path }))
      .send()
      .await
      .map_err(|e| Error(format!("获取文件信息失败: {}", e)))?;

    let status = response.status();
    if !status.is_success() {
      return Err(Error(format!("获取文件信息失败，状态码: {}", status)));
    }

    let result: serde_json::Value = response
      .json()
      .await
      .map_err(|e| Error(format!("文件信息解析失败: {}", e)))?;

    let code = result.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code != 200 {
      let message = result
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("未知错误");
      return Err(Error(format!("获取文件信息失败: {} ({})", message, code)));
    }

    let data = result
      .get("data")
      .ok_or_else(|| Error("文件信息缺少 data 字段".to_string()))?;

    if data
      .get("is_dir")
      .and_then(|v| v.as_bool())
      .unwrap_or(false)
    {
      return Err(Error(format!("{} 是一个目录，无法直接下载", path)));
    }

    let name = data
      .get("name")
      .and_then(|v| v.as_str())
      .unwrap_or_else(|| path.rsplit('/').next().unwrap_or("download"))
      .to_string();
    let sign = data
      .get("sign")
      .and_then(|v| v.as_str())
      .unwrap_or_default()
      .to_string();
    let raw_url = data
      .get("raw_url")
      .and_then(|v| v.as_str())
      .unwrap_or_default()
      .to_string();

    Ok(ResolvedFile {
      download_url: self.download_url(&path, &sign),
      path,
      name,
      size: data.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
      sign,
      raw_url,
    })
  }

  /// 构造稳定的下载链接
  ///
  /// `/d/<path>?sign=<sign>` 是 OpenList 的官方下载入口：`sign` 由站点签发且长期有效，
  /// 每次请求时服务端会现场生成新的存储直链并 302 跳转，因此比直接用 `raw_url`
  /// （通常只有几分钟有效期）可靠得多。无 `sign` 时退回普通直链。
  pub fn download_url(&self, path: &str, sign: &str) -> String {
    let path = normalize_path(path);
    if sign.trim().is_empty() {
      format!("{}/d{}", self.base, path)
    } else {
      format!("{}/d{}?sign={}", self.base, path, urlencoding(sign))
    }
  }

  /// 测试与站点的连通性，返回站点公告等基础信息
  pub async fn probe(&self) -> Result<String, Error> {
    let response = self
      .client
      .get(format!("{}/api/public/settings", self.base))
      .timeout(REQUEST_TIMEOUT)
      .send()
      .await
      .map_err(|e| Error(format!("连接下载站失败: {}", e)))?;

    if !response.status().is_success() {
      return Err(Error(format!(
        "下载站返回异常状态码: {}",
        response.status()
      )));
    }

    let result: serde_json::Value = response
      .json()
      .await
      .map_err(|e| Error(format!("站点信息解析失败: {}", e)))?;

    let announcement = result
      .get("data")
      .and_then(|d| d.get("announcement"))
      .and_then(|v| v.as_str())
      .unwrap_or("");

    Ok(format!(
      "站点: {}\n公告: {}",
      self.base,
      if announcement.is_empty() {
        "(无)"
      } else {
        announcement
      }
    ))
  }
}

/// 归一化路径：确保以 `/` 开头，去掉多余尾部斜杠
pub fn normalize_path(path: &str) -> String {
  let trimmed = path.trim();
  if trimmed.is_empty() || trimmed == "/" {
    return "/".to_string();
  }
  let with_leading = if trimmed.starts_with('/') {
    trimmed.to_string()
  } else {
    format!("/{}", trimmed)
  };
  with_leading.trim_end_matches('/').to_string()
}

/// 拼接父路径与名称
pub fn join_path(parent: &str, name: &str) -> String {
  let parent = normalize_path(parent);
  if parent == "/" {
    format!("/{}", name)
  } else {
    format!("{}/{}", parent, name)
  }
}

/// 最小化的百分号编码（用于 query 参数值）
fn urlencoding(value: &str) -> String {
  let mut out = String::with_capacity(value.len());
  for byte in value.as_bytes() {
    match byte {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
        out.push(*byte as char)
      }
      _ => out.push_str(&format!("%{:02X}", byte)),
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn normalize_path_works() {
    assert_eq!(normalize_path(""), "/");
    assert_eq!(normalize_path("/"), "/");
    assert_eq!(normalize_path("XMCL/0.2.0"), "/XMCL/0.2.0");
    assert_eq!(normalize_path("/XMCL/0.2.0/"), "/XMCL/0.2.0");
  }

  #[test]
  fn join_path_works() {
    assert_eq!(join_path("/", "XMCL"), "/XMCL");
    assert_eq!(join_path("/XMCL", "0.2.0"), "/XMCL/0.2.0");
  }

  #[test]
  fn urlencoding_escapes_sign() {
    assert_eq!(urlencoding("a=b:c/d"), "a%3Db%3Ac%2Fd");
    assert_eq!(urlencoding("abc-_.~"), "abc-_.~");
  }
}
