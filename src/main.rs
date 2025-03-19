use futures::future::join_all;
use reqwest::Client;
use scraper::{Html, Selector};
use std::ffi::{OsStr, OsString}; // 引入 OsString
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use structopt::StructOpt;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore; // 用于计时
mod md5_vec; // 引入 md5_vec 模块
use md5::{Digest, Md5}; // 引入 MD5 计算库

#[derive(StructOpt)]
struct Cli {
    /// 目标页面的URL（如果不加 -u 参数，则直接使用第一个参数作为目标 URL）
    #[structopt(short = "u", long = "url", parse(try_from_os_str = parse_url))]
    url: Option<String>,

    /// 代理前缀URL（默认为 https://proxy.acgh.top/proxy/）
    #[structopt(
        short = "p",
        long = "proxy-url",
        default_value = "https://proxy.acgh.top/proxy/"
    )]
    proxy_url: String,

    /// 命令行中未明确指定 -u 时的默认 URL 参数
    #[structopt(parse(try_from_os_str = parse_url))]
    default_url: Option<String>,
}

/// 自定义解析函数，将 OsStr 转换为 String
fn parse_url(value: &OsStr) -> Result<String, OsString> {
    value
        .to_str()
        .map(|s| s.to_string())
        .ok_or_else(|| OsString::from("无效的 URL 参数"))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 解析命令行参数
    let mut args = Cli::from_args();

    // 如果未提供 -u 参数，则将 default_url 赋值给 url
    if args.url.is_none() && args.default_url.is_some() {
        args.url = args.default_url.take();
    }

    // 检查是否提供了有效的 URL
    let target_url = match args.url {
        Some(url) => {
            if url.starts_with("https://telegra.ph") {
                format!("{}{}", args.proxy_url, url)
            } else {
                url
            }
        }
        None => {
            eprintln!("错误：未提供目标 URL，请使用 -u 或直接提供 URL 参数。");
            return Ok(());
        }
    };

    // 配置 HTTP 客户端，设置 User-Agent
    let client = Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/112.0.0.0 Safari/537.36")
        .build()?;

    // 处理目标页面，包括提取图片 URL 和下载图片
    process_page(
        &client,
        &target_url,
        &args.proxy_url,
        &md5_vec::KNOWN_MD5_HASHES,
    )
    .await?;

    Ok(())
}

/// 处理单个页面的逻辑
async fn process_page(
    client: &Client,     // HTTP 客户端
    url: &str,           // 页面的目标 URL
    proxy_url: &str,     // 代理前缀 URL
    md5_hashes: &[&str], // 提供的 MD5 值列表
) -> Result<(), Box<dyn std::error::Error>> {
    // 获取页面内容
    let response = client.get(url).send().await?;
    let body = response.text().await?;
    let document = Html::parse_document(&body); // 使用 scraper 解析 HTML 文档

    // 提取 <h1> 标题作为文件夹名称
    let h1_title = extract_h1_title(&document)?;

    // 创建以 <h1> 标题命名的文件夹
    let folder_path = create_folder(&h1_title)?;

    // 收集页面中所有图片的 URL，并通过代理前缀补全
    let image_urls = collect_image_urls(&document, proxy_url)?;

    println!("从页面 {} 收集到 {} 张图片", url, image_urls.len());

    // 开始计时，记录图片下载耗时
    let start_time = Instant::now();

    // 下载图片到指定文件夹
    download_images(client, &folder_path, image_urls, md5_hashes).await?;

    // 计算并打印图片下载总耗时
    let elapsed_time = start_time.elapsed();
    println!(
        "文件夹 '{}' 中的所有图片下载完成，耗时 {:.2} 秒",
        folder_path.display(),
        elapsed_time.as_secs_f64()
    );

    Ok(())
}

/// 提取页面中的 <h1> 标题
fn extract_h1_title(document: &Html) -> Result<String, Box<dyn std::error::Error>> {
    // 使用 CSS 选择器定位 <h1> 元素
    let h1_selector = Selector::parse("h1").unwrap();
    // 提取 <h1> 标题文本，去除多余空格和换行符
    let h1_title = document
        .select(&h1_selector)
        .next()
        .map(|element| element.text().collect::<String>().trim().to_string())
        .unwrap_or_else(|| "Untitled".to_string()); // 如果没有找到 <h1>，使用默认标题 "Untitled"

    Ok(h1_title)
}

/// 创建以标题命名的文件夹
fn create_folder(title: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    // 清理非法字符，确保文件夹名称合法
    let folder_name = sanitize_filename::sanitize(title);
    // 构造文件夹路径
    let folder_path = Path::new("./img").join(folder_name);
    // 如果文件夹不存在，则创建
    if !folder_path.exists() {
        std::fs::create_dir_all(&folder_path)?;
    }
    Ok(folder_path)
}

/// 收集页面中所有图片的 URL
/// 收集页面中所有图片的 URL
fn collect_image_urls(
    document: &Html, // 解析后的 HTML 文档
    proxy_url: &str, // 代理前缀 URL
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    // 使用 CSS 选择器定位所有 <img> 元素
    let img_selector = Selector::parse("img").unwrap();

    // 提取每个 <img> 元素的 src 属性，并通过代理前缀补全 URL
    let image_urls: Vec<_> = document
        .select(&img_selector)
        .filter_map(|element| element.value().attr("src")) // 提取 src 属性
        .map(|s| {
            // 如果图片 URL 已经包含代理前缀，则直接返回原始 URL
            if s.starts_with("/proxy/") {
                let s = s.trim_start_matches("/proxy/"); // 移除开头的 "/proxy"
                format!("{}{}", proxy_url, s)
            } else {
                // 否则补全代理前缀
                format!("{}{}", proxy_url, s)
            }
        })
        .collect();

    Ok(image_urls)
}

/// 下载图片到指定文件夹
async fn download_images(
    client: &Client,         // HTTP 客户端
    folder_path: &Path,      // 文件夹路径
    image_urls: Vec<String>, // 图片 URL 列表
    md5_hashes: &[&str],     // 提供的 MD5 值列表
) -> Result<(), Box<dyn std::error::Error>> {
    // 使用信号量限制并发数，防止过多请求导致被封禁
    let concurrency_limit = Arc::new(Semaphore::new(3));

    // 计算图片编号所需的最大位数（例如，32 张图片需要 2 位数）
    let max_digits = (image_urls.len() as f64).log10().ceil() as usize;

    // 为每个图片 URL 创建一个异步任务
    let tasks = image_urls
        .into_iter()
        .enumerate() // 添加索引，用于生成文件名
        .map(|(index, img_url)| {
            let client = client.clone();
            let concurrency_limit = Arc::clone(&concurrency_limit);
            let folder_path = folder_path.to_owned();
            let md5_hashes = md5_hashes.to_owned();

            async move {
                // 获取信号量许可，限制并发数
                let _permit = concurrency_limit.acquire().await.unwrap();

                // 重试机制：最多重试 3 次
                let mut retries = 3;
                let mut success = false;

                while retries > 0 {
                    // 下载图片
                    let response = match client.get(&img_url).send().await {
                        Ok(res) => res,
                        Err(_) => {
                            retries -= 1;
                            println!("下载失败，剩余重试次数: {}", retries);
                            continue;
                        }
                    };

                    // 获取图片字节数据
                    let bytes = match response.bytes().await {
                        Ok(bytes) => bytes,
                        Err(_) => {
                            retries -= 1;
                            println!("获取图片失败，剩余重试次数: {}", retries);
                            continue;
                        }
                    };

                    // 检查图片大小是否小于 1KB
                    if bytes.len() < 1024 {
                        retries -= 1;
                        println!(
                            "{} 图片大小小于 1KB，可能下载失败，剩余重试次数: {}",
                            img_url, retries
                        );
                        continue;
                    }

                    // 计算图片的 MD5 值
                    let mut hasher = Md5::new();
                    hasher.update(&bytes);
                    let result = hasher.finalize();
                    let md5_hash = format!("{:x}", result); // 将 MD5 值转换为十六进制字符串

                    // 打印图片的 MD5 值
                    println!("图片 {} 的 MD5 值为: {}", img_url, md5_hash);

                    // 检查 MD5 值是否在提供的列表中
                    if md5_hashes.contains(&md5_hash.as_str()) {
                        println!(
                            "图片 {} 的 MD5 值已存在 ({})，跳过下载。",
                            img_url, md5_hash
                        );
                        break; // 跳过该图片的下载
                    }

                    // 获取图片扩展名
                    let ext = get_extension(&bytes).unwrap_or("jpg");

                    // 补零处理图片编号
                    let file_index = format!("{:0width$}", index + 1, width = max_digits);

                    // 打印图片 URL 和大小
                    println!(
                        "正在下载图片: {}, 大小: {:.2} KB",
                        img_url,
                        bytes.len() as f64 / 1024.0
                    );

                    // 保存图片到文件
                    let file_name = format!("{}/{}.{}", folder_path.display(), file_index, ext);
                    let path = Path::new(&file_name);
                    let mut file = fs::File::create(path).await.expect("创建文件失败");
                    file.write_all(&bytes).await.expect("写入文件失败");

                    println!("下载完成: {}", path.display());
                    success = true;
                    break; // 成功下载，退出重试循环
                }

                if !success {
                    eprintln!("图片下载失败: {}", img_url);
                }
            }
        });

    // 并发执行所有任务
    join_all(tasks).await;

    Ok(())
}

/// 获取图片的扩展名
fn get_extension(bytes: &[u8]) -> Option<&'static str> {
    // 判断图片的文件头，确定其格式
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg") // JPEG 格式
    } else if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        Some("png") // PNG 格式
    } else if bytes.starts_with(&[0x47, 0x49, 0x46, 0x38]) {
        Some("gif") // GIF 格式
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp") // WebP 格式
    } else if bytes.starts_with(&[0x42, 0x4D]) {
        Some("bmp") // BMP 格式
    } else if bytes.starts_with(&[0x49, 0x49, 0x2A, 0x00])
        || bytes.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
    {
        Some("tiff") // TIFF 格式
    } else if bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        Some("ico") // ICO 格式
    } else if bytes.starts_with(b"<?xml") || bytes.starts_with(b"<svg") {
        Some("svg") // SVG 格式
    } else {
        None // 无法识别的格式
    }
}
