/**
 * CAUCraft 下载站（OpenList）服务层
 *
 * 站点前端是一个 SPA，页面 HTML 中不含任何文件数据，目录内容全部由浏览器
 * 在运行时通过站点自身的 HTTP 接口拉取。因此这里统一走站点接口，并全部经由
 * Rust 后端代理 —— 既能规避浏览器 CORS / 混合内容限制，也便于统一处理
 * 站点地址与错误。
 */
import { invoke } from "@tauri-apps/api/core";
import type { FsListRequest, FsListResponse, FsObject } from "@/types/openlist";

/**
 * 下载站地址
 *
 * 仅用于展示与兜底；真正的请求地址由 Rust 后端解析
 * （后端会优先读取编译期注入的 `XMCL_OPENLIST_BASE_URL`，为空则回退内置默认值）。
 */
export const OPENLIST_BASE_URL =
  process.env.NEXT_PUBLIC_OPENLIST_BASE_URL || "http://download.cauc.fun";

/** 目录树遍历默认最大深度 */
export const DEFAULT_MAX_DEPTH = 6;
/** 全站搜索默认最多返回条数 */
export const DEFAULT_MAX_RESULTS = 300;

/**
 * 下载站接口客户端
 */
export class OpenListAPI {
  /**
   * 列出目录内容（通过 Rust 后端代理）
   */
  async list(request: FsListRequest): Promise<FsListResponse> {
    console.log("[download-site] 请求目录:", request);

    try {
      const response = await invoke<FsListResponse>("openlist_browse", {
        request: {
          path: request.path || "/",
          page: request.page || 1,
          perPage: request.per_page || 100,
        },
      });

      console.log(
        "[download-site] 返回 code:",
        response.code,
        "条目数:",
        response.data?.content?.length || 0
      );

      return response;
    } catch (error) {
      console.error("[download-site] 请求目录失败:", error);
      throw new Error(`下载站请求失败: ${error}`);
    }
  }

  /**
   * 浏览指定路径
   * @param path 路径
   * @param page 页码
   * @param perPage 每页数量
   */
  async browse(
    path: string = "/",
    page: number = 1,
    perPage: number = 100
  ): Promise<FsListResponse> {
    return this.list({ path, page, per_page: perPage });
  }

  /**
   * 在当前目录内按名称过滤（本地过滤，不产生额外请求）
   * @param basePath 基础路径
   * @param query 关键词
   */
  async search(basePath: string, query: string): Promise<FsObject[]> {
    const response = await this.browse(basePath);
    if (response.code !== 200 || !response.data) {
      return [];
    }
    const keyword = query.toLowerCase();
    return response.data.content.filter((item) =>
      item.name.toLowerCase().includes(keyword)
    );
  }

  /**
   * 全站搜索
   *
   * 由后端递归遍历目录树后按文件名匹配，因此可以命中子目录中的文件，
   * 不像目录内过滤那样只能看到当前层级。
   * @param keyword 关键词
   * @param root 搜索起始路径
   * @param maxDepth 最大遍历深度
   * @param maxResults 最多返回条数
   */
  async searchAll(
    keyword: string,
    root: string = "/",
    maxDepth: number = DEFAULT_MAX_DEPTH,
    maxResults: number = DEFAULT_MAX_RESULTS
  ): Promise<FsObject[]> {
    if (!keyword.trim()) {
      return [];
    }

    console.log(`[download-site] 全站搜索 "${keyword}"，起始于 ${root}`);

    const results = await invoke<FsObject[]>("openlist_crawl_search", {
      keyword,
      root,
      maxDepth,
      maxResults,
    });

    console.log(`[download-site] 全站搜索命中 ${results?.length || 0} 个文件`);
    return results || [];
  }

  /**
   * 递归遍历目录树，返回其中的所有文件
   * @param path 起始路径
   * @param maxDepth 最大遍历深度
   */
  async crawlTree(
    path: string = "/",
    maxDepth: number = DEFAULT_MAX_DEPTH
  ): Promise<FsObject[]> {
    const results = await invoke<FsObject[]>("openlist_crawl_tree", {
      path,
      maxDepth,
    });
    return results || [];
  }

  /**
   * 下载文件到本地（支持断点续传）
   * @param fileName 文件名
   * @param filePath 文件相对站点根目录的完整路径（如 `/XMCL/0.2.0/xxx.zip`）
   * @param sign 下载签名
   * @param size 文件大小
   * @returns 下载信息 JSON 字符串
   */
  async downloadFile(
    fileName: string,
    filePath: string,
    sign: string,
    size: number
  ): Promise<string> {
    console.log("[download-site] 开始下载文件:");
    console.log(`  - 名称: ${fileName}`);
    console.log(`  - 路径: ${filePath}`);
    console.log(`  - 大小: ${size}`);

    try {
      const downloadedPath = await invoke<string>("openlist_download_modpack", {
        fileName,
        filePath,
        sign,
        size,
      });

      console.log("[download-site] 下载任务已创建:", downloadedPath);
      return downloadedPath;
    } catch (error) {
      console.error("[download-site] 下载失败:", error);
      throw new Error(`下载失败: ${error}`);
    }
  }

  /**
   * 测试与下载站的连通性
   */
  async testConnection(): Promise<string> {
    return await invoke<string>("test_openlist_connection");
  }
}

// 导出单例实例
export const openlistAPI = new OpenListAPI();

// 导出默认实例
export default openlistAPI;
